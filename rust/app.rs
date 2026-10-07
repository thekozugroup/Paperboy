use crate::{
    printers,
    resend::Resend,
    store::{Store, now},
    worker::Worker,
};
use anyhow::{Result, bail};
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
};
use rand::RngCore;
use rusqlite::params;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tower_http::services::{ServeDir, ServeFile};

pub struct App {
    pub store: Arc<Store>,
    pub worker: Arc<Worker>,
    pub demo: bool,
    pub secure_cookie: bool,
    pub bootstrap: Option<String>,
    attempts: Mutex<VecDeque<Instant>>,
}
impl App {
    pub fn open(root: &Path, socket: String, demo: bool, secure_cookie: bool) -> Result<Arc<Self>> {
        let store = Arc::new(Store::open(root)?);
        if demo
            && (!store.text("password_hash")?.is_empty()
                || !["", "preview"].contains(&store.text("api_key")?.as_str()))
        {
            bail!("Use an empty, separate data directory for the design preview.");
        }
        let bootstrap = if !demo && store.text("password_hash")?.is_empty() {
            let path = root.join("setup.code");
            if !path.exists() {
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)?;
                file.write_all(random(12).as_bytes())?;
                file.sync_all()?;
            }
            Some(fs::read_to_string(path)?.trim().to_owned())
        } else {
            None
        };
        if demo {
            seed_demo(&store)?;
        }
        Ok(Arc::new(Self {
            worker: Arc::new(Worker::new(store.clone(), socket)),
            store,
            demo,
            secure_cookie,
            bootstrap,
            attempts: Mutex::new(VecDeque::new()),
        }))
    }
    pub fn router(self: Arc<Self>, web: &Path) -> Router {
        Router::new()
            .route("/api/{*path}", any(api))
            .nest_service("/assets", ServeDir::new(web))
            .route_service("/", ServeFile::new(web.join("index.html")))
            .layer(middleware::from_fn_with_state(self.clone(), boundaries))
            .with_state(self)
    }
    fn session(&self, headers: &HeaderMap, write: bool) -> ApiResult<String> {
        if self.demo {
            if write {
                return Err(error(
                    403,
                    "This preview is read-only. Start your own Paperboy to make changes.",
                ));
            }
            return Ok("demo".into());
        }
        let token = digest(cookie(headers));
        let sessions = self.store.rows(
            "SELECT csrf FROM sessions WHERE token=? AND expires>?",
            &[&token, &epoch()],
        )?;
        let csrf = sessions
            .first()
            .and_then(|row| row["csrf"].as_str())
            .ok_or_else(|| error(401, "Sign in to Paperboy."))?;
        if write
            && !constant_time(
                headers
                    .get("x-paperboy-csrf")
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or(""),
                csrf,
            )
        {
            return Err(error(
                403,
                "Your session changed. Refresh Paperboy and try again.",
            ));
        }
        Ok(csrf.into())
    }
    fn new_session(&self) -> ApiResult<Response> {
        let token = random(32);
        let csrf = random(24);
        self.store.db(|db| {
            let tx = db.transaction()?;
            tx.execute("DELETE FROM sessions WHERE expires<?", [epoch()])?;
            tx.execute(
                "INSERT INTO sessions VALUES (?,?,?)",
                params![digest(&token), csrf, epoch() + 604800.0],
            )?;
            tx.commit()?;
            Ok(())
        })?;
        let mut response = success(json!({"authenticated":true,"csrf":csrf}));
        let secure = if self.secure_cookie { "; Secure" } else { "" };
        response.headers_mut().insert(header::SET_COOKIE,format!("paperboy_session={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=604800{secure}").parse().unwrap());
        Ok(response)
    }
    fn rate_limit(&self) -> ApiResult<()> {
        let mut attempts = self
            .attempts
            .lock()
            .map_err(|_| error(500, "Sign-in is temporarily unavailable."))?;
        while attempts
            .front()
            .is_some_and(|time| time.elapsed() > Duration::from_secs(300))
        {
            attempts.pop_front();
        }
        if attempts.len() >= 15 {
            return Err(error(
                429,
                "Too many sign-in attempts. Try again in five minutes.",
            ));
        }
        attempts.push_back(Instant::now());
        Ok(())
    }
}
#[derive(Debug)]
pub struct ApiError(StatusCode, String);
type ApiResult<T> = std::result::Result<T, ApiError>;
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, axum::Json(json!({"detail":self.1}))).into_response()
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(_: anyhow::Error) -> Self {
        error(500, "Paperboy could not complete this request. Try again.")
    }
}
fn error(status: u16, message: &str) -> ApiError {
    ApiError(StatusCode::from_u16(status).unwrap(), message.into())
}
fn success(value: Value) -> Response {
    axum::Json(value).into_response()
}
fn random(size: usize) -> String {
    let mut bytes = vec![0; size];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}
fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
fn constant_time(a: &str, b: &str) -> bool {
    bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}
fn epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
fn cookie(headers: &HeaderMap) -> &str {
    headers
        .get(header::COOKIE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == "paperboy_session").then_some(value))
        .unwrap_or("")
}
pub fn password_hash(password: &str, salt: &str) -> Result<String> {
    let mut output = [0u8; 64];
    let params = scrypt::Params::new(14, 8, 1, 64)?;
    scrypt::scrypt(
        password.as_bytes(),
        &hex::decode(salt)?,
        &params,
        &mut output,
    )?;
    Ok(hex::encode(output))
}
fn text<'a>(body: &'a Value, key: &str, min: usize, max: usize) -> ApiResult<&'a str> {
    body[key]
        .as_str()
        .filter(|s| (min..=max).contains(&s.chars().count()))
        .ok_or_else(|| {
            error(
                422,
                &format!("{key}: Enter a value between {min} and {max} characters."),
            )
        })
}
fn optional_text<'a>(body: &'a Value, key: &str, max: usize) -> ApiResult<&'a str> {
    if body.get(key).is_none() {
        Ok("")
    } else {
        text(body, key, 0, max)
    }
}
fn email(value: &str) -> ApiResult<String> {
    static EMAIL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let value = value.trim().to_lowercase();
    let pattern = EMAIL.get_or_init(|| {
        regex::Regex::new(r"^[a-z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-z0-9-]+(?:\.[a-z0-9-]+)+$").unwrap()
    });
    if value.len() > 254 || !pattern.is_match(&value) {
        return Err(error(422, "Enter a valid email address."));
    }
    Ok(value)
}
fn preferences(body: &Value) -> ApiResult<Value> {
    let mut result = json!({});
    for (key, default, choices) in [
        ("paper", "Letter", &["Letter", "A4"][..]),
        ("color", "monochrome", &["monochrome", "color"][..]),
        (
            "sides",
            "one-sided",
            &["one-sided", "two-sided-long-edge"][..],
        ),
    ] {
        let value = body
            .get(key)
            .map(|_| text(body, key, 1, 40))
            .transpose()?
            .unwrap_or(default);
        if !choices.contains(&value) {
            return Err(error(422, &format!("{key}: Choose a supported option.")));
        }
        result[key] = json!(value);
    }
    for (key, default, max) in [
        ("max_pages", 50, 100),
        ("max_size_mb", 25, 25),
        ("daily_limit", 20, 100),
        ("retention_days", 7, 90),
    ] {
        let value = body
            .get(key)
            .map_or(Some(default), Value::as_i64)
            .filter(|v| (1..=max).contains(v))
            .ok_or_else(|| error(422, &format!("{key}: Choose a number between 1 and {max}.")))?;
        result[key] = json!(value);
    }
    Ok(result)
}
async fn boundaries(State(app): State<Arc<App>>, mut request: Request, next: Next) -> Response {
    let is_api = request.uri().path().starts_with("/api/");
    let mut reject = None;
    if is_api && request.method() != Method::GET && request.method() != Method::HEAD {
        let headers = request.headers();
        let scheme = if app.secure_cookie { "https" } else { "http" };
        let host = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        if headers.get(header::ORIGIN).is_some_and(|origin| {
            origin.to_str().ok() != Some(format!("{scheme}://{host}").as_str())
        }) {
            reject = Some(error(403, "This request did not come from Paperboy."));
        } else if headers
            .get("sec-fetch-site")
            .is_some_and(|site| site == "cross-site")
        {
            reject = Some(error(403, "Cross-site requests are not allowed."));
        } else if let Some(length) = headers.get(header::CONTENT_LENGTH) {
            match length.to_str().ok().and_then(|s| s.parse::<u64>().ok()) {
                Some(length) if length > 16384 => {
                    reject = Some(error(413, "This request is too large."))
                }
                None => reject = Some(error(400, "Invalid request.")),
                _ => {}
            }
        }
        if reject.is_none() {
            let (parts, body) = request.into_parts();
            match to_bytes(body, 16384).await {
                Ok(bytes) => request = Request::from_parts(parts, Body::from(bytes)),
                Err(_) => {
                    request = Request::from_parts(parts, Body::empty());
                    reject = Some(error(413, "This request is too large."));
                }
            }
        }
    }
    let mut response = if let Some(error) = reject {
        error.into_response()
    } else {
        next.run(request).await
    };
    for (name, value) in [
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "no-referrer"),
        (
            "content-security-policy",
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
        (
            "cache-control",
            if is_api { "no-store" } else { "no-cache" },
        ),
    ] {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static(name),
            value.parse().unwrap(),
        );
    }
    response
}
async fn api(
    State(app): State<Arc<App>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    bytes: Bytes,
) -> ApiResult<Response> {
    let path = uri.path();
    let write = method != Method::GET && method != Method::HEAD;
    if !write && path == "/api/health" {
        return Ok(success(
            json!({"status":"ok","app":"Paperboy","version":env!("CARGO_PKG_VERSION"),"runtime":"Rust"}),
        ));
    }
    if !write && path == "/api/auth/status" {
        let csrf = app.session(&headers, false).ok();
        return Ok(success(
            json!({"initialized":app.demo || !app.store.text("password_hash")?.is_empty(),"authenticated":csrf.is_some(),"csrf":csrf,"demo":app.demo}),
        ));
    }
    let public_auth =
        method == Method::POST && ["/api/auth/setup", "/api/auth/login"].contains(&path);
    if !public_auth {
        app.session(&headers, write)?;
    }
    let body: Value = if write && !bytes.is_empty() {
        serde_json::from_slice(&bytes)
            .ok()
            .filter(Value::is_object)
            .ok_or_else(|| error(422, "Enter valid settings and try again."))?
    } else {
        json!({})
    };
    match (method.as_str(), path) {
        ("POST", "/api/auth/setup") => {
            let password = text(&body, "password", 10, 128)?.to_owned();
            let code = optional_text(&body, "setup_code", 128)?;
            app.rate_limit()?;
            let _gate = app.worker.gate.lock().await;
            if app.demo || !app.store.text("password_hash")?.is_empty() {
                return Err(error(
                    409,
                    "Paperboy already has an owner. Sign in instead.",
                ));
            }
            if !constant_time(code, app.bootstrap.as_deref().unwrap_or("")) {
                return Err(error(
                    403,
                    "That setup code is incorrect. Find it in your Docker logs.",
                ));
            }
            let salt = random(16);
            let hash_salt = salt.clone();
            let hash = tokio::task::spawn_blocking(move || password_hash(&password, &hash_salt))
                .await
                .map_err(|_| error(500, "Owner setup was interrupted."))??;
            app.store
                .set(json!({"password_salt":salt,"password_hash":hash}))?;
            let _ = fs::remove_file(app.store.root.join("setup.code"));
            app.new_session()
        }
        ("POST", "/api/auth/login") => {
            let password = text(&body, "password", 10, 128)?.to_owned();
            optional_text(&body, "setup_code", 128)?;
            app.rate_limit()?;
            let saved = app.store.text("password_hash")?;
            let salt = app.store.text("password_salt")?;
            if app.demo || saved.is_empty() {
                return Err(error(401, "That password is incorrect. Try again."));
            }
            let hash = tokio::task::spawn_blocking(move || password_hash(&password, &salt))
                .await
                .map_err(|_| error(500, "Sign-in was interrupted."))??;
            if !constant_time(&hash, &saved) {
                return Err(error(401, "That password is incorrect. Try again."));
            }
            app.new_session()
        }
        ("POST", "/api/auth/logout") => {
            app.store.db(|db| {
                db.execute(
                    "DELETE FROM sessions WHERE token=?",
                    [digest(cookie(&headers))],
                )?;
                Ok(())
            })?;
            let mut response = success(json!({"ok":true}));
            response.headers_mut().insert(
                header::SET_COOKIE,
                "paperboy_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"
                    .parse()
                    .unwrap(),
            );
            Ok(response)
        }
        ("GET" | "HEAD", "/api/state") => {
            let settings = app.store.settings()?;
            let printer_status = if app.demo {
                json!({"state":"ready","message":"Ready to print"})
            } else {
                printers::status(&settings["printer"]).await
            };
            let blocked=app.store.rows("SELECT * FROM messages WHERE status='blocked' AND sender IS NOT NULL ORDER BY created_at DESC LIMIT 30",&[])?;
            Ok(success(
                json!({"settings":settings,"senders":app.store.senders()?,"jobs":app.store.jobs()?,"blocked":blocked,"printer_status":printer_status,"demo":app.demo,"updates":crate::updates::state(app.store.get("release_status")?)}),
            ))
        }
        ("GET", "/api/updates") => Ok(success(crate::updates::state(
            app.store.get("release_status")?,
        ))),
        ("POST", "/api/updates/check") => {
            // A separate limit avoids exhausting GitHub's anonymous quota through repeated clicks.
            let last = app.store.get("release_requested")?.as_i64().unwrap_or(0);
            if chrono::Utc::now().timestamp() - last < 60 {
                return Err(error(429, "Wait a minute before checking again."));
            }
            app.store
                .set(json!({"release_requested":chrono::Utc::now().timestamp()}))?;
            if let Some(path) =
                crate::updates::control_dir().filter(|p| crate::updates::connected(p))
            {
                crate::updates::atomic(&path.join("check.json"), &json!({"nonce":random(16)}))?;
            } else {
                crate::updates::check(&app.store).await?;
            }
            Ok(success(json!({"ok":true})))
        }
        ("PUT", "/api/updates/policy") | ("POST", "/api/updates/install") => {
            let path = crate::updates::control_dir()
                .filter(|p| crate::updates::connected(p))
                .ok_or_else(|| {
                    error(
                        409,
                        "Enable the updater on this server first. See the installation guide.",
                    )
                })?;
            let status = crate::updates::read(&path.join("status.json"));
            if status["pin"].as_str().is_some_and(|s| !s.is_empty()) {
                return Err(error(
                    409,
                    "This server is pinned to a version. Remove its version pin to install updates.",
                ));
            }
            if path.as_os_str().is_empty() {
                return Err(error(409, "Updates are unavailable."));
            }
            if method == Method::PUT {
                let policy: crate::updates::Policy = serde_json::from_value(body)
                    .map_err(|_| error(422, "Choose an update preference."))?;
                crate::updates::atomic(&path.join("policy.json"), &policy)?;
            } else {
                let requested = text(&body, "version", 1, 32)?;
                let selected = crate::updates::version(requested)
                    .map_err(|_| error(422, "Choose a stable Paperboy release."))?;
                if selected <= crate::updates::version(env!("CARGO_PKG_VERSION")).unwrap() {
                    return Err(error(
                        409,
                        "Paperboy already has this release or a newer version.",
                    ));
                }
                if ["downloading", "waiting", "installing", "recovering"]
                    .contains(&status["phase"].as_str().unwrap_or(""))
                {
                    return Err(error(409, "An update is already in progress."));
                }
                let last = app.store.get("install_requested")?.as_i64().unwrap_or(0);
                if chrono::Utc::now().timestamp() - last < 60 {
                    return Err(error(
                        429,
                        "The update was already requested. Wait a minute before retrying.",
                    ));
                }
                if status["latest"] != requested {
                    return Err(error(409, "Check for updates again before installing."));
                }
                crate::updates::atomic(
                    &path.join("install.json"),
                    &json!({"nonce":random(16),"version":requested}),
                )?;
                app.store
                    .set(json!({"install_requested":chrono::Utc::now().timestamp()}))?;
            }
            Ok(success(json!({"ok":true})))
        }
        ("PUT", "/api/settings/email") => {
            let inbox = email(text(&body, "inbox", 1, 300)?)?;
            let input = optional_text(&body, "api_key", 200)?.trim();
            let key = if input.is_empty() {
                app.store.text("api_key")?
            } else {
                input.into()
            };
            if !key.starts_with("re_") {
                return Err(error(400, "Enter your Resend API key. It starts with re_."));
            }
            Resend::new(&key)?
                .inbox(None)
                .await
                .map_err(|e| error(400, &e.to_string()))?;
            let _gate = app.worker.gate.lock().await;
            let changed = inbox != app.store.text("inbox")? || key != app.store.text("api_key")?;
            let mut settings = json!({"api_key":key,"inbox":inbox,"sync_error":null});
            if changed {
                app.store.db(|db|{db.execute("UPDATE jobs SET status='cancelled',reason='Receiving address changed',updated_at=? WHERE status IN ('queued','preparing')",[now()])?;Ok(())})?;
                settings["receive_since"] = json!(now());
                for name in ["last_email_id", "scan_after", "scan_head"] {
                    settings[name] = json!("");
                }
            }
            app.store.set(settings)?;
            Ok(success(
                json!({"ok":true,"message":"Resend connected. New mail will be checked every 30 seconds."}),
            ))
        }
        ("GET" | "HEAD", "/api/printers/discover") => {
            let found = if app.demo {
                vec![app.store.get("printer")?]
            } else {
                printers::discover().await.map_err(|_| {
                    error(
                        400,
                        "Printer discovery is unavailable. You can add one by IP address.",
                    )
                })?
            };
            let message = if app.demo {
                "Preview printer"
            } else if found.is_empty() {
                "No printers found. You can add one by IP address."
            } else {
                ""
            };
            Ok(success(json!({"printers":found,"message":message})))
        }
        ("PUT", "/api/printer") => {
            let name = text(&body, "name", 1, 100)?.trim();
            if name.is_empty() {
                return Err(error(422, "Enter a printer name."));
            }
            let uri = text(&body, "uri", 1, 300)?;
            let printer = printers::pair(name, uri)
                .await
                .map_err(|e| error(400, &e.to_string()))?;
            let _gate = app.worker.gate.lock().await;
            app.store.set(json!({"printer":printer}))?;
            Ok(success(json!({"ok":true})))
        }
        ("POST", "/api/printer/test") => {
            let _gate = app.worker.gate.lock().await;
            if app.store.get("printer")?.is_null() {
                return Err(error(400, "Choose a printer first."));
            }
            if !app.store.rows("SELECT id FROM jobs WHERE sender='Paperboy test' AND status IN ('queued','preparing','submitting','submitted')",&[])?.is_empty(){return Err(error(409,"A test page is already in the queue."));}
            let id = uuid::Uuid::new_v4().to_string();
            app.store.db(|db|{db.execute("INSERT INTO jobs (id,email_id,attachment_id,sender,filename,status,created_at,updated_at) VALUES (?,'test',?,'Paperboy test','Paperboy test page','queued',?,?)",params![id,id,now(),now()])?;Ok(())})?;
            app.worker.wake.notify_one();
            Ok(success(json!({"ok":true})))
        }
        ("POST", "/api/senders") => {
            let address = email(text(&body, "email", 1, 300)?)?;
            let name = text(&body, "name", 1, 80)?.trim();
            if name.is_empty() {
                return Err(error(422, "Enter a name."));
            }
            let _gate = app.worker.gate.lock().await;
            if app.store.allowed(&address)? {
                return Err(error(409, "This address is already approved."));
            }
            app.store.db(|db| {
                db.execute(
                    "INSERT INTO senders VALUES (?,?,?)",
                    params![address, name, now()],
                )?;
                Ok(())
            })?;
            Ok(success(json!({"ok":true})))
        }
        ("DELETE", path) if path.starts_with("/api/senders/") => {
            let address = percent_encoding::percent_decode_str(&path[13..])
                .decode_utf8()
                .map_err(|_| error(422, "Enter a valid email address."))?;
            let address = email(&address)?;
            let _gate = app.worker.gate.lock().await;
            app.store.db(|db|{let tx=db.transaction()?;tx.execute("DELETE FROM senders WHERE email=?",[&address])?;tx.execute("UPDATE jobs SET status='blocked',reason='Sender was removed before printing',updated_at=? WHERE sender=? AND status IN ('queued','preparing')",params![now(),address])?;tx.commit()?;Ok(())})?;
            Ok(success(json!({"ok":true})))
        }
        ("PUT", "/api/settings/preferences") => {
            app.store.set(preferences(&body)?)?;
            Ok(success(json!({"ok":true})))
        }
        ("POST", "/api/setup/complete") => {
            let _gate = app.worker.gate.lock().await;
            if app.store.text("api_key")?.is_empty()
                || app.store.text("inbox")?.is_empty()
                || app.store.get("printer")?.is_null()
                || app.store.senders()?.is_empty()
            {
                return Err(error(
                    400,
                    "Connect Resend, choose a printer, and add an approved sender first.",
                ));
            }
            if !app.store.flag("setup_complete")? {
                app.store
                    .set(json!({"setup_complete":true,"receive_since":now()}))?;
            }
            app.worker.wake.notify_one();
            Ok(success(json!({"ok":true})))
        }
        ("PUT", "/api/printing") => {
            let paused = body["paused"]
                .as_bool()
                .ok_or_else(|| error(422, "Choose whether printing is paused."))?;
            let _gate = app.worker.gate.lock().await;
            app.store.set(json!({"paused":paused}))?;
            app.worker.wake.notify_one();
            Ok(success(json!({"ok":true})))
        }
        ("POST", "/api/sync") => {
            if !app.store.flag("setup_complete")? {
                return Err(error(400, "Finish setup before checking mail."));
            }
            app.worker.sync().await?;
            let failure = app.store.get("sync_error")?;
            Ok(success(json!({"ok":failure.is_null(),"error":failure})))
        }
        ("POST", path) if path.starts_with("/api/jobs/") => {
            let parts: Vec<_> = path[10..].split('/').collect();
            if parts.len() != 2 {
                return Err(error(404, "Job not found."));
            }
            let id = parts[0];
            let action = parts[1];
            let _gate = app.worker.gate.lock().await;
            let job = app
                .store
                .job(id)?
                .ok_or_else(|| error(404, "Job not found."))?;
            match action {
                "cancel" => {
                    if job["status"] == "submitted" {
                        printers::cancel(
                            job["cups_id"].as_i64().unwrap_or(0) as i32,
                            job["printer_queue"].as_str().unwrap_or(""),
                        )
                        .await
                        .map_err(|_| {
                            error(
                                400,
                                "The print service could not cancel this job. Check the printer.",
                            )
                        })?;
                    } else if !["queued", "preparing"]
                        .contains(&job["status"].as_str().unwrap_or(""))
                    {
                        return Err(error(409, "This job cannot be cancelled."));
                    }
                    app.store.update_job(
                        id,
                        json!({"status":"cancelled","reason":"Cancelled in Paperboy"}),
                    )?;
                }
                "retry" => {
                    if job["status"] != "failed"
                        || !job["cups_id"].is_null()
                        || !job["printer_queue"].is_null()
                    {
                        return Err(error(
                            409,
                            "Only a file that failed before delivery can be retried.",
                        ));
                    }
                    if job["sender"] != "Paperboy test"
                        && !app.store.allowed(job["sender"].as_str().unwrap_or(""))?
                    {
                        return Err(error(403, "This sender is no longer approved."));
                    }
                    app.store
                        .update_job(id, json!({"status":"queued","reason":null}))?;
                    app.worker.wake.notify_one();
                }
                _ => return Err(error(404, "Job not found.")),
            }
            Ok(success(json!({"ok":true})))
        }
        _ => Err(error(404, "This page could not be found.")),
    }
}
fn seed_demo(store: &Store) -> Result<()> {
    store.set(json!({"inbox":"print@home.resend.app","api_key":"preview","setup_complete":true,"receive_since":now(),"last_sync":now(),"printer":{"name":"Brother HL-L2460DW","uri":"ipp://192.168.1.42:631/ipp/print","queue":"preview"}}))?;
    store.db(|db|{
        let tx=db.transaction()?;
        for (address,name) in [("alex@example.com","Alex"),("sam@example.com","Sam"),("jordan@example.com","Jordan")] {tx.execute("INSERT OR IGNORE INTO senders VALUES (?,?,?)",params![address,name,now()])?;}
        if !tx.query_row("SELECT EXISTS(SELECT 1 FROM jobs)",[],|r|r.get::<_,bool>(0))? {
            for (filename,sender,pages) in [("Weekend itinerary.pdf","alex@example.com",3),("School permission slip.docx","sam@example.com",1),("Family photo.jpg","jordan@example.com",1)] {
                let id=uuid::Uuid::new_v4().to_string();tx.execute("INSERT INTO jobs (id,email_id,attachment_id,sender,filename,status,pages,created_at,updated_at) VALUES (?,?,?,?,?,'completed',?,?,?)",params![id,id,id,sender,filename,pages,now(),now()])?;
            }
        }tx.commit()?;Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;
    async fn request(
        app: &Arc<App>,
        method: &str,
        path: &str,
        body: Value,
        auth: Option<&(String, String)>,
    ) -> (StatusCode, HeaderMap, Value) {
        let mut request = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("host", "localhost")
            .header("content-type", "application/json");
        if let Some((cookie, csrf)) = auth {
            request = request
                .header("cookie", cookie)
                .header("x-paperboy-csrf", csrf);
        }
        let response = app
            .clone()
            .router(Path::new("web"))
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (
            status,
            headers,
            serde_json::from_slice(&body).unwrap_or(Value::Null),
        )
    }
    fn fixture() -> (tempfile::TempDir, Arc<App>) {
        let dir = tempfile::tempdir().unwrap();
        let app = App::open(dir.path(), "unused".into(), false, false).unwrap();
        (dir, app)
    }
    #[tokio::test]
    async fn update_controls_require_owner_and_do_not_grant_docker_access() {
        let (_dir, app) = fixture();
        for (method, path) in [
            ("GET", "/api/updates"),
            ("POST", "/api/updates/install"),
            ("PUT", "/api/updates/policy"),
        ] {
            assert_eq!(
                request(&app, method, path, json!({}), None).await.0,
                StatusCode::UNAUTHORIZED
            );
        }
        let auth = own(&app).await;
        let updates = request(&app, "GET", "/api/updates", json!({}), Some(&auth)).await;
        assert_eq!(updates.0, StatusCode::OK);
        assert_eq!(updates.2["managed"], false);
        assert_eq!(updates.2["current"], env!("CARGO_PKG_VERSION"));
        assert_eq!(
            request(
                &app,
                "POST",
                "/api/updates/install",
                json!({"version":"0.4.0"}),
                Some(&auth)
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            request(
                &app,
                "PUT",
                "/api/updates/policy",
                json!({"automatic":true}),
                Some(&auth)
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        let bad = (auth.0.clone(), "bad-csrf".into());
        assert_eq!(
            request(
                &app,
                "PUT",
                "/api/updates/policy",
                json!({"automatic":true}),
                Some(&bad)
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    async fn own(app: &Arc<App>) -> (String, String) {
        let (status, headers, body) = request(
            app,
            "POST",
            "/api/auth/setup",
            json!({"password":"test-owner-password","setup_code":app.bootstrap}),
            None,
        )
        .await;
        assert_eq!(status, 200);
        (
            headers[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .into(),
            body["csrf"].as_str().unwrap().into(),
        )
    }
    #[tokio::test]
    async fn setup_login_logout_and_secrets_remain_private() {
        let (_dir, app) = fixture();
        assert_eq!(
            request(&app, "GET", "/api/state", json!({}), None).await.0,
            401
        );
        assert_eq!(
            request(
                &app,
                "POST",
                "/api/auth/setup",
                json!({"password":"long-password","setup_code":"wrong"}),
                None
            )
            .await
            .0,
            403
        );
        let auth = own(&app).await;
        assert!(!app.store.root.join("setup.code").exists());
        assert_eq!(
            request(
                &app,
                "POST",
                "/api/auth/setup",
                json!({"password":"long-password","setup_code":app.bootstrap}),
                None
            )
            .await
            .0,
            409
        );
        app.store
            .set(json!({"api_key":"re_secret_test_only"}))
            .unwrap();
        let state = request(&app, "GET", "/api/state", json!({}), Some(&auth)).await;
        assert_eq!(state.0, 200);
        assert_eq!(state.2["settings"]["api_key_set"], true);
        assert!(!state.2.to_string().contains("re_secret_test_only"));
        assert_eq!(state.1["cache-control"], "no-store");
        assert_eq!(state.1["x-frame-options"], "DENY");
        let stored = app.store.rows("SELECT token FROM sessions", &[]).unwrap();
        assert!(!json!(stored).to_string().contains(&auth.0[17..]));
        assert_eq!(
            request(&app, "POST", "/api/auth/logout", json!({}), Some(&auth))
                .await
                .0,
            200
        );
        assert_eq!(
            request(&app, "GET", "/api/state", json!({}), Some(&auth))
                .await
                .0,
            401
        );
        assert_eq!(
            request(
                &app,
                "POST",
                "/api/auth/login",
                json!({"password":"wrong-password"}),
                None
            )
            .await
            .0,
            401
        );
        assert_eq!(
            request(
                &app,
                "POST",
                "/api/auth/login",
                json!({"password":"test-owner-password"}),
                None
            )
            .await
            .0,
            200
        );
    }
    #[tokio::test]
    async fn concurrent_claims_cannot_replace_owner() {
        let (_dir, app) = fixture();
        let body = json!({"password":"test-owner-password","setup_code":app.bootstrap});
        let (first, second) = tokio::join!(
            request(&app, "POST", "/api/auth/setup", body.clone(), None),
            request(&app, "POST", "/api/auth/setup", body, None)
        );
        let mut statuses = [first.0.as_u16(), second.0.as_u16()];
        statuses.sort();
        assert_eq!(statuses, [200, 409]);
    }
    #[tokio::test]
    async fn writes_require_csrf_and_same_origin() {
        let (_dir, app) = fixture();
        let auth = own(&app).await;
        let body = json!({"email":"alex@example.com","name":"Alex"});
        let no_csrf = (auth.0.clone(), String::new());
        assert_eq!(
            request(&app, "POST", "/api/senders", body.clone(), Some(&no_csrf))
                .await
                .0,
            403
        );
        for (name, value) in [
            ("origin", "https://attacker.example"),
            ("sec-fetch-site", "cross-site"),
        ] {
            let response = app
                .clone()
                .router(Path::new("web"))
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri("/api/senders")
                        .header("host", "localhost")
                        .header(name, value)
                        .header("cookie", &auth.0)
                        .header("x-paperboy-csrf", &auth.1)
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), 403);
            assert_eq!(response.headers()["x-frame-options"], "DENY");
        }
        assert_eq!(
            request(&app, "POST", "/api/senders", body, Some(&auth))
                .await
                .0,
            200
        );
    }
    #[tokio::test]
    async fn validation_and_chunked_limits_do_not_echo_credentials() {
        let (_dir, app) = fixture();
        for body in [
            json!({"password":"SECRET"}),
            json!({"password":{"secret":"SECRET"}}),
        ] {
            let response = request(&app, "POST", "/api/auth/login", body, None).await;
            assert_eq!(response.0, 422);
            assert!(!response.2.to_string().contains("SECRET"));
        }
        let chunks = futures_util::stream::iter(vec![
            Ok::<_, std::io::Error>(Bytes::from(vec![b'x'; 10000])),
            Ok(Bytes::from(vec![b'x'; 10000])),
        ]);
        let response = app
            .clone()
            .router(Path::new("web"))
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/auth/login")
                    .body(Body::from_stream(chunks))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 413);
    }
    #[tokio::test]
    async fn sign_in_attempts_are_bounded() {
        let (_dir, app) = fixture();
        for _ in 0..15 {
            assert_eq!(
                request(
                    &app,
                    "POST",
                    "/api/auth/login",
                    json!({"password":"wrong-password"}),
                    None
                )
                .await
                .0,
                401
            );
        }
        assert_eq!(
            request(
                &app,
                "POST",
                "/api/auth/login",
                json!({"password":"wrong-password"}),
                None
            )
            .await
            .0,
            429
        );
    }
    #[tokio::test]
    async fn approved_addresses_are_exact_and_removal_blocks_queued_mail() {
        let (_dir, app) = fixture();
        let auth = own(&app).await;
        assert_eq!(
            request(
                &app,
                "POST",
                "/api/senders",
                json!({"email":"  ALEX@Example.com ","name":"Alex"}),
                Some(&auth)
            )
            .await
            .0,
            200
        );
        assert!(app.store.allowed("alex@example.com").unwrap());
        assert!(!app.store.allowed("alex+other@example.com").unwrap());
        assert_eq!(
            request(
                &app,
                "POST",
                "/api/senders",
                json!({"email":"alex@example.com","name":"Alex"}),
                Some(&auth)
            )
            .await
            .0,
            409
        );
        assert_eq!(
            request(
                &app,
                "POST",
                "/api/senders",
                json!({"email":"Alex <alex@example.com>","name":"Alex"}),
                Some(&auth)
            )
            .await
            .0,
            422
        );
        app.worker
            .enqueue(
                &json!({"id":"email","from":"alex@example.com"}),
                vec![json!({"id":"file","filename":"family.pdf"})],
            )
            .unwrap();
        assert_eq!(
            request(
                &app,
                "DELETE",
                "/api/senders/alex%40example.com",
                json!({}),
                Some(&auth)
            )
            .await
            .0,
            200
        );
        assert!(!app.store.allowed("alex@example.com").unwrap());
        assert_eq!(app.store.jobs().unwrap()[0]["status"], "blocked");
    }
    #[tokio::test]
    async fn limits_pause_and_setup_are_explicit() {
        let (_dir, app) = fixture();
        let auth = own(&app).await;
        assert_eq!(
            request(&app, "POST", "/api/setup/complete", json!({}), Some(&auth))
                .await
                .0,
            400
        );
        assert_eq!(
            request(&app, "POST", "/api/sync", json!({}), Some(&auth))
                .await
                .0,
            400
        );
        for body in [
            json!({"max_pages":101}),
            json!({"max_size_mb":26}),
            json!({"daily_limit":0}),
            json!({"paper":"Legal"}),
            json!({"sides":"invalid"}),
            json!({"retention_days":91}),
            json!({"max_pages":true}),
        ] {
            assert_eq!(
                request(&app, "PUT", "/api/settings/preferences", body, Some(&auth))
                    .await
                    .0,
                422
            );
        }
        assert_eq!(
            request(
                &app,
                "PUT",
                "/api/settings/preferences",
                json!({"paper":"A4","max_pages":2,"daily_limit":1}),
                Some(&auth)
            )
            .await
            .0,
            200
        );
        assert_eq!(app.store.get("paper").unwrap(), "A4");
        assert_eq!(
            request(
                &app,
                "PUT",
                "/api/printing",
                json!({"paused":true}),
                Some(&auth)
            )
            .await
            .0,
            200
        );
        assert!(app.store.flag("paused").unwrap());
        assert_eq!(
            request(
                &app,
                "PUT",
                "/api/printing",
                json!({"paused":"true"}),
                Some(&auth)
            )
            .await
            .0,
            422
        );
    }
    #[tokio::test]
    async fn unsafe_retry_and_duplicate_test_pages_are_blocked() {
        let (_dir, app) = fixture();
        let auth = own(&app).await;
        assert_eq!(
            request(&app, "POST", "/api/printer/test", json!({}), Some(&auth))
                .await
                .0,
            400
        );
        app.store
            .set(json!({"printer":{"queue":"paperboy_012345abcdef"}}))
            .unwrap();
        assert_eq!(
            request(&app, "POST", "/api/printer/test", json!({}), Some(&auth))
                .await
                .0,
            200
        );
        assert_eq!(
            request(&app, "POST", "/api/printer/test", json!({}), Some(&auth))
                .await
                .0,
            409
        );
        let id = app.store.jobs().unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            request(
                &app,
                "POST",
                &format!("/api/jobs/{id}/cancel"),
                json!({}),
                Some(&auth)
            )
            .await
            .0,
            200
        );
        assert_eq!(app.store.job(&id).unwrap().unwrap()["status"], "cancelled");
        for values in [
            json!({"status":"uncertain"}),
            json!({"status":"failed","cups_id":42}),
            json!({"status":"failed","cups_id":null,"printer_queue":"paperboy_012345abcdef"}),
        ] {
            app.store.update_job(&id, values).unwrap();
            assert_eq!(
                request(
                    &app,
                    "POST",
                    &format!("/api/jobs/{id}/retry"),
                    json!({}),
                    Some(&auth)
                )
                .await
                .0,
                409
            );
        }
        app.store
            .update_job(
                &id,
                json!({"status":"failed","cups_id":null,"printer_queue":null}),
            )
            .unwrap();
        assert_eq!(
            request(
                &app,
                "POST",
                &format!("/api/jobs/{id}/retry"),
                json!({}),
                Some(&auth)
            )
            .await
            .0,
            200
        );
        app.store
            .update_job(&id, json!({"status":"failed"}))
            .unwrap();
        app.store
            .db(|db| {
                db.execute(
                    "UPDATE jobs SET sender='removed@example.com' WHERE id=?",
                    [&id],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            request(
                &app,
                "POST",
                &format!("/api/jobs/{id}/retry"),
                json!({}),
                Some(&auth)
            )
            .await
            .0,
            403
        );
    }
    #[tokio::test]
    async fn preview_is_read_only_and_cannot_overwrite_live_data() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::open(dir.path(), "unused".into(), true, false).unwrap();
        assert_eq!(
            request(&app, "GET", "/api/state", json!({}), None).await.2["demo"],
            true
        );
        assert_eq!(
            request(&app, "PUT", "/api/printing", json!({"paused":true}), None)
                .await
                .0,
            403
        );
        assert_eq!(
            request(
                &app,
                "POST",
                "/api/auth/setup",
                json!({"password":"long-password"}),
                None
            )
            .await
            .0,
            409
        );
        app.store
            .set(json!({"api_key":"re_live_configuration"}))
            .unwrap();
        assert!(App::open(dir.path(), "unused".into(), true, false).is_err());
        assert_eq!(app.store.text("api_key").unwrap(), "re_live_configuration");
    }
    #[tokio::test]
    async fn python_password_and_saved_sessions_remain_compatible() {
        let (_dir, app) = fixture();
        let expected = "037f75ed18d778b478ea7e279c0d9b09d1357a4f51234bd8afde8f6192f0ff4bfd9c35aec06816447357a217811212c41d7b8d0b9ac2778ec99ed1ccea1c95db";
        let salt = "00112233445566778899aabbccddeeff";
        app.store
            .set(json!({"password_salt":salt,"password_hash":expected}))
            .unwrap();
        assert_eq!(
            request(
                &app,
                "POST",
                "/api/auth/login",
                json!({"password":"legacy-owner-password"}),
                None
            )
            .await
            .0,
            200
        );
        app.store
            .db(|db| {
                db.execute(
                    "INSERT INTO sessions VALUES (?,?,?)",
                    params![
                        digest("python-session-token"),
                        "python-csrf",
                        epoch() + 100.0
                    ],
                )?;
                Ok(())
            })
            .unwrap();
        let auth = (
            "paperboy_session=python-session-token".into(),
            "python-csrf".into(),
        );
        assert_eq!(
            request(&app, "GET", "/api/state", json!({}), Some(&auth))
                .await
                .0,
            200
        );
    }
}
