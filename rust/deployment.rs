//! Docker replacement with a durable journal. Only this optional companion has Docker access.
use crate::updates::{self, APP_IMAGE, CONVERTER_IMAGE};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

#[derive(Clone)]
pub struct Engine {
    client: reqwest::Client,
    base: String,
}
impl Engine {
    pub async fn open(socket: &Path) -> Result<Self> {
        let client = reqwest::Client::builder()
            .unix_socket(socket)
            .no_proxy()
            .timeout(Duration::from_secs(600))
            .build()?;
        let info: Value = client
            .get("http://localhost/version")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let version = info["ApiVersion"]
            .as_str()
            .context("Docker did not report its API version.")?;
        if !version.starts_with("1.")
            || version.len() > 6
            || !version[2..].chars().all(|c| c.is_ascii_digit())
        {
            bail!("Docker reported an invalid API version.");
        }
        Ok(Self {
            client,
            base: format!("http://localhost/v{version}"),
        })
    }
    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let mut request = self.client.request(method, format!("{}{path}", self.base));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(Value::Null);
        }
        if !response.status().is_success() {
            bail!(
                "Docker could not complete the update operation ({}).",
                response.status().as_u16()
            );
        }
        let bytes = response.bytes().await?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
    pub async fn inspect(&self, id: &str) -> Result<Value> {
        self.inspect_optional(id)
            .await?
            .context("The Docker container is missing.")
    }
    pub async fn inspect_optional(&self, id: &str) -> Result<Option<Value>> {
        reference(id)?;
        let response = self
            .client
            .get(format!("{}/containers/{id}/json", self.base))
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            bail!("Docker could not inspect the managed container.");
        }
        Ok(Some(response.json().await?))
    }
    pub async fn post(&self, path: &str, body: Option<Value>) -> Result<Value> {
        self.request(reqwest::Method::POST, path, body).await
    }
    pub async fn remove(&self, id: &str) -> Result<()> {
        reference(id)?;
        // Never remove volumes. Named printer/data volumes survive replacement and rollback.
        self.request(
            reqwest::Method::DELETE,
            &format!("/containers/{id}?force=true"),
            None,
        )
        .await?;
        Ok(())
    }
    pub async fn members(&self, project: &str) -> Result<Vec<Value>> {
        let filters = json!({"label":[format!("com.docker.compose.project={project}"),"life.paperboy.managed=true"]});
        let encoded = url::form_urlencoded::byte_serialize(filters.to_string().as_bytes())
            .collect::<String>();
        let values = self
            .request(
                reqwest::Method::GET,
                &format!("/containers/json?all=true&filters={encoded}"),
                None,
            )
            .await?;
        Ok(values
            .as_array()
            .context("Docker returned an invalid service list.")?
            .clone())
    }
    pub async fn pull(&self, image: &str, release: &str) -> Result<String> {
        updates::version(release)?;
        if ![APP_IMAGE, CONVERTER_IMAGE].contains(&image) {
            bail!("Only official Paperboy images can be installed.");
        }
        let mut response = self
            .client
            .post(format!(
                "{}/images/create?fromImage={image}&tag={release}",
                self.base
            ))
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("The release image could not be downloaded.");
        }
        let mut buffer = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            buffer.extend_from_slice(&chunk);
            while let Some(end) = buffer.iter().position(|c| *c == b'\n') {
                let line: Vec<_> = buffer.drain(..=end).collect();
                if let Ok(v) = serde_json::from_slice::<Value>(&line)
                    && (v.get("error").is_some() || v.get("errorDetail").is_some())
                {
                    bail!("The release image could not be downloaded.");
                }
            }
            if buffer.len() > 1024 * 1024 {
                bail!("Docker returned invalid download information.");
            }
        }
        if !buffer.iter().all(u8::is_ascii_whitespace)
            && serde_json::from_slice::<Value>(&buffer)?
                .get("error")
                .is_some()
        {
            bail!("The release image could not be downloaded.");
        }
        let info = self
            .request(
                reqwest::Method::GET,
                &format!("/images/{image}:{release}/json"),
                None,
            )
            .await?;
        if info["Config"]["Labels"]["org.opencontainers.image.source"] != updates::REPOSITORY
            || info["Config"]["Labels"]["org.opencontainers.image.version"] != release
        {
            bail!("The downloaded image does not match this release.");
        }
        Ok(info["Id"]
            .as_str()
            .context("Invalid Docker image identity.")?
            .into())
    }
    pub async fn create(&self, name: &str, body: Value) -> Result<String> {
        reference(name)?;
        let result = self
            .post(&format!("/containers/create?name={name}"), Some(body))
            .await?;
        Ok(result["Id"]
            .as_str()
            .context("Docker did not create the replacement.")?
            .into())
    }
    pub async fn health(&self, id: &str) -> Result<()> {
        for _ in 0..60 {
            let state = self.inspect(id).await?;
            if state["State"]["Health"]["Status"] == "healthy" {
                return Ok(());
            }
            if state["State"]["Running"] != true
                || state["State"]["Health"]["Status"] == "unhealthy"
            {
                bail!("The replacement did not become healthy.");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        bail!("The replacement took too long to start.")
    }
    pub async fn refresh_channels(&self, project: &str, release: &str) -> Result<()> {
        let selected = updates::version(release)?;
        let members = self.members(project).await?;
        for (role, image) in [("app", APP_IMAGE), ("converter", CONVERTER_IMAGE)] {
            let matching: Vec<_> = members
                .iter()
                .filter(|v| v["Labels"]["life.paperboy.role"] == role)
                .collect();
            if matching.len() != 1 {
                bail!("The updated installation is incomplete.");
            }
            let info = self
                .inspect(
                    matching[0]["Id"]
                        .as_str()
                        .context("Missing service identity.")?,
                )
                .await?;
            validate(&info, project, role)?;
            if info["Config"]["Labels"]["org.opencontainers.image.version"] != release {
                bail!("The running services do not match the installed release.");
            }
            let id = info["Image"].as_str().context("Missing image identity.")?;
            if id.len() != 71
                || !id.starts_with("sha256:")
                || !id[7..].chars().all(|c| c.is_ascii_hexdigit())
            {
                bail!("Invalid image identity.");
            }
            for tag in ["stable", "latest"] {
                let response = self
                    .client
                    .get(format!("{}/images/{image}:{tag}/json", self.base))
                    .send()
                    .await?;
                if response.status().is_success() {
                    let previous: Value = response.json().await?;
                    // Tags are shared by installations on this host. Never move them backwards.
                    if previous["Config"]["Labels"]["org.opencontainers.image.version"]
                        .as_str()
                        .and_then(|v| updates::version(v).ok())
                        .is_some_and(|v| v >= selected)
                    {
                        continue;
                    }
                } else if response.status() != reqwest::StatusCode::NOT_FOUND {
                    bail!("Docker could not inspect the cached release channel.");
                }
                self.post(&format!("/images/{id}/tag?repo={image}&tag={tag}"), None)
                    .await?;
            }
        }
        Ok(())
    }
}
fn reference(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
    {
        bail!("Invalid managed container reference.");
    }
    Ok(())
}
pub fn replacement(info: &Value, image: &str, release: &str) -> Result<Value> {
    let mut config = info["Config"].clone();
    if !config.is_object() || !info["HostConfig"].is_object() {
        bail!("Invalid container configuration.");
    }
    config["Image"] = json!(image);
    config.as_object_mut().unwrap().remove("Hostname");
    config["Labels"]["org.opencontainers.image.version"] = json!(release);
    config["HostConfig"] = info["HostConfig"].clone();
    // Preserve named volume sources even when the original container used image-declared volumes.
    let binds: Vec<_> = info["Mounts"]
        .as_array()
        .context("Missing persistent mounts.")?
        .iter()
        .filter(|mount| mount["Type"] == "volume" || mount["Type"] == "bind")
        .map(|mount| {
            let source = if mount["Type"] == "volume" {
                mount["Name"].as_str()
            } else {
                mount["Source"].as_str()
            }
            .context("Invalid persistent mount.")?;
            let target = mount["Destination"]
                .as_str()
                .context("Invalid persistent mount.")?;
            Ok(json!(format!(
                "{source}:{target}:{}",
                if mount["RW"] == true { "rw" } else { "ro" }
            )))
        })
        .collect::<Result<_>>()?;
    config["HostConfig"]["Binds"] = json!(binds);
    config["HostConfig"]["Mounts"] = json!([]);
    let mode = info["HostConfig"]["NetworkMode"]
        .as_str()
        .unwrap_or("default");
    if !["host", "none"].contains(&mode) {
        let mut endpoints = json!({});
        if let Some(networks) = info["NetworkSettings"]["Networks"].as_object() {
            for (name, net) in networks {
                if net["IPAMConfig"].as_object().is_some_and(|o| !o.is_empty()) {
                    bail!("Updates require dynamically assigned container addresses.");
                }
                endpoints[name] = json!({"Aliases":net["Aliases"]});
            }
        }
        config["NetworkingConfig"] = json!({"EndpointsConfig":endpoints});
    }
    Ok(config)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Saved {
    pub id: String,
    pub name: String,
    pub backup: String,
    pub role: String,
}
impl Saved {
    fn check(&self) -> Result<()> {
        reference(&self.name)?;
        if self.id.len() != 64
            || !self.id.chars().all(|c| c.is_ascii_hexdigit())
            || self.backup != format!("{}-paperboy-old-{}", self.name, &self.id[..12])
            || !["app", "converter", "updater"].contains(&self.role.as_str())
        {
            bail!("Invalid saved update identity.");
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Journal {
    pub services: Vec<Saved>,
    pub created: Vec<String>,
    pub committed: bool,
    pub version: String,
}
pub async fn handoff(engine: &Engine, path: &Path, project: &str, own: &str) -> Result<()> {
    if !path.join("handoff.json").exists() {
        return Ok(());
    }
    let old: Saved = serde_json::from_value(updates::read(&path.join("handoff.json")))?;
    old.check()?;
    if own.len() < 12 || !own.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("Run the updater as a managed Docker service.");
    }
    let current = engine.inspect(own).await?;
    validate(&current, project, "updater")?;
    if old.id.starts_with(own) {
        if current["Name"] == format!("/{}", old.backup) {
            if engine.inspect_optional(&old.name).await?.is_some() {
                bail!("The replacement is completing the updater handoff.");
            }
            engine
                .post(
                    &format!("/containers/{}/rename?name={}", old.id, old.name),
                    None,
                )
                .await?;
        } else if current["Name"] != format!("/{}", old.name) {
            bail!("The updater handoff does not match this installation.");
        }
    } else {
        if current["Name"] != format!("/{}", old.name) {
            bail!("Only the managed replacement can finish the updater handoff.");
        }
        if let Some(info) = engine.inspect_optional(&old.id).await? {
            validate(&info, project, "updater")?;
            if info["Name"] != format!("/{}", old.backup) {
                bail!("The previous updater no longer matches the saved handoff.");
            }
            engine.remove(&old.id).await?;
        }
    }
    std::fs::remove_file(path.join("handoff.json"))?;
    Ok(())
}

pub fn saved(info: &Value, role: &str) -> Result<Saved> {
    let id = info["Id"]
        .as_str()
        .context("Invalid service identity.")?
        .to_owned();
    let name = info["Name"]
        .as_str()
        .context("Invalid service name.")?
        .trim_start_matches('/')
        .to_owned();
    if !id.chars().all(|c| c.is_ascii_hexdigit())
        || id.len() != 64
        || name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
    {
        bail!("Invalid managed service identity.");
    }
    Ok(Saved {
        backup: format!("{name}-paperboy-old-{}", &id[..12]),
        id,
        name,
        role: role.into(),
    })
}
pub async fn recover(engine: &Engine, path: &Path, project: &str) -> Result<()> {
    if !path.join("journal.json").exists() {
        return Ok(());
    }
    let journal: Journal = serde_json::from_value(updates::read(&path.join("journal.json")))?;
    // Validate every recorded identity before touching the daemon, including after a host crash.
    for service in &journal.services {
        service.check()?;
        if let Some(info) = engine.inspect_optional(&service.id).await? {
            validate(&info, project, &service.role)?;
            if saved(&info, &service.role)?.id != service.id
                || !info["Name"].as_str().is_some_and(|n| {
                    [format!("/{}", service.name), format!("/{}", service.backup)]
                        .contains(&n.to_owned())
                })
            {
                bail!("The saved update no longer matches this installation.");
            }
        } else if !journal.committed {
            bail!("A previous container is missing. Restore it before retrying the update.");
        }
    }
    for id in &journal.created {
        if id.len() != 64 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("Invalid replacement identity in the update journal.");
        }
    }
    if journal.committed {
        for old in &journal.services {
            if engine.inspect_optional(&old.id).await?.is_some() {
                engine.remove(&old.id).await?;
            }
        }
    } else {
        // Discover a replacement by name too: a crash can occur after Docker create but before
        // its new ID is written to the journal. Never lose that orphan or overwrite the old name.
        for old in &journal.services {
            if let Some(info) = engine.inspect_optional(&old.name).await?
                && info["Id"] != old.id
            {
                validate(&info, project, &old.role)?;
                engine
                    .remove(
                        info["Id"]
                            .as_str()
                            .context("Invalid replacement identity.")?,
                    )
                    .await?;
            }
        }
        for id in &journal.created {
            if let Some(info) = engine.inspect_optional(id).await? {
                let role = info["Config"]["Labels"]["life.paperboy.role"]
                    .as_str()
                    .unwrap_or("");
                validate(&info, project, role)?;
                engine.remove(id).await?;
            }
        }
        for old in journal.services.iter().rev() {
            if engine.inspect(&old.id).await?["Name"] != format!("/{}", old.name) {
                engine
                    .post(
                        &format!("/containers/{}/rename?name={}", old.id, old.name),
                        None,
                    )
                    .await?;
            }
            engine
                .post(&format!("/containers/{}/start", old.id), None)
                .await?;
        }
    }
    std::fs::remove_file(path.join("journal.json"))?;
    let _ = std::fs::remove_file(path.join("drain.json"));
    Ok(())
}
pub fn validate(info: &Value, project: &str, role: &str) -> Result<()> {
    let labels = &info["Config"]["Labels"];
    if !["app", "converter", "updater"].contains(&role)
        || labels["life.paperboy.managed"] != "true"
        || labels["com.docker.compose.project"] != project
        || labels["life.paperboy.role"] != role
    {
        bail!("This container is outside the managed Paperboy installation.");
    }
    let image = info["Config"]["Image"].as_str().unwrap_or("");
    let expected = if role == "converter" {
        CONVERTER_IMAGE
    } else {
        APP_IMAGE
    };
    if !(image.starts_with(&format!("{expected}:")) || image.starts_with("sha256:")) {
        bail!("Only official Paperboy installations can be updated.");
    }
    Ok(())
}
pub async fn replace(
    engine: &Engine,
    path: &Path,
    project: &str,
    release: &str,
    services: Vec<Value>,
    images: &[String; 2],
) -> Result<()> {
    let mut journal = Journal {
        version: release.into(),
        ..Default::default()
    };
    let mut plans = Vec::new();
    // Preflight all services before stopping either. Converter starts before the app.
    for (index, role) in ["converter", "app"].iter().enumerate() {
        let matching: Vec<_> = services
            .iter()
            .filter(|v| v["Labels"]["life.paperboy.role"] == *role)
            .collect();
        if matching.len() != 1 {
            bail!("The installation must contain exactly one app and one converter.");
        }
        let id = matching[0]["Id"]
            .as_str()
            .context("Missing service identity.")?;
        let info = engine.inspect(id).await?;
        validate(&info, project, role)?;
        let old = saved(&info, role)?;
        plans.push((old.clone(), replacement(&info, &images[index], release)?));
        journal.services.push(old);
    }
    updates::atomic(&path.join("journal.json"), &journal)?;
    // Stop app first so no converter call or CUPS write is in flight.
    for old in journal.services.iter().rev() {
        engine
            .post(&format!("/containers/{}/stop?t=30", old.id), None)
            .await?;
    }
    for (old, body) in plans {
        engine
            .post(
                &format!("/containers/{}/rename?name={}", old.id, old.backup),
                None,
            )
            .await?;
        let new = engine.create(&old.name, body).await?;
        journal.created.push(new.clone());
        updates::atomic(&path.join("journal.json"), &journal)?;
        engine
            .post(&format!("/containers/{new}/start"), None)
            .await?;
        engine.health(&new).await?;
    }
    journal.committed = true;
    updates::atomic(&path.join("journal.json"), &journal)?;
    recover(engine, path, project).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };
    struct Fixture {
        containers: BTreeMap<String, Value>,
        images: BTreeMap<String, Value>,
        calls: Vec<String>,
        bad: bool,
        next: u64,
    }
    async fn fixture(bad: bool) -> (Engine, Arc<Mutex<Fixture>>, tokio::task::JoinHandle<()>) {
        use axum::{
            Json, Router,
            body::to_bytes,
            extract::{Request, State},
            http::StatusCode,
            response::IntoResponse,
            routing::any,
        };
        async fn handler(
            State(state): State<Arc<Mutex<Fixture>>>,
            request: Request,
        ) -> axum::response::Response {
            let method = request.method().to_string();
            let uri = request.uri().clone();
            let body = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
            let mut state = state.lock().unwrap();
            let path = uri.path();
            state.calls.push(format!("{method} {uri}"));
            let query: BTreeMap<_, _> =
                url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
                    .into_owned()
                    .collect();
            if path == "/containers/json" {
                return Json(json!(members_from_containers(&state.containers))).into_response();
            }
            if path.starts_with("/images/") {
                if path.ends_with("/json") {
                    let key = path
                        .trim_start_matches("/images/")
                        .trim_end_matches("/json");
                    return match state.images.get(key) {
                        Some(image) => Json(image.clone()).into_response(),
                        None => StatusCode::NOT_FOUND.into_response(),
                    };
                }
                if path.ends_with("/tag") {
                    let key = format!("{}:{}", query["repo"], query["tag"]);
                    state.images.insert(
                        key,
                        json!({"Config":{"Labels":{"org.opencontainers.image.version":"0.3.0"}}}),
                    );
                    return StatusCode::CREATED.into_response();
                }
            }
            if path == "/containers/create" {
                state.next += 1;
                let id = format!("{:064x}", state.next);
                let config: Value = serde_json::from_slice(&body).unwrap();
                let name = format!("/{}", query["name"]);
                let health = if state.bad && config["Labels"]["life.paperboy.role"] == "app" {
                    "unhealthy"
                } else {
                    "healthy"
                };
                state.containers.insert(id.clone(),json!({"Id":id,"Name":name,"HostConfig":config["HostConfig"],"Config":config,"Mounts":[],"State":{"Running":false,"Health":{"Status":health}}}));
                return Json(json!({"Id":id})).into_response();
            }
            let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
            let key = parts.get(1).copied().unwrap_or("");
            let id = state
                .containers
                .iter()
                .find(|(id, v)| id.as_str() == key || v["Name"] == format!("/{key}"))
                .map(|(id, _)| id.clone());
            let Some(id) = id else {
                return StatusCode::NOT_FOUND.into_response();
            };
            if method == "DELETE" {
                state.containers.remove(&id);
                return StatusCode::NO_CONTENT.into_response();
            }
            let container = state.containers.get_mut(&id).unwrap();
            match parts.get(2).copied() {
                Some("json") => Json(container.clone()).into_response(),
                Some("stop") => {
                    container["State"]["Running"] = json!(false);
                    StatusCode::NO_CONTENT.into_response()
                }
                Some("start") => {
                    container["State"]["Running"] = json!(true);
                    StatusCode::NO_CONTENT.into_response()
                }
                Some("rename") => {
                    container["Name"] = json!(format!("/{}", query["name"]));
                    StatusCode::NO_CONTENT.into_response()
                }
                _ => StatusCode::BAD_REQUEST.into_response(),
            }
        }
        let mut containers = BTreeMap::new();
        for (id, role, image) in [
            ("a".repeat(64), "app", APP_IMAGE),
            ("b".repeat(64), "converter", CONVERTER_IMAGE),
        ] {
            containers.insert(id.clone(),json!({"Id":id,"Image":format!("sha256:{id}"),"Name":format!("/{role}"),"Config":{"Image":format!("{image}:0.3.0"),"Labels":{"org.opencontainers.image.version":"0.3.0","life.paperboy.managed":"true","life.paperboy.role":role,"com.docker.compose.project":"qa"}},"HostConfig":{"NetworkMode":"none"},"Mounts":[{"Type":"volume","Name":"keep-data","Destination":"/data","RW":true}],"State":{"Running":true,"Health":{"Status":"healthy"}}}));
        }
        let state = Arc::new(Mutex::new(Fixture {
            containers,
            images: [APP_IMAGE, CONVERTER_IMAGE].into_iter().flat_map(|image| ["stable", "latest"].into_iter().map(move |tag| (format!("{image}:{tag}"), json!({"Config":{"Labels":{"org.opencontainers.image.version":"0.2.0"}}})))).collect(),
            calls: Vec::new(),
            bad,
            next: 1,
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/{*path}", any(handler))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (
            Engine {
                client: reqwest::Client::builder().no_proxy().build().unwrap(),
                base,
            },
            state,
            task,
        )
    }
    fn members_from_containers(containers: &BTreeMap<String, Value>) -> Vec<Value> {
        containers
            .values()
            .map(|v| json!({"Id":v["Id"],"Labels":v["Config"]["Labels"]}))
            .collect()
    }
    fn members(state: &Arc<Mutex<Fixture>>) -> Vec<Value> {
        members_from_containers(&state.lock().unwrap().containers)
    }
    #[tokio::test]
    async fn cached_channels_advance_after_upgrade_but_never_move_backwards() {
        let (engine, state, task) = fixture(false).await;
        engine.refresh_channels("qa", "0.3.0").await.unwrap();
        assert_eq!(
            state
                .lock()
                .unwrap()
                .calls
                .iter()
                .filter(|c| c.contains("/tag?"))
                .count(),
            4
        );
        state.lock().unwrap().calls.clear();
        for image in state.lock().unwrap().images.values_mut() {
            image["Config"]["Labels"]["org.opencontainers.image.version"] = json!("0.4.0");
        }
        engine.refresh_channels("qa", "0.3.0").await.unwrap();
        assert!(
            !state
                .lock()
                .unwrap()
                .calls
                .iter()
                .any(|c| c.contains("/tag?"))
        );
        task.abort();
    }
    #[tokio::test]
    async fn altered_journal_references_are_rejected_before_docker_requests() {
        let (engine, state, task) = fixture(false).await;
        let path = tempfile::tempdir().unwrap();
        let mut old = saved(&engine.inspect(&"a".repeat(64)).await.unwrap(), "app").unwrap();
        old.name = "app?force=true".into();
        updates::atomic(
            &path.path().join("journal.json"),
            &Journal {
                services: vec![old],
                ..Default::default()
            },
        )
        .unwrap();
        state.lock().unwrap().calls.clear();
        assert!(recover(&engine, path.path(), "qa").await.is_err());
        assert!(state.lock().unwrap().calls.is_empty());
        assert!(
            engine
                .inspect_optional("app/json?escape=true")
                .await
                .is_err()
        );
        assert!(state.lock().unwrap().calls.is_empty());
        task.abort();
    }
    #[tokio::test]
    async fn updater_handoff_recovers_before_create_and_after_predecessor_removal() {
        for after_remove in [false, true] {
            let (engine, state, task) = fixture(false).await;
            let path = tempfile::tempdir().unwrap();
            let old_id = "a".repeat(64);
            let new_id = "c".repeat(64);
            let mut old = engine.inspect(&old_id).await.unwrap();
            old["Config"]["Labels"]["life.paperboy.role"] = json!("updater");
            state
                .lock()
                .unwrap()
                .containers
                .insert(old_id.clone(), old.clone());
            let saved = saved(&old, "updater").unwrap();
            updates::atomic(&path.path().join("handoff.json"), &saved).unwrap();
            if after_remove {
                let mut new = old.clone();
                new["Id"] = json!(new_id);
                state.lock().unwrap().containers.insert(new_id.clone(), new);
                state.lock().unwrap().containers.remove(&old_id);
                handoff(&engine, path.path(), "qa", &new_id).await.unwrap();
            } else {
                engine
                    .post(
                        &format!("/containers/{old_id}/rename?name={}", saved.backup),
                        None,
                    )
                    .await
                    .unwrap();
                handoff(&engine, path.path(), "qa", &old_id).await.unwrap();
                assert_eq!(engine.inspect(&old_id).await.unwrap()["Name"], "/app");
            }
            assert!(!path.path().join("handoff.json").exists());
            task.abort();
        }
    }
    #[tokio::test]
    async fn new_updater_removes_its_predecessor_before_acquiring_the_lock() {
        let (engine, state, task) = fixture(false).await;
        let path = tempfile::tempdir().unwrap();
        let old_id = "a".repeat(64);
        let new_id = "c".repeat(64);
        let mut old = engine.inspect(&old_id).await.unwrap();
        old["Config"]["Labels"]["life.paperboy.role"] = json!("updater");
        let saved = saved(&old, "updater").unwrap();
        let mut new = old.clone();
        new["Id"] = json!(new_id);
        old["Name"] = json!(format!("/{}", saved.backup));
        state.lock().unwrap().containers.insert(old_id.clone(), old);
        state.lock().unwrap().containers.insert(new_id.clone(), new);
        updates::atomic(&path.path().join("handoff.json"), &saved).unwrap();
        handoff(&engine, path.path(), "qa", &new_id).await.unwrap();
        assert!(!state.lock().unwrap().containers.contains_key(&old_id));
        assert!(state.lock().unwrap().containers.contains_key(&new_id));
        task.abort();
    }
    #[tokio::test]
    async fn healthy_replacement_keeps_volumes_and_commits_before_cleanup() {
        let (engine, state, task) = fixture(false).await;
        let path = tempfile::tempdir().unwrap();
        replace(
            &engine,
            path.path(),
            "qa",
            "0.4.0",
            members(&state),
            &["sha256:converter".into(), "sha256:app".into()],
        )
        .await
        .unwrap();
        let s = state.lock().unwrap();
        assert_eq!(s.containers.len(), 2);
        for c in s.containers.values() {
            assert_eq!(c["State"]["Running"], true);
            assert_eq!(c["HostConfig"]["Binds"], json!(["keep-data:/data:rw"]));
        }
        let stops: Vec<_> = s.calls.iter().filter(|c| c.contains("/stop?")).collect();
        assert!(stops[0].contains(&"a".repeat(64)));
        assert!(!s.calls.iter().any(|c| c.contains("v=true")));
        assert!(!path.path().join("journal.json").exists());
        task.abort();
    }
    #[tokio::test]
    async fn failed_startup_restores_both_previous_containers() {
        let (engine, state, task) = fixture(true).await;
        let path = tempfile::tempdir().unwrap();
        assert!(
            replace(
                &engine,
                path.path(),
                "qa",
                "0.4.0",
                members(&state),
                &["sha256:converter".into(), "sha256:app".into()]
            )
            .await
            .is_err()
        );
        assert!(path.path().join("journal.json").exists());
        recover(&engine, path.path(), "qa").await.unwrap();
        let s = state.lock().unwrap();
        assert_eq!(s.containers.len(), 2);
        assert!(s.containers.contains_key(&"a".repeat(64)));
        assert!(s.containers.contains_key(&"b".repeat(64)));
        for c in s.containers.values() {
            assert_eq!(c["State"]["Running"], true);
            assert!(!c["Name"].as_str().unwrap().contains("paperboy-old"));
        }
        task.abort();
    }
    #[tokio::test]
    async fn crash_between_create_and_journal_write_recovers_orphan() {
        let (engine, state, task) = fixture(false).await;
        let path = tempfile::tempdir().unwrap();
        let info = engine.inspect(&"a".repeat(64)).await.unwrap();
        let old = saved(&info, "app").unwrap();
        updates::atomic(
            &path.path().join("journal.json"),
            &Journal {
                services: vec![old.clone()],
                ..Default::default()
            },
        )
        .unwrap();
        engine
            .post(
                &format!("/containers/{}/rename?name={}", old.id, old.backup),
                None,
            )
            .await
            .unwrap();
        let orphan = engine
            .create(
                &old.name,
                replacement(&info, "sha256:app", "0.4.0").unwrap(),
            )
            .await
            .unwrap();
        recover(&engine, path.path(), "qa").await.unwrap();
        assert!(!state.lock().unwrap().containers.contains_key(&orphan));
        assert_eq!(engine.inspect(&old.id).await.unwrap()["Name"], "/app");
        task.abort();
    }
    #[tokio::test]
    async fn recovery_cannot_remove_another_projects_container() {
        let (engine, state, task) = fixture(false).await;
        let path = tempfile::tempdir().unwrap();
        state
            .lock()
            .unwrap()
            .containers
            .get_mut(&"a".repeat(64))
            .unwrap()["Config"]["Labels"]["com.docker.compose.project"] = json!("other");
        updates::atomic(
            &path.path().join("journal.json"),
            &Journal {
                created: vec!["a".repeat(64)],
                ..Default::default()
            },
        )
        .unwrap();
        assert!(recover(&engine, path.path(), "qa").await.is_err());
        assert_eq!(state.lock().unwrap().containers.len(), 2);
        task.abort();
    }
    #[test]
    fn replacement_preserves_settings_and_has_no_new_privileges() {
        let info = json!({"Config":{"Image":format!("{APP_IMAGE}:0.3.0"),"Env":["A=B"],"Hostname":"old","Labels":{"life.paperboy.role":"app"}},"HostConfig":{"NetworkMode":"host","Memory":536870912,"SecurityOpt":["no-new-privileges:true"]},"Mounts":[{"Type":"volume","Name":"paperboy-data","Destination":"/data","RW":true},{"Type":"bind","Source":"/tmp/control","Destination":"/updates","RW":false}]});
        let body = replacement(&info, "sha256:abc", "0.4.0").unwrap();
        assert_eq!(
            body["HostConfig"]["Binds"],
            json!(["paperboy-data:/data:rw", "/tmp/control:/updates:ro"])
        );
        assert_eq!(body["Env"], info["Config"]["Env"]);
        assert_eq!(body["HostConfig"]["Memory"], 536870912);
        assert!(body.get("Hostname").is_none());
        assert!(body.get("NetworkingConfig").is_none());
    }
    #[test]
    fn rejects_other_containers_and_static_addresses() {
        let info = json!({"Config":{"Labels":{"life.paperboy.managed":"true","life.paperboy.role":"app","com.docker.compose.project":"another"}}});
        assert!(validate(&info, "paperboy", "app").is_err());
        let info = json!({"Config":{},"HostConfig":{"NetworkMode":"bridge"},"Mounts":[],"NetworkSettings":{"Networks":{"lan":{"IPAMConfig":{"IPv4Address":"192.168.1.8"}}}}});
        assert!(replacement(&info, "test", "0.4.0").is_err());
    }
}
