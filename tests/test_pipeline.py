import uuid
from datetime import datetime, timedelta, timezone

import pymupdf
import pytest
from PIL import Image
from reportlab.pdfgen.canvas import Canvas

from paperboy import printers
from paperboy.conversion import ConversionError, convert
from paperboy.store import Store, now
from paperboy.worker import Worker


@pytest.fixture
def store(tmp_path):
    store = Store(tmp_path)
    store.set(
        inbox="print@home.resend.app",
        api_key="re_test",
        receive_since=now(),
        printer={
            "name": "Test printer",
            "queue": "test",
            "uri": "ipp://192.168.1.20:631/ipp/print",
        },
    )
    with store.db() as db:
        db.execute("INSERT INTO senders VALUES (?,?,?)", ("alex@example.com", "Alex", now()))
    return store


def email(sender="Alex <alex@example.com>", recipient="print@home.resend.app", **extra):
    return {
        "id": str(uuid.uuid4()),
        "from": sender,
        "to": [recipient],
        "created_at": now(),
        "subject": "Print this",
        **extra,
    }


class Mail:
    def __init__(self, item, auth=None, files=None):
        self.item = item
        self.auth = auth if auth is not None else {"dmarc": "pass"}
        self.files = (
            files
            if files is not None
            else [
                {
                    "id": "file-1",
                    "filename": "document.pdf",
                    "size": 100,
                    "content_disposition": "attachment",
                    "download_url": "https://inbound-cdn.resend.com/file",
                }
            ]
        )
        self.calls = []

    def email(self, email_id):
        self.calls.append("email")
        return {**self.item, "authentication": self.auth}

    def attachments(self, email_id):
        self.calls.append("attachments")
        return self.files

    def close(self):
        pass


@pytest.mark.parametrize(
    "sender,recipient",
    [
        ("spam@example.com", "print@home.resend.app"),
        ("alex@example.com", "other@home.resend.app"),
    ],
)
def test_unapproved_or_wrong_recipient_never_fetches_content(store, sender, recipient):
    item = email(sender, recipient)
    client = Mail(item)
    Worker(store).ingest(item, client)
    assert client.calls == []
    assert store.jobs() == []


def test_spoofed_sender_cannot_fetch_attachments(store):
    item = email()
    client = Mail(item, {"dmarc": "fail", "dkim": "fail"})
    Worker(store).ingest(item, client)
    assert client.calls == ["email"]
    assert store.jobs() == []


def test_duplicate_email_and_attachment_creates_one_job(store):
    item = email()
    client = Mail(item)
    worker = Worker(store)
    worker.ingest(item, client)
    worker.ingest(item, client)
    assert len(store.jobs()) == 1
    assert store.jobs()[0]["status"] == "queued"
    # Deduplication survives a database reopen.
    reopened = Store(store.root)
    Worker(reopened).ingest(item, client)
    assert len(reopened.jobs()) == 1


def test_inline_signatures_are_not_printed(store):
    item = email()
    client = Mail(
        item, files=[{"id": "signature", "filename": "logo.png", "content_disposition": "inline"}]
    )
    Worker(store).ingest(item, client)
    assert store.jobs() == []


def test_daily_limit_blocks_the_whole_message(store):
    store.set(daily_limit=1)
    item = email()
    files = [{"id": str(i), "filename": "file.pdf", "size": 20} for i in range(2)]
    Worker(store).ingest(item, Mail(item, files=files))
    assert store.jobs() == []
    with store.db() as db:
        assert db.execute("SELECT status FROM messages").fetchone()[0] == "blocked"


def test_unsupported_and_oversized_files_are_visible_failures(store):
    item = email()
    files = [
        {"id": "bad", "filename": "malware.exe", "size": 20},
        {"id": "big", "filename": "huge.pdf", "size": 30 * 1024 * 1024},
    ]
    Worker(store).ingest(item, Mail(item, files=files))
    assert len(store.jobs()) == 2
    assert all(job["status"] == "failed" for job in store.jobs())


def make_pdf(path, pages=1):
    canvas = Canvas(str(path))
    for _ in range(pages):
        canvas.drawString(72, 720, "Paperboy test")
        canvas.showPage()
    canvas.save()


@pytest.mark.parametrize("extension", [".pdf", ".txt", ".png", ".tiff"])
def test_common_files_convert_to_sanitized_pdf(tmp_path, extension):
    source, output = tmp_path / ("file" + extension), tmp_path / "output.pdf"
    if extension == ".pdf":
        make_pdf(source)
    elif extension == ".txt":
        source.write_text("A family document\nReady to print.")
    else:
        Image.new("RGB", (320, 240), "white").save(source)
    assert convert(source, output) == 1
    with pymupdf.open(output) as pdf:
        assert len(pdf) == 1
        assert pdf[0].rect.width == 612
        assert pdf.embfile_count() == 0
        assert len(pdf[0].get_images()) == 1


def test_page_limit_and_encrypted_pdf_are_rejected(tmp_path):
    source, output = tmp_path / "pages.pdf", tmp_path / "output.pdf"
    make_pdf(source, 3)
    with pytest.raises(ConversionError, match="1–2"):
        convert(source, output, max_pages=2)
    with pymupdf.open(source) as pdf:
        pdf.save(
            tmp_path / "locked.pdf",
            encryption=pymupdf.PDF_ENCRYPT_AES_256,
            owner_pw="owner",
            user_pw="secret",
        )
    with pytest.raises(ConversionError, match="password protected"):
        convert(tmp_path / "locked.pdf", output)


def test_pdf_active_content_and_attachments_are_removed(tmp_path):
    source, output = tmp_path / "active.pdf", tmp_path / "output.pdf"
    with pymupdf.open() as pdf:
        page = pdf.new_page()
        page.insert_text((72, 72), "A safe-looking file")
        pdf.embfile_add("payload.txt", b"embedded data")
        page.insert_link(
            {
                "kind": pymupdf.LINK_URI,
                "from": pymupdf.Rect(60, 60, 200, 90),
                "uri": "https://example.com",
            }
        )
        pdf.save(source)
    convert(source, output)
    with pymupdf.open(output) as pdf:
        assert pdf.embfile_count() == 0
        assert pdf[0].get_links() == []


def pipeline(store, monkeypatch):
    item = email()
    mail = Mail(item)
    worker = Worker(store)
    worker.ingest(item, mail)
    monkeypatch.setattr("paperboy.worker.Resend", lambda key: mail)
    monkeypatch.setattr("paperboy.worker.download", lambda url, path, size: make_pdf(path))
    monkeypatch.setattr(printers, "status", lambda printer: {"state": "ready"})
    return worker


def test_pipeline_submits_once_and_cleans_working_files(store, monkeypatch):
    worker = pipeline(store, monkeypatch)
    submissions = []

    def submit(printer, path, job_id, settings):
        with pymupdf.open(path) as pdf:
            assert len(pdf) == 1
        submissions.append(job_id)
        return 42

    monkeypatch.setattr(printers, "submit", submit)
    worker.process_one()
    worker.process_one()
    assert len(submissions) == 1
    assert store.jobs()[0]["status"] == "submitted"
    assert store.jobs()[0]["cups_id"] == 42
    assert list((store.root / "spool").iterdir()) == []
    monkeypatch.setattr(printers, "job_state", lambda job_id: 9)
    worker.reconcile()
    assert store.jobs()[0]["status"] == "completed"


@pytest.mark.parametrize("change", ["pause", "revoke", "cancel"])
def test_changes_during_conversion_prevent_print_submission(store, monkeypatch, change):
    worker = pipeline(store, monkeypatch)

    def conversion(source, output, settings):
        make_pdf(output)
        if change == "pause":
            store.set(paused=True)
        elif change == "revoke":
            with store.db() as db:
                db.execute("DELETE FROM senders")
        else:
            store.update_job(store.jobs()[0]["id"], status="cancelled")
        return 1

    worker.convert_file = conversion
    monkeypatch.setattr(printers, "submit", lambda *args: pytest.fail("Must not submit"))
    worker.process_one()
    assert (
        store.jobs()[0]["status"]
        == {"pause": "queued", "revoke": "blocked", "cancel": "cancelled"}[change]
    )


def test_ambiguous_submission_is_never_automatically_retried(store, monkeypatch):
    worker = pipeline(store, monkeypatch)
    calls = []

    def submit(*args):
        calls.append(1)
        raise printers.PrinterError("connection lost after send")

    monkeypatch.setattr(printers, "submit", submit)
    worker.process_one()
    worker.process_one()
    assert calls == [1]
    assert store.jobs()[0]["status"] == "uncertain"


def test_restart_does_not_repeat_interrupted_submission(store, monkeypatch):
    worker = pipeline(store, monkeypatch)
    store.update_job(store.jobs()[0]["id"], status="submitting")
    monkeypatch.setattr(worker, "run", lambda: None)
    worker.start()
    worker.close()
    assert store.jobs()[0]["status"] == "uncertain"


def test_history_cleanup_preserves_deduplication(store):
    item = email()
    client = Mail(item)
    worker = Worker(store)
    worker.ingest(item, client)
    past = (datetime.now(timezone.utc) - timedelta(days=8)).isoformat()
    with store.db() as db:
        db.execute("UPDATE jobs SET created_at=?, status='completed'", (past,))
        db.execute("UPDATE messages SET created_at=?", (past,))
    worker.cleanup()
    assert store.jobs() == []
    worker.ingest(item, client)
    assert store.jobs() == []
    with store.db() as db:
        assert db.execute("SELECT sender FROM messages").fetchone()[0] is None


def test_sync_paginates_and_ignores_mail_from_before_setup(store, monkeypatch):
    cutoff = datetime.now(timezone.utc)
    store.set(receive_since=cutoff.isoformat())
    items = [email(created_at=(cutoff + timedelta(seconds=i + 1)).isoformat()) for i in range(105)]
    items.reverse()
    old = email(created_at=(cutoff - timedelta(days=1)).isoformat())

    class Paginated:
        def inbox(self, after=None):
            return {
                "data": items[:100] if after is None else items[100:] + [old],
                "has_more": after is None,
            }

        def email(self, email_id):
            return {
                **next(item for item in items if item["id"] == email_id),
                "authentication": {"dmarc": "pass"},
            }

        def attachments(self, email_id):
            return [{"id": "file", "filename": "document.pdf", "size": 100}]

        def close(self):
            pass

    store.set(daily_limit=100)
    monkeypatch.setattr("paperboy.worker.Resend", lambda key: Paginated())
    worker = Worker(store)
    worker.sync()
    assert store.get("sync_error") is None
    assert store.get("last_email_id") == items[0]["id"]
    with store.db() as db:
        assert db.execute("SELECT COUNT(*) FROM jobs").fetchone()[0] == 100
        assert db.execute("SELECT COUNT(*) FROM messages").fetchone()[0] == 105
        assert not db.execute("SELECT 1 FROM messages WHERE id=?", (old["id"],)).fetchone()
