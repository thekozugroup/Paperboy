use crate::{process, resend::bounded_response};
use anyhow::{Result, anyhow, bail};
use ipp::{
    parser::IppParser,
    prelude::{DelimiterTag, IppAttribute, IppRequestResponse, IppValue, IppVersion, Operation},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    net::IpAddr,
    path::Path,
    time::{Duration, Instant},
};
use tokio::net::{TcpStream, lookup_host};
use url::Url;

pub fn local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_private() && !v.is_loopback(),
        IpAddr::V6(v) => v.is_unique_local() || v.is_unicast_link_local(),
    }
}
pub async fn validate_uri(value: &str) -> Result<String> {
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("Enter a valid printer address.");
    }
    let mut url =
        Url::parse(value.trim()).map_err(|_| anyhow!("Enter a valid printer address."))?;
    if !["ipp", "ipps"].contains(&url.scheme())
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port().is_some_and(|p| ![631, 443].contains(&p))
    {
        bail!("Use a local IPP address, such as ipp://192.168.1.50:631/ipp/print.");
    }
    let host = url.host_str().unwrap().trim_matches(['[', ']']);
    let port = url.port().unwrap_or(631);
    let addresses: Vec<_> = tokio::time::timeout(Duration::from_secs(3), lookup_host((host, port)))
        .await
        .map_err(|_| anyhow!("The printer address could not be found."))?
        .map_err(|_| anyhow!("The printer address could not be found. Check its IP address."))?
        .collect();
    if addresses.is_empty() || addresses.iter().any(|a| !local_ip(a.ip())) {
        bail!("Choose a printer on your local network.");
    }
    url.set_ip_host(addresses[0].ip())
        .map_err(|_| anyhow!("Invalid printer address."))?;
    url.set_port(Some(port))
        .map_err(|_| anyhow!("Invalid printer port."))?;
    if url.path().is_empty() || url.path() == "/" {
        url.set_path("/ipp/print");
    }
    Ok(url.to_string())
}
async fn reachable(uri: &str, seconds: u64) -> Result<()> {
    let uri = Url::parse(uri)?;
    tokio::time::timeout(
        Duration::from_secs(seconds),
        TcpStream::connect((
            uri.host_str().unwrap_or("").trim_matches(['[', ']']),
            uri.port().unwrap_or(631),
        )),
    )
    .await??;
    Ok(())
}
fn queue(printer: &Value) -> Result<&str> {
    let queue = printer["queue"]
        .as_str()
        .ok_or_else(|| anyhow!("Choose a printer first."))?;
    let valid = queue
        .strip_prefix("paperboy_")
        .is_some_and(|s| s.len() == 12 && s.bytes().all(|b| b.is_ascii_hexdigit()));
    if !valid {
        bail!("Invalid saved printer queue.");
    }
    Ok(queue)
}
async fn cups(
    request: IppRequestResponse,
    path: &str,
    payload: Option<&Path>,
) -> Result<IppRequestResponse> {
    let client = reqwest::Client::builder()
        .unix_socket("/run/cups/cups.sock")
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()?;
    let mut bytes = request.to_bytes().to_vec();
    if let Some(path) = payload {
        let content = tokio::fs::read(path).await?;
        if content.len() > 100 * 1024 * 1024 {
            bail!("The print document is too large.");
        }
        bytes.extend(content);
    }
    let mut response = client
        .post(format!("http://localhost{path}"))
        .header("Content-Type", "application/ipp")
        .body(bytes)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("The print service is unavailable.");
    }
    let bytes = bounded_response(&mut response, 4 * 1024 * 1024).await?;
    let result = IppParser::new(std::io::Cursor::new(bytes)).parse()?;
    if !result.header().status_code().is_success() {
        bail!("The print service did not accept the request.");
    }
    Ok(result)
}
fn number(response: &IppRequestResponse, name: &str) -> Option<i32> {
    response
        .attributes()
        .groups()
        .iter()
        .find_map(|group| group.get(name))
        .and_then(|a| match a.value() {
            IppValue::Enum(v) | IppValue::Integer(v) => Some(*v),
            _ => None,
        })
}
fn attribute(
    request: &mut IppRequestResponse,
    tag: DelimiterTag,
    name: &str,
    value: IppValue,
) -> Result<()> {
    request
        .attributes_mut()
        .add(tag, IppAttribute::new(name.try_into()?, value));
    Ok(())
}
async fn printer_state(printer: &Value) -> Result<i32> {
    let queue = queue(printer)?;
    let request = IppRequestResponse::new(
        IppVersion::v1_1(),
        Operation::GetPrinterAttributes,
        Some(format!("ipp://localhost/printers/{queue}").parse()?),
    )?;
    number(
        &cups(request, &format!("/printers/{queue}"), None).await?,
        "printer-state",
    )
    .ok_or_else(|| anyhow!("The printer state is unavailable."))
}
pub async fn pair(name: &str, uri: &str) -> Result<Value> {
    let uri = validate_uri(uri).await?;
    reachable(&uri, 3)
        .await
        .map_err(|_| anyhow!("The printer did not respond. Check its IP address and power."))?;
    let queue = format!(
        "paperboy_{}",
        &hex::encode(Sha256::digest(uri.as_bytes()))[..12]
    );
    process::run(
        "lpadmin",
        &[
            "-p".into(),
            queue.clone(),
            "-E".into(),
            "-v".into(),
            uri.clone(),
            "-m".into(),
            "everywhere".into(),
            "-D".into(),
            name.into(),
            "-o".into(),
            "printer-error-policy=retry-job".into(),
        ],
        25,
    )
    .await
    .map_err(|_| {
        anyhow!(
            "The printer did not respond. Check the address and that it supports AirPrint or IPP."
        )
    })?;
    let printer = json!({"name":name,"uri":uri,"queue":queue});
    if printer_state(&printer).await? == 5 {
        bail!("The printer queue is stopped. Check the printer and pair it again.");
    }
    Ok(printer)
}
pub async fn status(printer: &Value) -> Value {
    if printer.is_null() {
        return json!({"state":"unpaired","message":"Choose a printer"});
    }
    let result=async {let state=printer_state(printer).await?;if state==5 {return Ok::<_,anyhow::Error>(json!({"state":"offline","message":"Printer needs attention"}));}reachable(printer["uri"].as_str().unwrap_or(""),2).await?;Ok(json!({"state":if state==4 {"printing"}else{"ready"},"message":if state==4{"Printing"}else{"Ready to print"}}))}.await;
    result.unwrap_or_else(|_| json!({"state":"offline","message":"Printer unavailable"}))
}
pub async fn submit(printer: &Value, path: &Path, job_id: &str, settings: &Value) -> Result<i32> {
    let queue = queue(printer)?;
    let mut request = IppRequestResponse::new(
        IppVersion::v1_1(),
        Operation::PrintJob,
        Some(format!("ipp://localhost/printers/{queue}").parse()?),
    )?;
    attribute(
        &mut request,
        DelimiterTag::OperationAttributes,
        "requesting-user-name",
        IppValue::NameWithoutLanguage("paperboy".try_into()?),
    )?;
    attribute(
        &mut request,
        DelimiterTag::OperationAttributes,
        "job-name",
        IppValue::NameWithoutLanguage(format!("Paperboy {job_id}").try_into()?),
    )?;
    attribute(
        &mut request,
        DelimiterTag::OperationAttributes,
        "document-format",
        IppValue::MimeMediaType("application/pdf".try_into()?),
    )?;
    for (name, value) in [
        (
            "media",
            if settings["paper"] == "A4" {
                "iso_a4_210x297mm"
            } else {
                "na_letter_8.5x11in"
            },
        ),
        (
            "print-color-mode",
            settings["color"].as_str().unwrap_or("monochrome"),
        ),
        ("sides", settings["sides"].as_str().unwrap_or("one-sided")),
    ] {
        attribute(
            &mut request,
            DelimiterTag::JobAttributes,
            name,
            IppValue::Keyword(value.try_into()?),
        )?;
    }
    attribute(
        &mut request,
        DelimiterTag::JobAttributes,
        "copies",
        IppValue::Integer(1),
    )?;
    let response = cups(request, &format!("/printers/{queue}"), Some(path)).await?;
    number(&response, "job-id")
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            anyhow!("Delivery could not be confirmed. Check the printer before sending again.")
        })
}
pub async fn job_state(id: i32, printer_queue: &str) -> Option<i32> {
    let result = async {
        let mut request = IppRequestResponse::new(
            IppVersion::v1_1(),
            Operation::GetJobAttributes,
            Some(format!("ipp://localhost/printers/{printer_queue}").parse()?),
        )?;
        attribute(
            &mut request,
            DelimiterTag::OperationAttributes,
            "job-id",
            IppValue::Integer(id),
        )?;
        Ok::<_, anyhow::Error>(number(
            &cups(request, &format!("/jobs/{id}"), None).await?,
            "job-state",
        ))
    }
    .await;
    result.ok().flatten()
}
pub async fn cancel(id: i32, printer_queue: &str) -> Result<()> {
    let mut request = IppRequestResponse::new(
        IppVersion::v1_1(),
        Operation::CancelJob,
        Some(format!("ipp://localhost/printers/{printer_queue}").parse()?),
    )?;
    attribute(
        &mut request,
        DelimiterTag::OperationAttributes,
        "job-id",
        IppValue::Integer(id),
    )?;
    attribute(
        &mut request,
        DelimiterTag::OperationAttributes,
        "requesting-user-name",
        IppValue::NameWithoutLanguage("paperboy".try_into()?),
    )?;
    cups(request, &format!("/jobs/{id}"), None).await?;
    Ok(())
}
pub async fn discover() -> Result<Vec<Value>> {
    tokio::task::spawn_blocking(|| {
        use mdns_sd::{ServiceDaemon,ServiceEvent};
        let daemon=ServiceDaemon::new()?;
        let plain=daemon.browse("_ipp._tcp.local.")?;
        let secure=daemon.browse("_ipps._tcp.local.")?;
        let start=Instant::now();let mut found=BTreeMap::new();
        while start.elapsed()<Duration::from_secs(3) {
            for (receiver,scheme) in [(&plain,"ipp"),(&secure,"ipps")] {
                if let Ok(ServiceEvent::ServiceResolved(info))=receiver.recv_timeout(Duration::from_millis(50))
                    && let Some(ip)=info.addresses.iter().map(|ip|ip.to_ip_addr()).find(|ip|ip.is_ipv4() && local_ip(*ip)) {
                        let resource=info.get_property_val_str("rp").unwrap_or("ipp/print").trim_start_matches('/');
                        let uri=format!("{scheme}://{ip}:{}/{resource}",info.port);
                        let label=info.get_property_val_str("ty").unwrap_or(&info.fullname).chars().take(100).collect::<String>();
                        found.insert(uri.clone(),json!({"name":label,"uri":uri,"location":info.get_property_val_str("note").unwrap_or("On your network")}));
                }
            }
        }
        let _=daemon.shutdown();Ok::<_,anyhow::Error>(found.into_values().collect())
    }).await?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn non_lan_printer_addresses_are_rejected() {
        for uri in [
            "http://192.168.1.20/",
            "ipp://127.0.0.1:631/print",
            "ipp://8.8.8.8:631/print",
            "ipp://169.254.169.254:631/print",
            "file:///etc/passwd",
            "ipp://user:pass@192.168.1.20:631/print",
            "ipp://192.168.1.20:22/print",
            "ipp://192.168.1.20:631/print\n--bad",
        ] {
            assert!(validate_uri(uri).await.is_err(), "{uri}");
        }
        assert_eq!(
            validate_uri("ipp://192.168.1.20:631/ipp/print")
                .await
                .unwrap(),
            "ipp://192.168.1.20:631/ipp/print"
        );
    }
    #[test]
    fn validates_saved_queue_before_use() {
        assert!(queue(&json!({"queue":"../other"})).is_err());
        assert!(queue(&json!({"queue":"paperboy_012345abcdef"})).is_ok());
    }
}
