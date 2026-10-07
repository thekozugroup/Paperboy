//! Release discovery and a small file protocol. The web service never accesses Docker.
use anyhow::{Result, bail};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

pub const REPOSITORY: &str = "https://github.com/thekozugroup/Paperboy";
pub const RELEASE_API: &str = "https://api.github.com/repos/thekozugroup/Paperboy/releases/latest";
pub const APP_IMAGE: &str = "ghcr.io/thekozugroup/paperboy";
pub const CONVERTER_IMAGE: &str = "ghcr.io/thekozugroup/paperboy-converter";

pub fn version(input: &str) -> Result<Version> {
    let value = Version::parse(input.strip_prefix('v').unwrap_or(input))?;
    if !value.pre.is_empty() || !value.build.is_empty() || input.len() > 32 {
        bail!("Choose a stable release version, such as 0.3.0.");
    }
    Ok(value)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub url: String,
}
impl Release {
    pub fn parse(value: &Value) -> Result<Self> {
        let tag = value["tag_name"].as_str().unwrap_or("");
        let v = version(tag)?;
        let url = format!("{REPOSITORY}/releases/tag/v{v}");
        if value["draft"] != false || value["prerelease"] != false || value["html_url"] != url {
            bail!("The release could not be verified.");
        }
        // A published release must include the installation file. Draft/image-only builds are ignored.
        if !value["assets"]
            .as_array()
            .is_some_and(|assets| assets.iter().any(|a| a["name"] == "compose.release.yaml"))
        {
            bail!("The release is still being prepared.");
        }
        Ok(Self {
            version: v.to_string(),
            url,
        })
    }
}
pub async fn latest() -> Result<Release> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()?;
    let response = client
        .get(RELEASE_API)
        .header("User-Agent", "Paperboy-release-check")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("Release information is temporarily unavailable. Try again later.");
    }
    if response
        .content_length()
        .is_some_and(|size| size > 1024 * 1024)
    {
        bail!("Invalid release information.");
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > 1024 * 1024 {
            bail!("Invalid release information.");
        }
        bytes.extend_from_slice(&chunk);
    }
    Release::parse(&serde_json::from_slice(&bytes)?)
}
pub fn atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Invalid update path."))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(value)?)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
pub fn read(path: &Path) -> Value {
    fs::metadata(path)
        .ok()
        .filter(|m| m.len() <= 65536)
        .and_then(|_| fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(json!({}))
}
pub fn control_dir() -> Option<PathBuf> {
    std::env::var_os("PAPERBOY_UPDATE_DIR").map(PathBuf::from)
}
pub fn connected(path: &Path) -> bool {
    read(&path.join("status.json"))["heartbeat"]
        .as_i64()
        .is_some_and(|time| (0..90).contains(&(chrono::Utc::now().timestamp() - time)))
}
pub fn draining() -> bool {
    control_dir().is_some_and(|path| {
        if path.join("journal.json").exists()
            && read(&path.join("journal.json"))["committed"] != true
        {
            return true;
        }
        // Before replacement starts a vanished updater's lease can expire safely.
        read(&path.join("drain.json"))["until"]
            .as_i64()
            .is_some_and(|time| time > chrono::Utc::now().timestamp())
    })
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub automatic: bool,
}

pub async fn check(store: &crate::store::Store) -> Result<()> {
    let result = latest().await;
    let mut status = store.get("release_status")?;
    if !status.is_object() {
        status = json!({});
    }
    status["checked_at"] = json!(crate::store::now());
    match result {
        Ok(release) => {
            status["latest"] = json!(release.version);
            status["release_url"] = json!(release.url);
            status["error"] = Value::Null;
        }
        Err(_) => {
            status["error"] = json!("Couldn’t check for updates. Try again later.");
        }
    }
    store.set(json!({"release_status":status}))
}
pub async fn monitor(app: std::sync::Arc<crate::app::App>) {
    let mut next = std::time::Instant::now();
    loop {
        if let Some(path) = control_dir() {
            let mut ready = if let Ok(_gate) = app.worker.gate.try_lock() {
                app.store.rows("SELECT id FROM jobs WHERE status IN ('preparing','submitting','submitted') LIMIT 1", &[]).is_ok_and(|jobs| jobs.is_empty())
            } else {
                false
            };
            if ready
                && draining()
                && crate::printers::status(&app.store.get("printer").unwrap_or(Value::Null)).await["state"]
                    == "printing"
            {
                ready = false;
            }
            let _ = atomic(
                &path.join("app.json"),
                &json!({"heartbeat":chrono::Utc::now().timestamp(),"ready":ready,"version":env!("CARGO_PKG_VERSION")}),
            );
        }
        if !app.demo
            && std::time::Instant::now() >= next
            && !control_dir().is_some_and(|p| connected(&p))
        {
            let _ = check(&app.store).await;
            next = std::time::Instant::now() + Duration::from_secs(21600);
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

pub fn state(cached: Value) -> Value {
    let mut status = control_dir()
        .map(|path| read(&path.join("status.json")))
        .unwrap_or(json!({}));
    let managed = control_dir().is_some_and(|path| connected(&path));
    if !managed {
        status = cached;
    }
    let current = env!("CARGO_PKG_VERSION");
    status["current"] = json!(current);
    status["repository"] = json!(REPOSITORY);
    status["managed"] = json!(managed);
    status["available"] = json!(
        status["latest"]
            .as_str()
            .and_then(|s| version(s).ok())
            .is_some_and(|v| v > version(current).unwrap())
    );
    status["draining"] = json!(draining());
    status
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_is_stable_and_bound_to_the_paperboy_repo() {
        let mut r = json!({"tag_name":"v0.4.0","html_url":format!("{REPOSITORY}/releases/tag/v0.4.0"),"draft":false,"prerelease":false,"assets":[{"name":"compose.release.yaml"}]});
        assert_eq!(Release::parse(&r).unwrap().version, "0.4.0");
        for tag in ["v0.4.0-rc1", "v0.4.0+build", "latest", "0.3", "../../evil"] {
            r["tag_name"] = json!(tag);
            assert!(Release::parse(&r).is_err());
        }
        r["tag_name"] = json!("v0.4.0");
        r["html_url"] = json!("https://github.com/other/repo/releases/tag/v0.4.0");
        assert!(Release::parse(&r).is_err());
    }
    #[test]
    fn versions_order_numerically_and_ipc_is_bounded() {
        assert!(version("0.10.0").unwrap() > version("0.9.0").unwrap());
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("policy.json");
        atomic(&path, &Policy { automatic: true }).unwrap();
        assert_eq!(read(&path)["automatic"], true);
        fs::write(&path, vec![b' '; 65537]).unwrap();
        assert_eq!(read(&path), json!({}));
        assert!(serde_json::from_value::<Policy>(json!({"automatic":true,"image":"bad"})).is_err());
    }
}
