#![forbid(unsafe_code)]
use anyhow::{Context, Result, bail};
use paperboy::{
    deployment::{self, Engine},
    updates::{self, Policy},
};
use serde_json::{Value, json};
use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

fn set(status: &Arc<Mutex<Value>>, values: Value) {
    if let Ok(mut s) = status.lock() {
        for (k, v) in values.as_object().unwrap() {
            s[k] = v.clone();
        }
    }
}
async fn install(
    engine: &Engine,
    path: &Path,
    project: &str,
    release: &str,
    status: &Arc<Mutex<Value>>,
) -> Result<()> {
    set(status, json!({"phase":"downloading","error":null}));
    let converter = engine.pull(updates::CONVERTER_IMAGE, release).await?;
    let app = engine.pull(updates::APP_IMAGE, release).await?;
    set(status, json!({"phase":"waiting"}));
    updates::atomic(
        &path.join("drain.json"),
        &json!({"until":chrono::Utc::now().timestamp()+180}),
    )?;
    // Wait for a fresh acknowledgment produced AFTER the drain request. Existing prints finish.
    let requested = chrono::Utc::now().timestamp();
    let mut ready = false;
    for _ in 0..180 {
        let app = updates::read(&path.join("app.json"));
        if app["ready"] == true
            && app["heartbeat"]
                .as_i64()
                .is_some_and(|t| t > requested && chrono::Utc::now().timestamp() - t < 15)
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    if !ready {
        bail!("Printing is still active. The update will wait for you to try again.");
    }
    set(status, json!({"phase":"installing"}));
    deployment::replace(
        engine,
        path,
        project,
        release,
        engine.members(project).await?,
        &[converter, app.clone()],
    )
    .await?;
    set(
        status,
        json!({"phase":"idle","installed_at":paperboy::store::now(),"failed_version":null,"error":null}),
    );
    // Upgrade the updater last. The new process cleans up its stopped predecessor.
    let members = engine.members(project).await?;
    let own: Vec<_> = members
        .iter()
        .filter(|m| {
            m["Labels"]["life.paperboy.role"] == "updater"
                && !m["Names"].as_array().is_some_and(|n| {
                    n.iter()
                        .any(|n| n.as_str().is_some_and(|s| s.contains("-paperboy-old-")))
                })
        })
        .collect();
    if own.len() != 1 {
        bail!("App updated. Recreate the updater with Docker Compose to update its tools.");
    }
    let info = engine
        .inspect(own[0]["Id"].as_str().context("Missing updater identity.")?)
        .await?;
    deployment::validate(&info, project, "updater")?;
    let old = deployment::saved(&info, "updater")?;
    updates::atomic(&path.join("handoff.json"), &old)?;
    engine
        .post(
            &format!("/containers/{}/rename?name={}", old.id, old.backup),
            None,
        )
        .await?;
    let body = deployment::replacement(&info, &app, release)?;
    match engine.create(&old.name, body).await {
        Ok(new) => {
            if engine
                .post(&format!("/containers/{new}/start"), None)
                .await
                .is_err()
            {
                engine.remove(&new).await?;
                engine
                    .post(
                        &format!("/containers/{}/rename?name={}", old.id, old.name),
                        None,
                    )
                    .await?;
                fs::remove_file(path.join("handoff.json"))?;
                bail!("App updated. The updater could not restart; retry the update.");
            }
            // The new updater removes this process's container; no self-stop race or restart loop.
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
        Err(error) => {
            engine
                .post(
                    &format!("/containers/{}/rename?name={}", old.id, old.name),
                    None,
                )
                .await?;
            fs::remove_file(path.join("handoff.json"))?;
            return Err(error);
        }
    }
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    let path = PathBuf::from(env::var("PAPERBOY_UPDATE_DIR").unwrap_or_else(|_| "/updates".into()));
    fs::create_dir_all(&path)?;
    // The app writes requests as uid 1000; the updater never mounts its secrets or database.
    let project = env::var("PAPERBOY_PROJECT").unwrap_or_else(|_| "paperboy".into());
    if project.is_empty()
        || !project
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
    {
        bail!("Invalid Compose project.");
    }
    let pin = env::var("PAPERBOY_PIN_VERSION").unwrap_or_default();
    if !pin.is_empty() {
        updates::version(&pin)?;
    }
    let engine = Engine::open(Path::new("/var/run/docker.sock")).await?;
    // Complete the self-handoff before taking the predecessor's exclusive lock.
    if path.join("handoff.json").exists() {
        let old: deployment::Saved =
            serde_json::from_value(updates::read(&path.join("handoff.json")))?;
        let info = engine.inspect(&old.id).await?;
        deployment::validate(&info, &project, "updater")?;
        // Only a replacement with the original name can finish the handoff.
        let own = env::var("HOSTNAME").unwrap_or_default();
        if !old.id.starts_with(&own)
            && !own.is_empty()
            && info["Name"] == format!("/{}", old.backup)
        {
            let current = engine.inspect(&own).await?;
            deployment::validate(&current, &project, "updater")?;
            if current["Name"] != format!("/{}", old.name) {
                bail!("Only the managed replacement can finish the updater handoff.");
            }
            engine.remove(&old.id).await?;
            fs::remove_file(path.join("handoff.json"))?;
        }
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.join("updater.lock"))?;
    lock.try_lock()
        .map_err(|_| anyhow::anyhow!("An updater is already running for this installation."))?;
    let status = Arc::new(Mutex::new(updates::read(&path.join("status.json"))));
    set(
        &status,
        json!({"phase":"recovering","pin":pin,"updater_version":env!("CARGO_PKG_VERSION")}),
    );
    // Heartbeat and drain lease survive long downloads/health waits without blocking the UI.
    let shared = status.clone();
    let control = path.clone();
    tokio::spawn(async move {
        loop {
            let s = {
                let mut s = shared.lock().unwrap();
                s["heartbeat"] = json!(chrono::Utc::now().timestamp());
                s.clone()
            };
            let _ = updates::atomic(&control.join("status.json"), &s);
            if ["waiting", "installing"].contains(&s["phase"].as_str().unwrap_or("")) {
                let _ = updates::atomic(
                    &control.join("drain.json"),
                    &json!({"until":chrono::Utc::now().timestamp()+180}),
                );
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
    deployment::recover(&engine, &path, &project).await?;
    if !path.join("policy.json").exists() {
        updates::atomic(
            &path.join("policy.json"),
            &Policy {
                automatic: env::var("PAPERBOY_AUTO_UPDATE").as_deref() == Ok("1"),
            },
        )?;
    }
    set(&status, json!({"phase":"idle"}));
    let mut next = Instant::now();
    let mut next_install = Instant::now();
    loop {
        let policy: Policy =
            serde_json::from_value(updates::read(&path.join("policy.json"))).unwrap_or_default();
        set(&status, json!({"automatic":policy.automatic}));
        let check = updates::read(&path.join("check.json"));
        let wanted = updates::read(&path.join("install.json"));
        let snapshot = status.lock().unwrap().clone();
        let manual = wanted["nonce"].is_string() && wanted["nonce"] != snapshot["handled_install"];
        let check_requested =
            check["nonce"].is_string() && check["nonce"] != snapshot["handled_check"];
        if Instant::now() >= next || check_requested || manual {
            // Every manual install resolves the official release again, never a caller-supplied image.
            set(
                &status,
                json!({"handled_check":check["nonce"],"checked_at":paperboy::store::now()}),
            );
            match updates::latest().await {
                Ok(release) => set(
                    &status,
                    json!({"latest":release.version,"release_url":release.url,"error":null}),
                ),
                Err(_) => set(
                    &status,
                    json!({"error":"Couldn’t check for updates. Try again later."}),
                ),
            }
            next = Instant::now() + Duration::from_secs(3600);
        }
        let snapshot = status.lock().unwrap().clone();
        let target = snapshot["latest"].as_str().unwrap_or("");
        let app = updates::read(&path.join("app.json"));
        let newer = updates::version(target)
            .ok()
            .zip(
                app["version"]
                    .as_str()
                    .and_then(|v| updates::version(v).ok()),
            )
            .is_some_and(|(v, current)| v > current);
        let requested = manual
            && wanted["version"] == target
            && updates::version(target)
                .ok()
                .zip(
                    app["version"]
                        .as_str()
                        .and_then(|v| updates::version(v).ok()),
                )
                .is_some_and(|(selected, current)| selected >= current);
        if manual {
            set(&status, json!({"handled_install":wanted["nonce"]}));
            updates::atomic(&path.join("status.json"), &*status.lock().unwrap())?;
        }
        if pin.is_empty()
            && snapshot["error"].is_null()
            && ((policy.automatic
                && newer
                && snapshot["failed_version"] != target
                && Instant::now() >= next_install)
                || requested)
            && updates::version(target).is_ok()
            && let Err(error) = install(&engine, &path, &project, target, &status).await
        {
            eprintln!("The update could not complete: {error}");
            let waiting = status.lock().unwrap()["phase"] == "waiting";
            set(&status, json!({"phase":"recovering"}));
            if deployment::recover(&engine, &path, &project).await.is_err() {
                set(
                    &status,
                    json!({"phase":"attention","error":"Recovery needs attention. Check the updater logs before restarting Paperboy."}),
                );
                bail!(
                    "Could not recover the previous containers. The update journal has been retained."
                );
            }
            let _ = fs::remove_file(path.join("drain.json"));
            next_install = Instant::now() + Duration::from_secs(3600);
            set(
                &status,
                json!({"phase":"idle","failed_version":if waiting {Value::Null}else{json!(target)},"error":if waiting {"Printing is still active. The updater will try again in an hour."}else{"Update failed. The previous installation was retained. Check the updater logs, then retry."}}),
            );
            eprintln!(
                "Paperboy update failed; previous containers retained. No saved data was removed."
            );
        }
        tokio::select! { _=paperboy::process::shutdown_signal()=>break, _=tokio::time::sleep(Duration::from_secs(2))=>{} }
    }
    Ok(())
}
