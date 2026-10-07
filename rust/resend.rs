use anyhow::{Result, anyhow, bail};
use reqwest::{Client, redirect::Policy};
use serde_json::Value;
use std::{
    net::{IpAddr, SocketAddr},
    path::Path,
    time::{Duration, Instant},
};
use tokio::io::AsyncWriteExt;
use url::Url;

pub fn authenticated(email: &Value) -> bool {
    let auth = &email["authentication"];
    auth["dmarc"] == "pass" || (auth["dmarc"] == "gray" && auth["dkim"] == "pass")
}
pub fn address(value: &str) -> String {
    let candidate = if let Some((_, tail)) = value.rsplit_once('<') {
        tail.strip_suffix('>').unwrap_or("")
    } else {
        value
    };
    let candidate = candidate.trim().to_lowercase();
    if candidate.contains(',')
        || candidate.contains(';')
        || candidate.chars().any(char::is_whitespace)
    {
        return String::new();
    }
    candidate
}
pub fn trusted_url(value: &str) -> Result<Url> {
    let url = Url::parse(value)
        .map_err(|_| anyhow!("Resend provided an unexpected attachment address."))?;
    let host = url.host_str().unwrap_or("").to_lowercase();
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
        || !(host == "resend.com"
            || host.ends_with(".resend.com")
            || host.ends_with(".cloudfront.net"))
    {
        bail!("Resend provided an unexpected attachment address.");
    }
    Ok(url)
}
pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let [a, b, c, _] = v.octets();
            !v.is_private()
                && !v.is_loopback()
                && !v.is_link_local()
                && !v.is_multicast()
                && !v.is_broadcast()
                && !v.is_unspecified()
                && !v.is_documentation()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 198 && [18, 19].contains(&b))
                && !(a == 192 && b == 0 && c == 0)
        }
        IpAddr::V6(v) => {
            let segments = v.segments();
            segments[0] & 0xe000 == 0x2000
                && !(segments[0] == 0x2001 && (segments[1] == 0x0db8 || segments[1] < 0x0200))
        }
    }
}

pub struct Resend {
    client: Client,
    last: Instant,
    base: String,
    interval: Duration,
}
impl Resend {
    pub fn new(key: &str) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .no_proxy()
            .redirect(Policy::none())
            .user_agent("Paperboy/0.2")
            .default_headers({
                let mut h = reqwest::header::HeaderMap::new();
                h.insert(
                    reqwest::header::AUTHORIZATION,
                    format!("Bearer {key}")
                        .parse()
                        .map_err(|_| anyhow!("Invalid saved API key."))?,
                );
                h
            })
            .build()?;
        Ok(Self {
            client,
            last: Instant::now() - Duration::from_secs(1),
            base: "https://api.resend.com".into(),
            interval: Duration::from_millis(600),
        })
    }
    async fn get(&mut self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        if let Some(delay) = self.interval.checked_sub(self.last.elapsed()) {
            tokio::time::sleep(delay).await;
        }
        self.last = Instant::now();
        let mut response = self
            .client
            .get(format!("{}{path}", self.base))
            .query(query)
            .send()
            .await
            .map_err(|_| anyhow!("Resend could not be reached. Check your internet connection."))?;
        match response.status().as_u16() {
            401 | 403 => bail!("Resend rejected the API key. Use a key with full access."),
            429 => bail!("Resend is busy. Paperboy will check again shortly."),
            200 => {}
            _ => bail!("Resend returned an error. Try again shortly."),
        }
        let bytes = bounded_response(&mut response, 4 * 1024 * 1024).await?;
        serde_json::from_slice(&bytes)
            .map_err(|_| anyhow!("Resend returned unexpected email data."))
    }
    pub async fn inbox(&mut self, after: Option<&str>) -> Result<Value> {
        let mut params = vec![("limit", "100".into())];
        if let Some(after) = after {
            params.push(("after", after.into()));
        }
        self.get("/emails/receiving", &params).await
    }
    #[cfg(test)]
    pub(crate) fn fixture(base: String) -> Self {
        Self {
            client: Client::builder()
                .no_proxy()
                .redirect(Policy::none())
                .build()
                .unwrap(),
            last: Instant::now(),
            base,
            interval: Duration::ZERO,
        }
    }
    pub async fn email(&mut self, id: &str) -> Result<Value> {
        self.get(
            &format!("/emails/receiving/{}", encoded(id)),
            &[("html_format", "cid".into())],
        )
        .await
    }
    pub async fn attachments(&mut self, id: &str) -> Result<Vec<Value>> {
        let mut items = vec![];
        let mut after: Option<String> = None;
        for _ in 0..5 {
            let mut params = vec![("limit", "100".into())];
            if let Some(ref after) = after {
                params.push(("after", after.clone()));
            }
            let response = self
                .get(
                    &format!("/emails/receiving/{}/attachments", encoded(id)),
                    &params,
                )
                .await?;
            let data = response["data"]
                .as_array()
                .ok_or_else(|| anyhow!("Resend returned unexpected attachment data."))?;
            items.extend(data.iter().cloned());
            if response["has_more"] != true {
                return Ok(items);
            }
            after = data
                .last()
                .and_then(|v| v["id"].as_str())
                .map(str::to_owned);
            if after.is_none() {
                break;
            }
        }
        bail!("Too many attachments in this message.")
    }
}
fn encoded(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}
pub async fn bounded_response(response: &mut reqwest::Response, max: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max as u64)
    {
        bail!("The response exceeds its size limit.");
    }
    let mut result = vec![];
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("The download was interrupted. Try again."))?
    {
        if result.len() + chunk.len() > max {
            bail!("The response exceeds its size limit.");
        }
        result.extend_from_slice(&chunk);
    }
    Ok(result)
}
pub async fn download(value: &str, path: &Path, max: usize) -> Result<()> {
    let url = trusted_url(value)?;
    let host = url.host_str().unwrap();
    let addresses: Vec<SocketAddr> = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::lookup_host((host, 443)),
    )
    .await
    .map_err(|_| anyhow!("The attachment server could not be reached."))?
    .map_err(|_| anyhow!("The attachment server could not be reached."))?
    .collect();
    if addresses.is_empty() || addresses.iter().any(|a| !public_ip(a.ip())) {
        bail!("The attachment address is not a public server.");
    }
    // Resolve once and pin that result. No API credential, proxies, or redirects enter this client.
    let client = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(60))
        .resolve_to_addrs(host, &addresses)
        .build()?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|_| anyhow!("The attachment could not be downloaded. Try again."))?;
    if response.status() != 200 {
        bail!("The attachment could not be downloaded. Try again.");
    }
    if response
        .content_length()
        .is_some_and(|length| length > max as u64)
    {
        bail!("This attachment exceeds your file size limit.");
    }
    let mut file = tokio::fs::File::create(path).await?;
    let mut total = 0usize;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("The attachment download was interrupted. Try again."))?
    {
        total += chunk.len();
        if total > max {
            bail!("This attachment exceeds your file size limit.");
        }
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::net::Ipv4Addr;
    #[test]
    fn only_resend_computed_authentication_is_trusted() {
        for (auth, allowed) in [
            (json!({"dmarc":"pass"}), true),
            (json!({"dmarc":"gray","dkim":"pass"}), true),
            (json!({"dmarc":"fail","dkim":"pass"}), false),
            (json!({"dmarc":"gray","spf":"pass"}), false),
            (json!({}), false),
        ] {
            assert_eq!(
                authenticated(
                    &json!({"authentication":auth,"headers":{"authentication-results":"dmarc=pass"}})
                ),
                allowed
            );
        }
    }
    #[test]
    fn blocks_untrusted_download_targets() {
        for url in [
            "http://resend.com/file",
            "https://evil.example/file",
            "https://resend.com.evil.example/file",
            "https://user:pass@resend.com/file",
            "https://resend.com:22/file",
        ] {
            assert!(trusted_url(url).is_err());
        }
        assert!(trusted_url("https://inbound-cdn.resend.com/file").is_ok());
        for ip in [
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::new(10, 0, 0, 1),
            Ipv4Addr::new(169, 254, 169, 254),
            Ipv4Addr::new(100, 64, 0, 1),
            Ipv4Addr::new(203, 0, 113, 1),
        ] {
            assert!(!public_ip(IpAddr::V4(ip)));
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
        assert!(!public_ip("::ffff:127.0.0.1".parse().unwrap()));
    }
    #[test]
    fn address_matching_is_exact() {
        assert_eq!(address("Alex <ALEX@example.com>"), "alex@example.com");
        assert_eq!(address("alex+other@example.com"), "alex+other@example.com");
        assert_eq!(address("a@example.com,b@example.com"), "");
    }
}
