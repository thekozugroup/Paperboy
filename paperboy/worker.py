import logging
import shutil
import threading
import time
import uuid
from datetime import datetime, timedelta, timezone
from email.utils import parseaddr
from pathlib import Path

import httpx

from paperboy import printers
from paperboy.conversion import SUPPORTED, ConversionError, convert
from paperboy.resend import Resend, ResendError, authenticated, download
from paperboy.store import now

log = logging.getLogger("paperboy")


def address(value):
    return parseaddr(value or "")[1].strip().lower()


def timestamp(value):
    return datetime.fromisoformat(value.replace("Z", "+00:00").replace(" ", "T")).astimezone(
        timezone.utc
    )


class Worker:
    def __init__(self, store, converter_socket=None):
        self.store = store
        self.converter_socket = converter_socket
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.wake = threading.Event()
        self.thread = None

    def start(self):
        shutil.rmtree(self.store.root / "spool", ignore_errors=True)
        with self.store.db() as db:
            db.execute("UPDATE jobs SET status='queued' WHERE status='preparing'")
            db.execute(
                "UPDATE jobs SET status='uncertain', reason=? WHERE status='submitting'",
                ("Paperboy restarted during delivery. Check the printer before sending again.",),
            )
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def close(self):
        self.stop.set()
        self.wake.set()
        if self.thread:
            self.thread.join(timeout=5)

    def run(self):
        next_poll = 0
        while not self.stop.is_set():
            try:
                if self.store.get("setup_complete"):
                    if time.monotonic() >= next_poll:
                        self.sync()
                        next_poll = time.monotonic() + 30
                    self.process_one()
                    self.reconcile()
                    self.cleanup()
            except Exception:
                # No exception text: upstream URLs, credentials, and document content stay out of logs.
                log.error("A background operation failed; it will be checked again.")
            self.wake.wait(2)
            if self.wake.is_set():
                next_poll = 0
            self.wake.clear()

    def record_message(self, email, status, reason=None):
        with self.store.db() as db:
            db.execute(
                "INSERT OR IGNORE INTO messages VALUES (?, ?, ?, ?, ?, ?)",
                (
                    email["id"],
                    address(email.get("from")),
                    email.get("subject", "")[:200],
                    status,
                    reason,
                    now(),
                ),
            )

    def ingest(self, email, resend):
        with self.store.db() as db:
            if db.execute("SELECT 1 FROM messages WHERE id=?", (email["id"],)).fetchone():
                return
        target = self.store.get("inbox")
        recipients = [address(v) for v in email.get("to", [])]
        if target not in recipients:
            self.record_message(email, "ignored", "Different receiving address")
            return
        sender = address(email.get("from"))
        if not self.store.allowed(sender):
            self.record_message(email, "blocked", "Sender is not approved")
            return
        full = resend.email(email["id"])
        if address(full.get("from")) != sender or target not in [
            address(v) for v in full.get("to", [])
        ]:
            self.record_message(email, "blocked", "Email details did not match")
            return
        if not authenticated(full):
            self.record_message(email, "blocked", "Sender authentication did not pass")
            return
        attachments = [
            a for a in resend.attachments(email["id"]) if a.get("content_disposition") != "inline"
        ]
        if not attachments:
            self.record_message(email, "ignored", "No file attachments")
            return
        if len(attachments) > 10:
            self.record_message(email, "blocked", "Send 10 or fewer attachments per email")
            return
        with self.store.db() as db:
            used = db.execute(
                "SELECT COUNT(*) FROM jobs WHERE sender=? AND created_at>=?",
                (sender, (datetime.now(timezone.utc) - timedelta(days=1)).isoformat()),
            ).fetchone()[0]
            if used + len(attachments) > self.store.get("daily_limit"):
                self.record_message(email, "blocked", "Sender's daily file limit reached")
                return
            for a in attachments:
                filename = Path(a.get("filename") or "attachment").name[:180]
                state, reason = "queued", None
                if Path(filename).suffix.lower() not in SUPPORTED:
                    state, reason = (
                        "failed",
                        "Unsupported file type. Send a PDF, document, or image.",
                    )
                elif a.get("size", 0) > self.store.get("max_size_mb") * 1024 * 1024:
                    state, reason = "failed", "Attachment exceeds your file size limit."
                db.execute(
                    "INSERT OR IGNORE INTO jobs (id,email_id,attachment_id,sender,filename,status,reason,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?)",
                    (
                        str(uuid.uuid4()),
                        email["id"],
                        a["id"],
                        sender,
                        filename,
                        state,
                        reason,
                        now(),
                        now(),
                    ),
                )
            db.execute(
                "INSERT OR IGNORE INTO messages VALUES (?,?,?,?,?,?)",
                (email["id"], sender, email.get("subject", "")[:200], "accepted", None, now()),
            )

    def sync(self):
        if not self.lock.acquire(blocking=False):
            return
        resend = Resend(self.store.get("api_key"))
        try:
            cursor = self.store.get("last_email_id")
            cutoff = timestamp(self.store.get("receive_since"))
            after = self.store.get("scan_after", "") or None
            newest = (self.store.get("scan_head", "") or None) if after else None
            items = []
            finished = False
            for _ in range(20):
                page = resend.inbox(after)
                data = page.get("data", [])
                if data and newest is None:
                    newest = data[0]["id"]
                for email in data:
                    if email["id"] == cursor or timestamp(email["created_at"]) < cutoff:
                        finished = True
                        break
                    items.append(email)
                if finished or not page.get("has_more") or not data:
                    finished = True
                    break
                after = data[-1]["id"]
            # Process oldest first; durable per-message/job uniqueness makes partial sync retries safe.
            for email in reversed(items):
                if self.stop.is_set():
                    return
                self.ingest(email, resend)
            if not finished:
                self.store.set(
                    scan_after=after,
                    scan_head=newest,
                    last_sync=now(),
                    sync_error="Checking a large inbox backlog. More messages will follow.",
                )
                return
            self.store.set(
                last_sync=now(),
                sync_error=None,
                scan_after="",
                scan_head="",
                **({"last_email_id": newest} if newest else {}),
            )
        except (ResendError, ValueError, KeyError) as error:
            self.store.set(
                sync_error=str(error)
                if isinstance(error, ResendError)
                else "Resend returned unexpected email data."
            )
        finally:
            resend.close()
            self.lock.release()

    def convert_file(self, source, output, settings):
        if not self.converter_socket:
            return convert(source, output, settings["paper"], settings["max_pages"])
        transport = httpx.HTTPTransport(uds=self.converter_socket)
        with httpx.Client(transport=transport, timeout=120) as client:
            with source.open("rb") as file:
                response = client.post(
                    "http://converter/convert",
                    content=file,
                    params={
                        "extension": source.suffix,
                        "paper": settings["paper"],
                        "max_pages": settings["max_pages"],
                    },
                )
            if response.status_code != 200:
                detail = response.json().get("detail", "Document conversion failed.")
                raise ConversionError(detail)
            if len(response.content) > 100 * 1024 * 1024:
                raise ConversionError("The converted document is too large.")
            output.write_bytes(response.content)
            return int(response.headers["x-paperboy-pages"])

    def process_one(self):
        if self.store.get("paused"):
            return
        with self.store.db() as db:
            row = db.execute(
                "SELECT * FROM jobs WHERE status='queued' ORDER BY created_at LIMIT 1"
            ).fetchone()
        if not row:
            return
        job = dict(row)
        if not self.store.allowed(job["sender"]) and job["sender"] != "Paperboy test":
            self.store.update_job(
                job["id"], status="blocked", reason="Sender was removed before printing."
            )
            return
        printer = self.store.get("printer")
        if printers.status(printer)["state"] not in {"ready", "printing"}:
            return
        self.store.update_job(job["id"], status="preparing", reason=None)
        work = self.store.root / "spool" / job["id"]
        work.mkdir(parents=True, exist_ok=True)
        try:
            settings = self.store.settings()
            output = work / "print.pdf"
            if job["sender"] == "Paperboy test":
                source = work / "test.txt"
                source.write_text(
                    "Paperboy\n\nYour printer is connected.\n\nEmail an attachment from an approved address to:\n"
                    + settings["inbox"]
                )
            else:
                resend = Resend(self.store.get("api_key"))
                try:
                    attachments = resend.attachments(job["email_id"])
                finally:
                    resend.close()
                attachment = next((a for a in attachments if a["id"] == job["attachment_id"]), None)
                if not attachment:
                    raise ResendError("The attachment is no longer available in Resend.")
                source = work / ("source" + Path(job["filename"]).suffix.lower())
                download(attachment["download_url"], source, settings["max_size_mb"] * 1024 * 1024)
            pages = self.convert_file(source, output, settings)
            with self.lock:
                # Revocation/pause checked immediately before the irreversible print submission.
                if self.store.get("paused"):
                    self.store.update_job(job["id"], status="queued")
                    return
                with self.store.db() as db:
                    current = db.execute(
                        "SELECT status FROM jobs WHERE id=?", (job["id"],)
                    ).fetchone()
                if not current or current[0] != "preparing":
                    return
                if job["sender"] != "Paperboy test" and not self.store.allowed(job["sender"]):
                    self.store.update_job(
                        job["id"], status="blocked", reason="Sender was removed before printing."
                    )
                    return
                self.store.update_job(
                    job["id"], status="submitting", pages=pages, printer_queue=printer["queue"]
                )
                cups_id = printers.submit(printer, output, job["id"], settings)
                self.store.update_job(job["id"], status="submitted", cups_id=cups_id)
        except printers.PrinterError:
            self.store.update_job(
                job["id"],
                status="uncertain",
                reason="Delivery could not be confirmed. Check the printer before sending again.",
            )
        except (ResendError, ConversionError) as error:
            self.store.update_job(job["id"], status="failed", reason=str(error))
        except Exception:
            current = next((j for j in self.store.jobs() if j["id"] == job["id"]), {})
            uncertain = current.get("status") == "submitting"
            self.store.update_job(
                job["id"],
                status="uncertain" if uncertain else "failed",
                reason="Check the printer before sending again."
                if uncertain
                else "Processing failed. Try exporting the file as PDF.",
            )
        finally:
            shutil.rmtree(work, ignore_errors=True)

    def reconcile(self):
        with self.store.db() as db:
            submitted = [
                dict(row) for row in db.execute("SELECT * FROM jobs WHERE status='submitted'")
            ]
        for job in submitted:
            state = printers.job_state(job["cups_id"])
            if state == 9:
                self.store.update_job(job["id"], status="completed")
            elif state in (7, 8):
                self.store.update_job(
                    job["id"],
                    status="cancelled" if state == 7 else "failed",
                    reason="Cancelled at the printer"
                    if state == 7
                    else "The printer stopped this job.",
                )
            elif state is None:
                self.store.update_job(
                    job["id"],
                    status="uncertain",
                    reason="The print service no longer has a delivery record. Check the printer.",
                )

    def cleanup(self):
        cutoff = (
            datetime.now(timezone.utc) - timedelta(days=self.store.get("retention_days"))
        ).isoformat()
        with self.store.db() as db:
            db.execute(
                "DELETE FROM jobs WHERE created_at<? AND status IN ('completed','cancelled','failed','blocked')",
                (cutoff,),
            )
            # Keep message IDs to prevent historical emails from being printed again.
            db.execute(
                "UPDATE messages SET sender=NULL, subject=NULL WHERE created_at<?", (cutoff,)
            )
