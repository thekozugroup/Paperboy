#![forbid(unsafe_code)]
use anyhow::{Result, bail};
use paperboy::{app::App, store::Store};
use std::{env, path::PathBuf, time::Duration};

#[tokio::main]
async fn main() -> Result<()> {
    let port = env::var("PAPERBOY_PORT")
        .unwrap_or_else(|_| "8025".into())
        .parse::<u16>()?;
    let root = PathBuf::from(env::var("PAPERBOY_DATA_DIR").unwrap_or_else(|_| "./data".into()));
    match env::args().nth(1).as_deref() {
        Some("health") => {
            let response = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(3))
                .build()?
                .get(format!("http://127.0.0.1:{port}/api/health"))
                .send()
                .await?;
            if !response.status().is_success() {
                bail!("Paperboy is unavailable.");
            }
            return Ok(());
        }
        Some("reset-password") => {
            let store = Store::open(&root)?;
            store.db(|db| {
                let tx = db.transaction()?;
                tx.execute(
                    "DELETE FROM settings WHERE key IN ('password_hash','password_salt')",
                    [],
                )?;
                tx.execute("DELETE FROM sessions", [])?;
                tx.commit()?;
                Ok(())
            })?;
            let _ = std::fs::remove_file(root.join("setup.code"));
            eprintln!("Owner access reset. Start Paperboy and use the new setup code in its logs.");
            return Ok(());
        }
        Some(_) => bail!("Use paperboy, paperboy health, or paperboy reset-password."),
        None => {}
    }
    let demo = env::var("PAPERBOY_DEMO").as_deref() == Ok("1");
    let app = App::open(
        &root,
        env::var("PAPERBOY_CONVERTER_SOCKET")
            .unwrap_or_else(|_| "/run/paperboy/convert.sock".into()),
        demo,
        env::var("PAPERBOY_SECURE_COOKIE").as_deref() == Ok("1"),
    )?;
    if let Some(code) = &app.bootstrap {
        eprintln!("Paperboy setup code: {code}");
    }
    let worker = if !demo {
        app.worker.recover()?;
        Some(tokio::spawn(app.worker.clone().run()))
    } else {
        None
    };
    let web = PathBuf::from(env::var("PAPERBOY_WEB_DIR").unwrap_or_else(|_| "web".into()));
    let host = env::var("PAPERBOY_HOST").unwrap_or_else(|_| "0.0.0.0".into());
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    eprintln!("Paperboy is listening on port {port}.");
    let result = axum::serve(listener, app.router(&web))
        .with_graceful_shutdown(shutdown())
        .await;
    if let Some(worker) = worker {
        worker.abort();
        let _ = worker.await;
    }
    result?;
    Ok(())
}
async fn shutdown() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("Signal handler");
    tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
}
