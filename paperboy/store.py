import json
import os
import sqlite3
import threading
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path

from cryptography.fernet import Fernet


def now():
    return datetime.now(timezone.utc).isoformat()


DEFAULTS = {
    "inbox": "",
    "api_key": "",
    "printer": None,
    "setup_complete": False,
    "paused": False,
    "paper": "Letter",
    "color": "monochrome",
    "sides": "one-sided",
    "max_pages": 50,
    "max_size_mb": 25,
    "daily_limit": 20,
    "retention_days": 7,
    "receive_since": "",
    "last_email_id": "",
    "last_sync": None,
    "sync_error": None,
}


class Store:
    def __init__(self, directory):
        self.root = Path(directory)
        self.root.mkdir(parents=True, exist_ok=True)
        os.chmod(self.root, 0o700)
        self.lock = threading.RLock()
        key_path = self.root / "encryption.key"
        if not key_path.exists():
            with key_path.open("xb") as f:
                f.write(Fernet.generate_key())
            os.chmod(key_path, 0o600)
        self.cipher = Fernet(key_path.read_bytes())
        self.path = self.root / "paperboy.sqlite3"
        with self.db() as db:
            db.executescript("""
                PRAGMA journal_mode=WAL;
                CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS senders (
                    email TEXT PRIMARY KEY, name TEXT NOT NULL, created_at TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS messages (
                    id TEXT PRIMARY KEY, sender TEXT, subject TEXT, status TEXT,
                    reason TEXT, created_at TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS jobs (
                    id TEXT PRIMARY KEY, email_id TEXT NOT NULL, attachment_id TEXT NOT NULL,
                    sender TEXT NOT NULL, filename TEXT NOT NULL, status TEXT NOT NULL,
                    reason TEXT, pages INTEGER, cups_id INTEGER, printer_queue TEXT,
                    created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                    UNIQUE(email_id, attachment_id));
                CREATE TABLE IF NOT EXISTS sessions (
                    token TEXT PRIMARY KEY, csrf TEXT NOT NULL, expires REAL NOT NULL);
            """)
        os.chmod(self.path, 0o600)

    @contextmanager
    def db(self):
        with self.lock:
            db = sqlite3.connect(self.path, timeout=15)
            db.row_factory = sqlite3.Row
            try:
                yield db
                db.commit()
            except Exception:
                db.rollback()
                raise
            finally:
                db.close()

    def get(self, key, default=None):
        with self.db() as db:
            row = db.execute("SELECT value FROM settings WHERE key=?", (key,)).fetchone()
        value = json.loads(row[0]) if row else DEFAULTS.get(key, default)
        if key == "api_key" and value:
            return self.cipher.decrypt(value.encode()).decode()
        return value

    def set(self, **values):
        with self.db() as db:
            for key, value in values.items():
                if key == "api_key" and value:
                    value = self.cipher.encrypt(value.encode()).decode()
                db.execute(
                    "INSERT OR REPLACE INTO settings VALUES (?, ?)", (key, json.dumps(value))
                )

    def settings(self):
        result = {key: self.get(key) for key in DEFAULTS if key != "api_key"}
        result["api_key_set"] = bool(self.get("api_key"))
        return result

    def senders(self):
        with self.db() as db:
            return [dict(r) for r in db.execute("SELECT * FROM senders ORDER BY created_at")]

    def allowed(self, email):
        with self.db() as db:
            return bool(db.execute("SELECT 1 FROM senders WHERE email=?", (email,)).fetchone())

    def jobs(self):
        with self.db() as db:
            return [
                dict(r) for r in db.execute("SELECT * FROM jobs ORDER BY created_at DESC LIMIT 100")
            ]

    def update_job(self, job_id, **values):
        allowed = {"status", "reason", "pages", "cups_id", "printer_queue", "updated_at"}
        if not values.keys() <= allowed:
            raise ValueError("Invalid job update")
        values["updated_at"] = now()
        with self.db() as db:
            db.execute(
                f"UPDATE jobs SET {','.join(k + '=?' for k in values)} WHERE id=?",
                (*values.values(), job_id),
            )
