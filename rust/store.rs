use anyhow::{Result, anyhow};
use fernet::Fernet;
use rusqlite::{Connection, Row, params, types::ValueRef};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
};

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
}
pub fn defaults() -> Value {
    json!({"inbox":"","printer":null,"setup_complete":false,"paused":false,"paper":"Letter","color":"monochrome","sides":"one-sided","max_pages":50,"max_size_mb":25,"daily_limit":20,"retention_days":7,"receive_since":"","last_email_id":"","last_sync":null,"sync_error":null})
}

pub struct Store {
    pub root: PathBuf,
    connection: Mutex<Connection>,
    cipher: Fernet,
}
impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        let key_path = root.join("encryption.key");
        if !key_path.exists() {
            use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&key_path)?;
            file.write_all(Fernet::generate_key().as_bytes())?;
            file.sync_all()?;
        }
        let cipher = Fernet::new(fs::read_to_string(key_path)?.trim())
            .ok_or_else(|| anyhow!("The saved encryption key is invalid."))?;
        let path = root.join("paperboy.sqlite3");
        let connection = Connection::open(&path)?;
        connection.busy_timeout(std::time::Duration::from_secs(15))?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS senders (email TEXT PRIMARY KEY,name TEXT NOT NULL,created_at TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS messages (id TEXT PRIMARY KEY,sender TEXT,subject TEXT,status TEXT,reason TEXT,created_at TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS jobs (id TEXT PRIMARY KEY,email_id TEXT NOT NULL,attachment_id TEXT NOT NULL,sender TEXT NOT NULL,filename TEXT NOT NULL,status TEXT NOT NULL,reason TEXT,pages INTEGER,cups_id INTEGER,printer_queue TEXT,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,UNIQUE(email_id,attachment_id));
            CREATE TABLE IF NOT EXISTS sessions (token TEXT PRIMARY KEY,csrf TEXT NOT NULL,expires REAL NOT NULL);")?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            root: root.into(),
            connection: Mutex::new(connection),
            cipher,
        })
    }
    pub fn db<T>(&self, action: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut db = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Database access was interrupted."))?;
        action(&mut db)
    }
    pub fn get(&self, key: &str) -> Result<Value> {
        let value = self.db(|db| {
            use rusqlite::OptionalExtension;
            let stored: Option<String> = db
                .query_row("SELECT value FROM settings WHERE key=?", [key], |row| {
                    row.get(0)
                })
                .optional()?;
            Ok(stored
                .map(|text| serde_json::from_str(&text))
                .transpose()?
                .unwrap_or_else(|| defaults().get(key).cloned().unwrap_or(Value::Null)))
        })?;
        if key == "api_key" && value.as_str().is_some_and(|s| !s.is_empty()) {
            let bytes = self
                .cipher
                .decrypt(value.as_str().unwrap())
                .map_err(|_| anyhow!("The saved API key could not be decrypted."))?;
            return Ok(Value::String(String::from_utf8(bytes)?));
        }
        Ok(value)
    }
    pub fn text(&self, key: &str) -> Result<String> {
        Ok(self.get(key)?.as_str().unwrap_or("").to_owned())
    }
    pub fn flag(&self, key: &str) -> Result<bool> {
        Ok(self.get(key)?.as_bool().unwrap_or(false))
    }
    pub fn number(&self, key: &str) -> Result<i64> {
        self.get(key)?
            .as_i64()
            .ok_or_else(|| anyhow!("Invalid saved setting."))
    }
    pub fn set(&self, values: Value) -> Result<()> {
        let object = values
            .as_object()
            .ok_or_else(|| anyhow!("Invalid settings."))?;
        let mut prepared = Vec::with_capacity(object.len());
        for (key, value) in object {
            let value = if key == "api_key" && value.as_str().is_some_and(|s| !s.is_empty()) {
                json!(self.cipher.encrypt(value.as_str().unwrap().as_bytes()))
            } else {
                value.clone()
            };
            prepared.push((key, value.to_string()));
        }
        self.db(|db| {
            let tx = db.transaction()?;
            for (key, value) in prepared {
                tx.execute(
                    "INSERT OR REPLACE INTO settings VALUES (?,?)",
                    params![key, value],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
    }
    pub fn settings(&self) -> Result<Value> {
        let mut result = defaults();
        for key in defaults().as_object().unwrap().keys() {
            result[key] = self.get(key)?;
        }
        result["api_key_set"] = json!(!self.text("api_key")?.is_empty());
        Ok(result)
    }
    pub fn rows(&self, sql: &str, params: &[&dyn rusqlite::ToSql]) -> Result<Vec<Value>> {
        self.db(|db| {
            let mut query = db.prepare(sql)?;
            let rows = query
                .query_map(params, row_json)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }
    pub fn senders(&self) -> Result<Vec<Value>> {
        self.rows("SELECT * FROM senders ORDER BY created_at", &[])
    }
    pub fn jobs(&self) -> Result<Vec<Value>> {
        self.rows("SELECT * FROM jobs ORDER BY created_at DESC LIMIT 100", &[])
    }
    pub fn allowed(&self, email: &str) -> Result<bool> {
        self.db(|db| {
            Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM senders WHERE email=?)",
                [email],
                |r| r.get(0),
            )?)
        })
    }
    pub fn job(&self, id: &str) -> Result<Option<Value>> {
        Ok(self
            .rows("SELECT * FROM jobs WHERE id=?", &[&id])?
            .into_iter()
            .next())
    }
    pub fn update_job(&self, id: &str, mut values: Value) -> Result<()> {
        let object = values
            .as_object_mut()
            .ok_or_else(|| anyhow!("Invalid job update."))?;
        if object.keys().any(|k| {
            ![
                "status",
                "reason",
                "pages",
                "cups_id",
                "printer_queue",
                "updated_at",
            ]
            .contains(&k.as_str())
        }) {
            return Err(anyhow!("Invalid job update."));
        }
        object.insert("updated_at".into(), json!(now()));
        let mut values = Vec::<rusqlite::types::Value>::new();
        let mut assignments = Vec::new();
        for (key, value) in object.iter() {
            assignments.push(format!("{key}=?"));
            values.push(match value {
                Value::Null => rusqlite::types::Value::Null,
                Value::Number(n) => rusqlite::types::Value::Integer(
                    n.as_i64().ok_or_else(|| anyhow!("Invalid job number."))?,
                ),
                Value::String(s) => rusqlite::types::Value::Text(s.clone()),
                _ => return Err(anyhow!("Invalid job value.")),
            });
        }
        values.push(rusqlite::types::Value::Text(id.into()));
        self.db(|db| {
            db.execute(
                &format!("UPDATE jobs SET {} WHERE id=?", assignments.join(",")),
                rusqlite::params_from_iter(values),
            )?;
            Ok(())
        })
    }
}
fn row_json(row: &Row<'_>) -> rusqlite::Result<Value> {
    let mut object = serde_json::Map::new();
    for index in 0..row.as_ref().column_count() {
        let value = match row.get_ref(index)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(v) => json!(v),
            ValueRef::Real(v) => json!(v),
            ValueRef::Text(v) => json!(String::from_utf8_lossy(v)),
            ValueRef::Blob(_) => Value::Null,
        };
        object.insert(row.as_ref().column_name(index)?.into(), value);
    }
    Ok(Value::Object(object))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_are_encrypted_and_not_in_browser_settings() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store
            .set(json!({"api_key":"re_private_test","inbox":"print@home.resend.app"}))
            .unwrap();
        assert_eq!(store.text("api_key").unwrap(), "re_private_test");
        let stored = store
            .rows("SELECT value FROM settings WHERE key='api_key'", &[])
            .unwrap();
        assert!(
            !stored[0]["value"]
                .as_str()
                .unwrap()
                .contains("re_private_test")
        );
        assert!(
            !store
                .settings()
                .unwrap()
                .to_string()
                .contains("re_private_test")
        );
        assert_eq!(store.settings().unwrap()["api_key_set"], true);
        drop(store);
        assert_eq!(
            Store::open(dir.path()).unwrap().text("api_key").unwrap(),
            "re_private_test"
        );
    }
    #[test]
    fn duplicate_ids_and_attachment_ids_are_persistent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.db(|db| {db.execute("INSERT INTO jobs (id,email_id,attachment_id,sender,filename,status,created_at,updated_at) VALUES ('a','email','file','alex@example.com','test.pdf','queued','now','now')",[])?;assert!(db.execute("INSERT INTO jobs (id,email_id,attachment_id,sender,filename,status,created_at,updated_at) VALUES ('b','email','file','alex@example.com','test.pdf','queued','now','now')",[]).is_err());Ok(())}).unwrap();
        assert!(
            store
                .update_job("a", json!({"evil='updated'":"bad"}))
                .is_err()
        );
        assert_eq!(store.jobs().unwrap().len(), 1);
    }
}
