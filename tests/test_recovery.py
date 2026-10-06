import subprocess
from datetime import datetime, timedelta, timezone

from fastapi.testclient import TestClient

from paperboy import printers
from paperboy.app import create_app
from paperboy.store import Store, now
from paperboy.worker import Worker
from tests.test_pipeline import email


def test_backlog_resumes_after_page_budget(tmp_path, monkeypatch):
    store = Store(tmp_path)
    cutoff = datetime.now(timezone.utc)
    store.set(inbox="print@home.resend.app", api_key="re_test", receive_since=cutoff.isoformat())
    items = [
        email(
            sender="blocked@example.com", created_at=(cutoff + timedelta(seconds=i + 1)).isoformat()
        )
        for i in range(2100)
    ]
    items.reverse()
    starts = []

    class Paged:
        def inbox(self, after=None):
            start = next((i + 1 for i, item in enumerate(items) if item["id"] == after), 0)
            starts.append(start)
            return {"data": items[start : start + 100], "has_more": start + 100 < len(items)}

        def close(self):
            pass

    monkeypatch.setattr("paperboy.worker.Resend", lambda key: Paged())
    worker = Worker(store)
    worker.sync()
    assert store.get("scan_after") == items[1999]["id"]
    assert not store.get("last_email_id")
    worker.sync()
    assert starts[-1] == 2000
    assert store.get("last_email_id") == items[0]["id"]
    assert store.get("sync_error") is None
    assert not store.get("scan_after")
    with store.db() as db:
        assert db.execute("SELECT COUNT(*) FROM messages").fetchone()[0] == 2100


def test_offline_printer_cannot_be_paired(monkeypatch):
    monkeypatch.setattr(printers, "validate_uri", lambda uri: "ipp://192.168.1.20:631/ipp/print")

    def unreachable(*args, **kwargs):
        raise OSError("offline")

    monkeypatch.setattr(printers.socket, "create_connection", unreachable)
    monkeypatch.setattr(
        subprocess,
        "run",
        lambda *a, **kw: (_ for _ in ()).throw(AssertionError("Must not create a queue")),
    )
    import pytest

    with pytest.raises(printers.PrinterError, match="did not respond"):
        printers.pair("Offline", "ipp://192.168.1.20:631/ipp/print")


def test_job_previously_sent_to_cups_cannot_be_retried(tmp_path):
    app = create_app(tmp_path, start_worker=False)
    store = app.state.store
    with TestClient(app) as client:
        response = client.post(
            "/api/auth/setup",
            json={
                "setup_code": (tmp_path / "setup.code").read_text(),
                "password": "test-owner-password",
            },
        )
        client.headers["X-Paperboy-CSRF"] = response.json()["csrf"]
        with store.db() as db:
            db.execute("INSERT INTO senders VALUES (?,?,?)", ("alex@example.com", "Alex", now()))
            db.execute(
                "INSERT INTO jobs (id,email_id,attachment_id,sender,filename,status,cups_id,printer_queue,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,?)",
                (
                    "delivered",
                    "email",
                    "file",
                    "alex@example.com",
                    "document.pdf",
                    "failed",
                    42,
                    "printer",
                    now(),
                    now(),
                ),
            )
        assert client.post("/api/jobs/delivered/retry").status_code == 409
