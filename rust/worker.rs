use crate::{
    conversion::SUPPORTED,
    printers,
    resend::{self, Resend},
    store::{Store, now},
};
use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Notify};

pub struct Worker {
    pub store: Arc<Store>,
    pub gate: Mutex<()>,
    pub wake: Notify,
    poll: Mutex<()>,
    socket: String,
}
impl Worker {
    pub fn new(store: Arc<Store>, socket: String) -> Self {
        Self {
            store,
            gate: Mutex::new(()),
            wake: Notify::new(),
            poll: Mutex::new(()),
            socket,
        }
    }
    pub fn recover(&self) -> Result<()> {
        let spool = self.store.root.join("spool");
        if spool.exists() {
            std::fs::remove_dir_all(spool)?;
        }
        self.store.db(|db|{let tx=db.transaction()?;tx.execute("UPDATE jobs SET status='queued' WHERE status='preparing'",[])?;tx.execute("UPDATE jobs SET status='uncertain',reason='Paperboy restarted during delivery. Check the printer before sending again.' WHERE status='submitting'",[])?;tx.commit()?;Ok(())})
    }
    pub async fn run(self: Arc<Self>) {
        let mut next_poll = Instant::now();
        loop {
            let result = async {
                if self.store.flag("setup_complete")? {
                    if Instant::now() >= next_poll {
                        self.sync().await?;
                        next_poll = Instant::now() + Duration::from_secs(30);
                    }
                    self.process_one().await?;
                    self.reconcile().await?;
                    self.cleanup()?;
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            if result.is_err() {
                eprintln!("A background operation failed; it will be checked again.");
            }
            tokio::select! {_=tokio::time::sleep(Duration::from_secs(2))=>{},_=self.wake.notified()=>{next_poll=Instant::now();}}
        }
    }
    fn record(&self, email: &Value, status: &str, reason: &str) -> Result<()> {
        let id = email["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Invalid email identifier."))?;
        let sender = resend::address(email["from"].as_str().unwrap_or(""));
        let subject = email["subject"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(200)
            .collect::<String>();
        self.store.db(|db| {
            db.execute(
                "INSERT OR IGNORE INTO messages VALUES (?,?,?,?,?,?)",
                params![id, sender, subject, status, reason, now()],
            )?;
            Ok(())
        })
    }
    pub fn precheck(&self, email: &Value) -> Result<bool> {
        let id = email["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Invalid email identifier."))?;
        if self.store.db(|db| {
            Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE id=?)",
                [id],
                |r| r.get::<_, bool>(0),
            )?)
        })? {
            return Ok(false);
        }
        let target = self.store.text("inbox")?;
        if !recipients(email).contains(&target) {
            self.record(email, "ignored", "Different receiving address")?;
            return Ok(false);
        }
        let sender = resend::address(email["from"].as_str().unwrap_or(""));
        if !self.store.allowed(&sender)? {
            self.record(email, "blocked", "Sender is not approved")?;
            return Ok(false);
        }
        Ok(true)
    }
    pub fn authenticated_details(&self, email: &Value, full: &Value) -> Result<bool> {
        let sender = resend::address(email["from"].as_str().unwrap_or(""));
        if full["id"] != email["id"]
            || resend::address(full["from"].as_str().unwrap_or("")) != sender
            || !recipients(full).contains(&self.store.text("inbox")?)
        {
            self.record(email, "blocked", "Email details did not match")?;
            return Ok(false);
        }
        if !resend::authenticated(full) {
            self.record(email, "blocked", "Sender authentication did not pass")?;
            return Ok(false);
        }
        Ok(true)
    }
    pub fn enqueue(&self, email: &Value, attachments: Vec<Value>) -> Result<()> {
        let attachments: Vec<_> = attachments
            .into_iter()
            .filter(|a| a["content_disposition"] != "inline")
            .collect();
        if attachments.is_empty() {
            return self.record(email, "ignored", "No file attachments");
        }
        if attachments.len() > 10 {
            return self.record(email, "blocked", "Send 10 or fewer attachments per email");
        }
        let id = email["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Invalid email identifier."))?;
        let sender = resend::address(email["from"].as_str().unwrap_or(""));
        if !self.store.allowed(&sender)? {
            return self.record(email, "blocked", "Sender was removed before processing");
        }
        let cutoff = (Utc::now() - chrono::Duration::days(1)).to_rfc3339();
        let daily = self.store.number("daily_limit")?;
        let size = self.store.number("max_size_mb")? * 1024 * 1024;
        let subject = email["subject"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(200)
            .collect::<String>();
        // IDs and counts enter the same durable transaction, including duplicate checks.
        self.store.db(|db| {
            let tx=db.transaction()?;
            if tx.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE id=?)",[id],|r|r.get::<_,bool>(0))? {return Ok(());}
            let used:i64=tx.query_row("SELECT COUNT(*) FROM jobs WHERE sender=? AND created_at>=?",params![sender,cutoff],|r|r.get(0))?;
            if used+attachments.len() as i64>daily {
                tx.execute("INSERT OR IGNORE INTO messages VALUES (?,?,?,?,?,?)",params![id,sender,subject,"blocked","Sender's daily file limit reached",now()])?;tx.commit()?;return Ok(());
            }
            for a in attachments {
                let attachment=a["id"].as_str().ok_or_else(||anyhow!("Invalid attachment identifier."))?;
                let filename=Path::new(a["filename"].as_str().unwrap_or("attachment")).file_name().unwrap_or_default().to_string_lossy().chars().take(180).collect::<String>();
                let extension=extension(&filename);
                let (status,reason)=if !SUPPORTED.contains(&extension.as_str()) {("failed",Some("Unsupported file type. Send a PDF, document, or image."))} else if a["size"].as_i64().unwrap_or(0)>size {("failed",Some("Attachment exceeds your file size limit."))} else {("queued",None)};
                tx.execute("INSERT OR IGNORE INTO jobs (id,email_id,attachment_id,sender,filename,status,reason,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?)",params![uuid::Uuid::new_v4().to_string(),id,attachment,sender,filename,status,reason,now(),now()])?;
            }
            tx.execute("INSERT OR IGNORE INTO messages VALUES (?,?,?,?,?,?)",params![id,sender,subject,"accepted",Option::<String>::None,now()])?;
            tx.commit()?;Ok(())
        })
    }
    pub async fn sync(&self) -> Result<()> {
        let Ok(_poll) = self.poll.try_lock() else {
            return Ok(());
        };
        let _gate = self.gate.lock().await;
        let result = self.sync_inner().await;
        if let Err(error) = result {
            // Network/domain errors are sanitized by the Resend client. Database details never escape.
            let message = error.to_string();
            let safe = if message.starts_with("Resend ")
                || message == "Too many attachments in this message."
            {
                message
            } else {
                "Resend returned unexpected email data.".into()
            };
            self.store.set(json!({"sync_error":safe}))?;
        }
        Ok(())
    }
    async fn sync_inner(&self) -> Result<()> {
        let mut resend = Resend::new(&self.store.text("api_key")?)?;
        self.sync_from(&mut resend).await
    }
    async fn sync_from(&self, resend: &mut Resend) -> Result<()> {
        let cursor = self.store.text("last_email_id")?;
        let cutoff = timestamp(&self.store.text("receive_since")?)?;
        let mut after = self.store.text("scan_after")?;
        let mut newest = if after.is_empty() {
            String::new()
        } else {
            self.store.text("scan_head")?
        };
        let mut items = vec![];
        let mut finished = false;
        for _ in 0..20 {
            let page = resend
                .inbox(if after.is_empty() { None } else { Some(&after) })
                .await?;
            let data = page["data"]
                .as_array()
                .ok_or_else(|| anyhow!("Invalid inbox data."))?;
            if newest.is_empty() {
                newest = data
                    .first()
                    .and_then(|e| e["id"].as_str())
                    .unwrap_or("")
                    .into();
            }
            for email in data {
                let id = email["id"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Invalid inbox identifier."))?;
                if id == cursor || timestamp(email["created_at"].as_str().unwrap_or(""))? < cutoff {
                    finished = true;
                    break;
                }
                items.push(email.clone());
            }
            if finished || page["has_more"] != true || data.is_empty() {
                finished = true;
                break;
            }
            after = data
                .last()
                .and_then(|email| email["id"].as_str())
                .unwrap_or("")
                .into();
        }
        for email in items.iter().rev() {
            if !self.precheck(email)? {
                continue;
            }
            let full = resend.email(email["id"].as_str().unwrap()).await?;
            if !self.authenticated_details(email, &full)? {
                continue;
            }
            let attachments = resend.attachments(email["id"].as_str().unwrap()).await?;
            self.enqueue(email, attachments)?;
        }
        if !finished {
            self.store.set(json!({"scan_after":after,"scan_head":newest,"last_sync":now(),"sync_error":"Checking a large inbox backlog. More messages will follow."}))?;
        } else {
            let mut values =
                json!({"last_sync":now(),"sync_error":null,"scan_after":"","scan_head":""});
            if !newest.is_empty() {
                values["last_email_id"] = json!(newest);
            }
            self.store.set(values)?;
        }
        Ok(())
    }
    pub fn authorize_submission(&self, job: &Value, pages: i64, printer: &Value) -> Result<bool> {
        let id = job["id"].as_str().ok_or_else(|| anyhow!("Invalid job."))?;
        let Some(current) = self.store.job(id)? else {
            return Ok(false);
        };
        if current["status"] != "preparing" {
            return Ok(false);
        }
        if job["sender"] != "Paperboy test"
            && !self.store.allowed(job["sender"].as_str().unwrap_or(""))?
        {
            self.store.update_job(
                id,
                json!({"status":"blocked","reason":"Sender was removed before printing."}),
            )?;
            return Ok(false);
        }
        if self.store.flag("paused")? {
            self.store.update_job(id, json!({"status":"queued"}))?;
            return Ok(false);
        }
        self.store.update_job(
            id,
            json!({"status":"submitting","pages":pages,"printer_queue":printer["queue"]}),
        )?;
        Ok(true)
    }
    async fn convert_file(&self, source: &Path, output: &Path, settings: &Value) -> Result<i64> {
        let client = reqwest::Client::builder()
            .unix_socket(self.socket.clone())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(120))
            .build()?;
        let data = tokio::fs::read(source).await?;
        let mut response = client
            .post("http://converter/convert")
            .query(&[
                ("extension", extension(&source.to_string_lossy())),
                (
                    "paper",
                    settings["paper"].as_str().unwrap_or("Letter").to_owned(),
                ),
                ("max_pages", settings["max_pages"].to_string()),
            ])
            .body(data)
            .send()
            .await
            .map_err(|_| anyhow!("The converter is unavailable. Check the Docker service."))?;
        if response.status() != 200 {
            return Err(anyhow!(
                "This file could not be converted. Send an unlocked PDF or a different copy."
            ));
        }
        let pages = response
            .headers()
            .get("x-paperboy-pages")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|pages| *pages > 0 && *pages <= settings["max_pages"].as_i64().unwrap_or(50))
            .ok_or_else(|| anyhow!("The converted page count is invalid."))?;
        let bytes = resend::bounded_response(&mut response, 100 * 1024 * 1024).await?;
        if !bytes.starts_with(b"%PDF-") {
            return Err(anyhow!("The converter returned an invalid print file."));
        }
        tokio::fs::write(output, bytes).await?;
        Ok(pages)
    }
    pub async fn process_one(&self) -> Result<()> {
        if self.store.flag("paused")? || crate::updates::draining() {
            return Ok(());
        }
        let Some(job) = self
            .store
            .rows(
                "SELECT * FROM jobs WHERE status='queued' ORDER BY created_at LIMIT 1",
                &[],
            )?
            .into_iter()
            .next()
        else {
            return Ok(());
        };
        let id = job["id"].as_str().ok_or_else(|| anyhow!("Invalid job."))?;
        let printer = self.store.get("printer")?;
        if !["ready", "printing"].contains(
            &printers::status(&printer).await["state"]
                .as_str()
                .unwrap_or(""),
        ) {
            return Ok(());
        }
        {
            let _gate = self.gate.lock().await;
            if self.store.flag("paused")?
                || crate::updates::draining()
                || self
                    .store
                    .job(id)?
                    .is_none_or(|current| current["status"] != "queued")
            {
                return Ok(());
            }
            if job["sender"] != "Paperboy test"
                && !self.store.allowed(job["sender"].as_str().unwrap_or(""))?
            {
                self.store.update_job(
                    id,
                    json!({"status":"blocked","reason":"Sender was removed before printing."}),
                )?;
                return Ok(());
            }
            self.store
                .update_job(id, json!({"status":"preparing","reason":null}))?;
        }
        let result=async {
            let spool=self.store.root.join("spool");std::fs::create_dir_all(&spool)?;
            let work=tempfile::Builder::new().prefix("job-").tempdir_in(spool)?;
            let settings=self.store.settings()?;
            let output=work.path().join("print.pdf");
            let source=if job["sender"]=="Paperboy test" {let source=work.path().join("test.txt");tokio::fs::write(&source,format!("Paperboy\n\nYour printer is connected.\n\nEmail an attachment from an approved address to:\n{}",settings["inbox"].as_str().unwrap_or(""))).await?;source}
                else {
                    let mut resend=Resend::new(&self.store.text("api_key")?)?;
                    let attachments=resend.attachments(job["email_id"].as_str().unwrap_or("")).await?;
                    let attachment=attachments.into_iter().find(|a|a["id"]==job["attachment_id"]).ok_or_else(||anyhow!("The attachment is no longer available in Resend."))?;
                    let source=work.path().join(format!("source{}",extension(job["filename"].as_str().unwrap_or(""))));
                    resend::download(attachment["download_url"].as_str().unwrap_or(""),&source,(settings["max_size_mb"].as_u64().unwrap_or(25)*1024*1024) as usize).await?;source
                };
            let pages=self.convert_file(&source,&output,&settings).await?;
            let _gate=self.gate.lock().await;
            if !self.authorize_submission(&job,pages,&printer)?{return Ok::<_,anyhow::Error>(());}
            // The durable 'submitting' state precedes the single irreversible CUPS request.
            match printers::submit(&printer,&output,id,&settings).await {
                Ok(cups_id)=>self.store.update_job(id,json!({"status":"submitted","cups_id":cups_id}))?,
                Err(_)=>self.store.update_job(id,json!({"status":"uncertain","reason":"Delivery could not be confirmed. Check the printer before sending again."}))?,
            }
            Ok(())
        }.await;
        if result.is_err() {
            let _gate = self.gate.lock().await;
            if let Some(current) = self.store.job(id)?
                && ["preparing", "submitting"].contains(&current["status"].as_str().unwrap_or(""))
            {
                let uncertain = current["status"] == "submitting";
                self.store.update_job(id,json!({"status":if uncertain{"uncertain"}else{"failed"},"reason":if uncertain{"Check the printer before sending again."}else{"Processing failed. Try exporting the file as PDF."}}))?;
            }
        }
        Ok(())
    }
    async fn reconcile(&self) -> Result<()> {
        for job in self
            .store
            .rows("SELECT * FROM jobs WHERE status='submitted'", &[])?
        {
            let id = job["id"].as_str().unwrap_or("");
            let state = printers::job_state(
                job["cups_id"].as_i64().unwrap_or(0) as i32,
                job["printer_queue"].as_str().unwrap_or(""),
            )
            .await;
            let _gate = self.gate.lock().await;
            if self
                .store
                .job(id)?
                .is_none_or(|current| current["status"] != "submitted")
            {
                continue;
            }
            match state {
                Some(9)=>self.store.update_job(id,json!({"status":"completed"}))?,
                Some(7)=>self.store.update_job(id,json!({"status":"cancelled","reason":"Cancelled at the printer"}))?,
                Some(8)=>self.store.update_job(id,json!({"status":"failed","reason":"The printer stopped this job."}))?,
                None=>self.store.update_job(id,json!({"status":"uncertain","reason":"The print service no longer has a delivery record. Check the printer."}))?,
                _=>{}
            }
        }
        Ok(())
    }
    pub fn cleanup(&self) -> Result<()> {
        let cutoff = (Utc::now() - chrono::Duration::days(self.store.number("retention_days")?))
            .to_rfc3339();
        self.store.db(|db|{let tx=db.transaction()?;tx.execute("DELETE FROM jobs WHERE created_at<? AND status IN ('completed','cancelled','failed','blocked')",[&cutoff])?;tx.execute("UPDATE messages SET sender=NULL,subject=NULL WHERE created_at<?",[&cutoff])?;tx.commit()?;Ok(())})
    }
}
fn recipients(email: &Value) -> Vec<String> {
    email["to"]
        .as_array()
        .map(|v| {
            v.iter()
                .map(|v| resend::address(v.as_str().unwrap_or("")))
                .collect()
        })
        .unwrap_or_default()
}
fn extension(name: &str) -> String {
    format!(
        ".{}",
        Path::new(name)
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase()
    )
}
fn timestamp(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(&value.replace(' ', "T"))?.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn mail_server(
        items: Vec<Value>,
    ) -> (
        Resend,
        Arc<std::sync::Mutex<Vec<String>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use axum::{Json, Router, extract::State, http::Uri, routing::get};
        type Fixture = (Arc<Vec<Value>>, Arc<std::sync::Mutex<Vec<String>>>);
        async fn response(State((items, calls)): State<Fixture>, uri: Uri) -> Json<Value> {
            calls.lock().unwrap().push(uri.to_string());
            if uri.path() == "/emails/receiving" {
                let after = url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
                    .find(|(key, _)| key == "after")
                    .map(|(_, value)| value.into_owned());
                let start = after
                    .and_then(|id| {
                        items
                            .iter()
                            .position(|email| email["id"] == id)
                            .map(|index| index + 1)
                    })
                    .unwrap_or(0);
                return Json(
                    json!({"data":items[start..items.len().min(start+100)],"has_more":start+100<items.len()}),
                );
            }
            let attachments = uri.path().ends_with("/attachments");
            let id = uri
                .path()
                .trim_start_matches("/emails/receiving/")
                .trim_end_matches("/attachments");
            let id = percent_encoding::percent_decode_str(id)
                .decode_utf8()
                .unwrap();
            if attachments {
                return Json(
                    json!({"data":[{"id":"file","filename":"document.pdf","size":100}],"has_more":false}),
                );
            }
            let mut email = items
                .iter()
                .find(|email| email["id"] == id.as_ref())
                .unwrap()
                .clone();
            if email["authentication"].is_null() {
                email["authentication"] = json!({"dmarc":"pass"});
            }
            Json(email)
        }
        let calls = Arc::new(std::sync::Mutex::new(vec![]));
        let router = Router::new()
            .route("/emails/receiving", get(response))
            .route("/emails/receiving/{*path}", get(response))
            .with_state((Arc::new(items), calls.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (Resend::fixture(base), calls, server)
    }
    fn fixture() -> (tempfile::TempDir, Arc<Store>, Worker, Value) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        store.set(json!({"inbox":"print@home.resend.app"})).unwrap();
        store
            .db(|db| {
                db.execute(
                    "INSERT INTO senders VALUES ('alex@example.com','Alex',?)",
                    [now()],
                )?;
                Ok(())
            })
            .unwrap();
        let worker = Worker::new(store.clone(), "unused".into());
        let email = json!({"id":"email","from":"Alex <alex@example.com>","to":["print@home.resend.app"],"subject":"Family file","authentication":{"dmarc":"pass"}});
        (dir, store, worker, email)
    }
    #[test]
    fn approval_authentication_duplicate_and_limits_guard_queue() {
        let (_dir, store, worker, email) = fixture();
        assert!(worker.precheck(&email).unwrap());
        assert!(worker.authenticated_details(&email, &email).unwrap());
        let attachment = json!({"id":"attachment","filename":"../../family.pdf","size":50});
        worker.enqueue(&email, vec![attachment.clone()]).unwrap();
        worker.enqueue(&email, vec![attachment]).unwrap();
        assert_eq!(store.jobs().unwrap().len(), 1);
        assert_eq!(store.jobs().unwrap()[0]["filename"], "family.pdf");
        assert!(!worker.precheck(&email).unwrap());
        let mut wrong = email.clone();
        wrong["id"] = json!("wrong");
        wrong["from"] = json!("stranger@example.com");
        assert!(!worker.precheck(&wrong).unwrap());
        let mut forged = email.clone();
        forged["id"] = json!("forged");
        forged["authentication"] = json!({"dmarc":"fail","dkim":"pass"});
        assert!(!worker.authenticated_details(&forged, &forged).unwrap());
        store.set(json!({"daily_limit":1})).unwrap();
        let mut second = email;
        second["id"] = json!("second");
        worker
            .enqueue(
                &second,
                vec![json!({"id":"second-file","filename":"file.pdf"})],
            )
            .unwrap();
        assert_eq!(store.jobs().unwrap().len(), 1);
    }
    #[test]
    fn cancellation_pause_and_revocation_survive_conversion() {
        let (_dir, store, worker, email) = fixture();
        worker
            .enqueue(&email, vec![json!({"id":"file","filename":"file.pdf"})])
            .unwrap();
        let job = store.jobs().unwrap()[0].clone();
        let id = job["id"].as_str().unwrap();
        let printer = json!({"queue":"paperboy_012345abcdef"});
        store.update_job(id, json!({"status":"cancelled"})).unwrap();
        store.set(json!({"paused":true})).unwrap();
        assert!(!worker.authorize_submission(&job, 1, &printer).unwrap());
        assert_eq!(store.job(id).unwrap().unwrap()["status"], "cancelled");
        store.update_job(id, json!({"status":"preparing"})).unwrap();
        assert!(!worker.authorize_submission(&job, 1, &printer).unwrap());
        assert_eq!(store.job(id).unwrap().unwrap()["status"], "queued");
        store.update_job(id, json!({"status":"preparing"})).unwrap();
        store
            .db(|db| {
                db.execute("DELETE FROM senders", [])?;
                Ok(())
            })
            .unwrap();
        assert!(!worker.authorize_submission(&job, 1, &printer).unwrap());
        assert_eq!(store.job(id).unwrap().unwrap()["status"], "blocked");
    }
    #[test]
    fn interrupted_delivery_is_never_automatically_retried() {
        let (_dir, store, worker, email) = fixture();
        worker
            .enqueue(&email, vec![json!({"id":"file","filename":"file.pdf"})])
            .unwrap();
        let job = store.jobs().unwrap()[0].clone();
        store
            .update_job(
                job["id"].as_str().unwrap(),
                json!({"status":"submitting","printer_queue":"paperboy_012345abcdef"}),
            )
            .unwrap();
        worker.recover().unwrap();
        assert_eq!(store.jobs().unwrap()[0]["status"], "uncertain");
        assert!(store.jobs().unwrap()[0]["printer_queue"].is_string());
    }
    #[tokio::test]
    async fn polling_filters_before_fetch_and_rejects_failed_authentication() {
        let (_dir, store, worker, email) = fixture();
        let cutoff = Utc::now();
        store
            .set(json!({"receive_since":cutoff.to_rfc3339()}))
            .unwrap();
        let mut spam = email.clone();
        spam["id"] = json!("spam");
        spam["from"] = json!("stranger@example.com");
        let mut wrong = email.clone();
        wrong["id"] = json!("wrong");
        wrong["to"] = json!(["other@home.resend.app"]);
        let mut forged = email;
        forged["id"] = json!("forged");
        forged["authentication"] = json!({"dmarc":"fail","dkim":"pass"});
        let mut items = vec![spam, wrong, forged];
        for item in &mut items {
            item["created_at"] = json!((cutoff + chrono::Duration::seconds(1)).to_rfc3339());
        }
        let (mut mail, calls, server) = mail_server(items).await;
        worker.sync_from(&mut mail).await.unwrap();
        assert!(store.jobs().unwrap().is_empty());
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(
            calls
                .iter()
                .any(|call| call == "/emails/receiving/forged?html_format=cid")
        );
        assert!(!calls.iter().any(|call| call.ends_with("attachments")));
        server.abort();
    }
    #[tokio::test]
    async fn pagination_setup_cutoff_daily_limits_and_cleanup_keep_duplicates_blocked() {
        let (_dir, store, worker, email) = fixture();
        let cutoff = Utc::now();
        store
            .set(json!({"receive_since":cutoff.to_rfc3339(),"daily_limit":100}))
            .unwrap();
        let mut items: Vec<_> = (0..105)
            .map(|index| {
                let mut item = email.clone();
                item["id"] = json!(format!("email{index}"));
                item["created_at"] =
                    json!((cutoff + chrono::Duration::seconds(index + 1)).to_rfc3339());
                item
            })
            .collect();
        items.reverse();
        let head = items[0]["id"].clone();
        let mut old = email;
        old["id"] = json!("old");
        old["created_at"] = json!((cutoff - chrono::Duration::days(1)).to_rfc3339());
        items.push(old);
        let (mut mail, _, server) = mail_server(items).await;
        worker.sync_from(&mut mail).await.unwrap();
        assert_eq!(store.jobs().unwrap().len(), 100);
        assert_eq!(store.get("last_email_id").unwrap(), head);
        assert_eq!(
            store.rows("SELECT id FROM messages", &[]).unwrap().len(),
            105
        );
        assert!(
            store
                .rows("SELECT id FROM messages WHERE id='old'", &[])
                .unwrap()
                .is_empty()
        );
        let past = (cutoff - chrono::Duration::days(8)).to_rfc3339();
        store
            .db(|db| {
                db.execute("UPDATE jobs SET status='completed',created_at=?", [&past])?;
                db.execute("UPDATE messages SET created_at=?", [&past])?;
                Ok(())
            })
            .unwrap();
        worker.cleanup().unwrap();
        assert!(store.jobs().unwrap().is_empty());
        worker.sync_from(&mut mail).await.unwrap();
        assert!(store.jobs().unwrap().is_empty());
        assert_eq!(
            store
                .rows("SELECT id FROM messages WHERE sender IS NULL", &[])
                .unwrap()
                .len(),
            105
        );
        server.abort();
    }
    #[tokio::test]
    async fn large_backlog_resumes_from_saved_cursor() {
        let (_dir, store, worker, email) = fixture();
        let cutoff = Utc::now();
        store
            .set(json!({"receive_since":cutoff.to_rfc3339()}))
            .unwrap();
        let mut items: Vec<_> = (0..2100)
            .map(|index| {
                let mut item = email.clone();
                item["id"] = json!(format!("email{index}"));
                item["from"] = json!("stranger@example.com");
                item["created_at"] =
                    json!((cutoff + chrono::Duration::seconds(index + 1)).to_rfc3339());
                item
            })
            .collect();
        items.reverse();
        let continuation = items[1999]["id"].clone();
        let head = items[0]["id"].clone();
        let (mut mail, calls, server) = mail_server(items).await;
        worker.sync_from(&mut mail).await.unwrap();
        assert_eq!(store.get("scan_after").unwrap(), continuation);
        assert_eq!(store.text("last_email_id").unwrap(), "");
        worker.sync_from(&mut mail).await.unwrap();
        assert_eq!(store.get("last_email_id").unwrap(), head);
        assert_eq!(store.text("scan_after").unwrap(), "");
        assert!(store.get("sync_error").unwrap().is_null());
        assert_eq!(
            store.rows("SELECT id FROM messages", &[]).unwrap().len(),
            2100
        );
        assert_eq!(calls.lock().unwrap().len(), 21);
        server.abort();
    }
    #[test]
    fn inline_and_excessive_attachments_never_enter_the_queue() {
        let (_dir, store, worker, email) = fixture();
        worker
            .enqueue(
                &email,
                vec![
                    json!({"id":"signature","filename":"logo.png","content_disposition":"inline"}),
                ],
            )
            .unwrap();
        assert!(store.jobs().unwrap().is_empty());
        let mut second = email.clone();
        second["id"] = json!("too-many");
        worker
            .enqueue(
                &second,
                (0..11)
                    .map(|id| json!({"id":format!("file{id}"),"filename":"document.pdf"}))
                    .collect(),
            )
            .unwrap();
        assert!(store.jobs().unwrap().is_empty());
        let mut third = email;
        third["id"] = json!("bad-files");
        worker
            .enqueue(
                &third,
                vec![
                    json!({"id":"executable","filename":"unsafe.exe"}),
                    json!({"id":"oversized","filename":"huge.pdf","size":30*1024*1024}),
                ],
            )
            .unwrap();
        assert_eq!(store.jobs().unwrap().len(), 2);
        assert!(
            store
                .jobs()
                .unwrap()
                .iter()
                .all(|job| job["status"] == "failed")
        );
    }
}
