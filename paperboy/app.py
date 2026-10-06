import hashlib
import hmac
import logging
import os
import re
import secrets
import time
import uuid
from collections import deque
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Literal

from fastapi import Depends, FastAPI, HTTPException, Request, Response
from fastapi.exceptions import RequestValidationError
from fastapi.responses import FileResponse, JSONResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel, Field, field_validator
from starlette.concurrency import run_in_threadpool

from paperboy import printers
from paperboy.resend import Resend, ResendError
from paperboy.store import Store, now
from paperboy.worker import Worker

WEB = Path(__file__).resolve().parent.parent / "web"


def email_address(value):
    value = value.strip().lower()
    if len(value) > 254 or not re.fullmatch(
        r"[a-z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-z0-9-]+(?:\.[a-z0-9-]+)+", value
    ):
        raise ValueError("Enter a valid email address.")
    return value


def password_hash(password, salt):
    return hashlib.scrypt(password.encode(), salt=bytes.fromhex(salt), n=16384, r=8, p=1).hex()


class Credentials(BaseModel):
    password: str = Field(min_length=10, max_length=128)
    setup_code: str = Field(default="", max_length=128)


class EmailSettings(BaseModel):
    api_key: str = Field(default="", max_length=200)
    inbox: str

    @field_validator("inbox")
    @classmethod
    def valid_inbox(cls, value):
        return email_address(value)


class Sender(BaseModel):
    email: str
    name: str = Field(min_length=1, max_length=80)

    @field_validator("email")
    @classmethod
    def valid_email(cls, value):
        return email_address(value)

    @field_validator("name")
    @classmethod
    def valid_name(cls, value):
        if not value.strip():
            raise ValueError("Enter a name.")
        return value.strip()


class Printer(BaseModel):
    name: str = Field(min_length=1, max_length=100)
    uri: str = Field(min_length=1, max_length=300)


class Preferences(BaseModel):
    paper: Literal["Letter", "A4"] = "Letter"
    color: Literal["monochrome", "color"] = "monochrome"
    sides: Literal["one-sided", "two-sided-long-edge"] = "one-sided"
    max_pages: int = Field(default=50, ge=1, le=100)
    max_size_mb: int = Field(default=25, ge=1, le=25)
    daily_limit: int = Field(default=20, ge=1, le=100)
    retention_days: int = Field(default=7, ge=1, le=90)


class Pause(BaseModel):
    paused: bool


class BodyLimitMiddleware:
    def __init__(self, app):
        self.app = app

    async def __call__(self, scope, receive, send):
        if (
            scope["type"] != "http"
            or not scope.get("path", "").startswith("/api/")
            or scope.get("method") in {"GET", "HEAD"}
        ):
            return await self.app(scope, receive, send)
        body = bytearray()
        while True:
            message = await receive()
            if message["type"] == "http.disconnect":
                return
            chunk = message.get("body", b"")
            if len(body) + len(chunk) > 16_384:
                response = JSONResponse({"detail": "This request is too large."}, status_code=413)
                return await response(scope, receive, send)
            body.extend(chunk)
            if not message.get("more_body", False):
                break
        delivered = False

        async def limited_receive():
            nonlocal delivered
            if not delivered:
                delivered = True
                return {"type": "http.request", "body": bytes(body), "more_body": False}
            return await receive()

        await self.app(scope, limited_receive, send)


def create_app(directory=None, start_worker=True, demo=False):
    store = Store(directory or os.getenv("PAPERBOY_DATA_DIR", "./data"))
    if demo and (store.get("password_hash") or store.get("api_key") not in ("", "preview")):
        raise ValueError("Use an empty, separate data directory for the design preview.")
    worker = Worker(store, os.getenv("PAPERBOY_CONVERTER_SOCKET"))
    attempts = deque(maxlen=100)
    bootstrap = None
    if not store.get("password_hash") and not demo:
        bootstrap_path = store.root / "setup.code"
        if not bootstrap_path.exists():
            bootstrap_path.write_text(secrets.token_urlsafe(12))
            os.chmod(bootstrap_path, 0o600)
        bootstrap = bootstrap_path.read_text().strip()

    @asynccontextmanager
    async def lifespan(app):
        if bootstrap:
            logging.getLogger("paperboy").warning("Paperboy setup code: %s", bootstrap)
        if start_worker and not demo:
            worker.start()
        yield
        worker.close()

    app = FastAPI(
        title="Paperboy", docs_url=None, redoc_url=None, openapi_url=None, lifespan=lifespan
    )
    app.state.store, app.state.worker = store, worker
    app.add_middleware(BodyLimitMiddleware)
    if demo:
        seed_demo(store)

    @app.middleware("http")
    async def boundaries(request, call_next):
        if request.url.path.startswith("/api/") and request.method not in ("GET", "HEAD"):
            origin = request.headers.get("origin")
            if origin and origin != f"{request.url.scheme}://{request.headers.get('host')}":
                return JSONResponse(
                    {"detail": "This request did not come from Paperboy."}, status_code=403
                )
            if request.headers.get("sec-fetch-site") == "cross-site":
                return JSONResponse(
                    {"detail": "Cross-site requests are not allowed."}, status_code=403
                )
            try:
                if int(request.headers.get("content-length", "0")) > 16_384:
                    return JSONResponse({"detail": "This request is too large."}, status_code=413)
            except ValueError:
                return JSONResponse({"detail": "Invalid request."}, status_code=400)
        response = await call_next(request)
        response.headers["X-Content-Type-Options"] = "nosniff"
        response.headers["X-Frame-Options"] = "DENY"
        response.headers["Referrer-Policy"] = "no-referrer"
        response.headers["Content-Security-Policy"] = (
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"
        )
        response.headers["Cache-Control"] = (
            "no-store" if request.url.path.startswith("/api/") else "no-cache"
        )
        return response

    @app.exception_handler(RequestValidationError)
    async def invalid_request(request, error):
        # Omit Pydantic's input echo, which might include passwords or API keys.
        messages = [
            f"{e['loc'][-1]}: {e['msg'].removeprefix('Value error, ')}" for e in error.errors()
        ]
        return JSONResponse({"detail": "; ".join(messages)}, status_code=422)

    @app.exception_handler(printers.PrinterError)
    async def printer_error(request, error):
        return JSONResponse({"detail": str(error)}, status_code=400)

    def session(request: Request):
        if demo:
            if request.method not in ("GET", "HEAD"):
                raise HTTPException(
                    403, "This preview is read-only. Start your own Paperboy to make changes."
                )
            return {"csrf": "demo"}
        token = request.cookies.get("paperboy_session", "")
        token_hash = hashlib.sha256(token.encode()).hexdigest()
        with store.db() as db:
            row = db.execute(
                "SELECT * FROM sessions WHERE token=? AND expires>?", (token_hash, time.time())
            ).fetchone()
        if not row:
            raise HTTPException(401, "Sign in to Paperboy.")
        if request.method not in ("GET", "HEAD") and not hmac.compare_digest(
            request.headers.get("x-paperboy-csrf", ""), row["csrf"]
        ):
            raise HTTPException(403, "Your session changed. Refresh Paperboy and try again.")
        return dict(row)

    def new_session(response: Response):
        token, csrf = secrets.token_urlsafe(32), secrets.token_urlsafe(24)
        with store.db() as db:
            db.execute("DELETE FROM sessions WHERE expires<?", (time.time(),))
            db.execute(
                "INSERT INTO sessions VALUES (?,?,?)",
                (hashlib.sha256(token.encode()).hexdigest(), csrf, time.time() + 86400 * 7),
            )
        response.set_cookie(
            "paperboy_session",
            token,
            httponly=True,
            samesite="strict",
            secure=os.getenv("PAPERBOY_SECURE_COOKIE") == "1",
            max_age=86400 * 7,
        )
        return {"authenticated": True, "csrf": csrf}

    def rate_limit():
        while attempts and attempts[0] < time.time() - 300:
            attempts.popleft()
        if len(attempts) >= 15:
            raise HTTPException(429, "Too many sign-in attempts. Try again in five minutes.")
        attempts.append(time.time())

    @app.get("/api/health")
    def health():
        return {"status": "ok", "app": "Paperboy", "version": "0.1.0"}

    @app.get("/api/auth/status")
    def auth_status(request: Request):
        try:
            current = session(request)
        except HTTPException:
            current = None
        return {
            "initialized": bool(store.get("password_hash")) or demo,
            "authenticated": bool(current),
            "csrf": current["csrf"] if current else None,
            "demo": demo,
        }

    @app.post("/api/auth/setup")
    def auth_setup(body: Credentials, response: Response):
        rate_limit()
        with worker.lock:
            if demo or store.get("password_hash"):
                raise HTTPException(409, "Paperboy already has an owner. Sign in instead.")
            if not hmac.compare_digest(body.setup_code, bootstrap or ""):
                raise HTTPException(
                    403, "That setup code is incorrect. Find it in your Docker logs."
                )
            salt = secrets.token_hex(16)
            store.set(password_salt=salt, password_hash=password_hash(body.password, salt))
            (store.root / "setup.code").unlink(missing_ok=True)
        return new_session(response)

    @app.post("/api/auth/login")
    def login(body: Credentials, response: Response):
        rate_limit()
        saved = store.get("password_hash")
        salt = store.get("password_salt")
        if demo or not saved or not hmac.compare_digest(password_hash(body.password, salt), saved):
            raise HTTPException(401, "That password is incorrect. Try again.")
        return new_session(response)

    @app.post("/api/auth/logout")
    def logout(request: Request, response: Response, auth=Depends(session)):
        token = request.cookies.get("paperboy_session", "")
        with store.db() as db:
            db.execute(
                "DELETE FROM sessions WHERE token=?", (hashlib.sha256(token.encode()).hexdigest(),)
            )
        response.delete_cookie("paperboy_session")
        return {"ok": True}

    @app.get("/api/state")
    async def state(auth=Depends(session)):
        settings = store.settings()
        printer_state = (
            {"state": "ready", "message": "Ready to print"}
            if demo
            else await run_in_threadpool(printers.status, settings["printer"])
        )
        with store.db() as db:
            blocked = [
                dict(r)
                for r in db.execute(
                    "SELECT * FROM messages WHERE status='blocked' AND sender IS NOT NULL ORDER BY created_at DESC LIMIT 30"
                )
            ]
        return {
            "settings": settings,
            "senders": store.senders(),
            "jobs": store.jobs(),
            "blocked": blocked,
            "printer_status": printer_state,
            "demo": demo,
        }

    @app.put("/api/settings/email")
    async def email_settings(body: EmailSettings, auth=Depends(session)):
        key = body.api_key.strip() or store.get("api_key")
        if not key.startswith("re_"):
            raise HTTPException(400, "Enter your Resend API key. It starts with re_.")

        def verify():
            client = Resend(key)
            try:
                client.inbox()
            except ResendError as error:
                raise HTTPException(400, str(error)) from None
            finally:
                client.close()

        await run_in_threadpool(verify)
        with worker.lock:
            changed = body.inbox != store.get("inbox") or key != store.get("api_key")
            store.set(api_key=key, inbox=body.inbox, sync_error=None)
            if changed:
                # Existing queued mail must never move silently to a different account/address.
                with store.db() as db:
                    db.execute(
                        "UPDATE jobs SET status='cancelled',reason='Receiving address changed' WHERE status IN ('queued','preparing')"
                    )
                store.set(receive_since=now(), last_email_id="", scan_after="", scan_head="")
        return {
            "ok": True,
            "message": "Resend connected. New mail will be checked every 30 seconds.",
        }

    @app.get("/api/printers/discover")
    async def discovery(auth=Depends(session)):
        if demo:
            return {"printers": [store.get("printer")], "message": "Preview printer"}
        found = await run_in_threadpool(printers.discover)
        return {
            "printers": found,
            "message": "" if found else "No printers found. You can add one by IP address.",
        }

    @app.put("/api/printer")
    async def pair_printer(body: Printer, auth=Depends(session)):
        result = await run_in_threadpool(printers.pair, body.name, body.uri)
        with worker.lock:
            store.set(printer=result)
        return {"ok": True}

    @app.post("/api/printer/test")
    def test_printer(auth=Depends(session)):
        if not store.get("printer"):
            raise HTTPException(400, "Choose a printer first.")
        with store.db() as db:
            active = db.execute(
                "SELECT 1 FROM jobs WHERE sender='Paperboy test' AND status IN ('queued','preparing','submitting','submitted')"
            ).fetchone()
            if active:
                raise HTTPException(409, "A test page is already in the queue.")
            job_id = str(uuid.uuid4())
            db.execute(
                "INSERT INTO jobs (id,email_id,attachment_id,sender,filename,status,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?)",
                (
                    job_id,
                    "test",
                    job_id,
                    "Paperboy test",
                    "Paperboy test page",
                    "queued",
                    now(),
                    now(),
                ),
            )
        worker.wake.set()
        return {"ok": True}

    @app.post("/api/senders")
    def add_sender(body: Sender, auth=Depends(session)):
        with worker.lock, store.db() as db:
            if db.execute("SELECT 1 FROM senders WHERE email=?", (body.email,)).fetchone():
                raise HTTPException(409, "This address is already approved.")
            db.execute("INSERT INTO senders VALUES (?,?,?)", (body.email, body.name, now()))
        return {"ok": True}

    @app.delete("/api/senders/{email}")
    def remove_sender(email: str, auth=Depends(session)):
        with worker.lock, store.db() as db:
            db.execute("DELETE FROM senders WHERE email=?", (email,))
            db.execute(
                "UPDATE jobs SET status='blocked',reason='Sender was removed before printing' WHERE sender=? AND status IN ('queued','preparing')",
                (email,),
            )
        return {"ok": True}

    @app.put("/api/settings/preferences")
    def preferences(body: Preferences, auth=Depends(session)):
        store.set(**body.model_dump())
        return {"ok": True}

    @app.post("/api/setup/complete")
    def complete_setup(auth=Depends(session)):
        if (
            not store.get("api_key")
            or not store.get("inbox")
            or not store.get("printer")
            or not store.senders()
        ):
            raise HTTPException(
                400, "Connect Resend, choose a printer, and add an approved sender first."
            )
        if not store.get("setup_complete"):
            store.set(setup_complete=True, receive_since=now())
        worker.wake.set()
        return {"ok": True}

    @app.put("/api/printing")
    def pause(body: Pause, auth=Depends(session)):
        with worker.lock:
            store.set(paused=body.paused)
        worker.wake.set()
        return {"ok": True}

    @app.post("/api/sync")
    async def sync(auth=Depends(session)):
        if not store.get("setup_complete"):
            raise HTTPException(400, "Finish setup before checking mail.")
        await run_in_threadpool(worker.sync)
        return {"ok": not store.get("sync_error"), "error": store.get("sync_error")}

    @app.post("/api/jobs/{job_id}/cancel")
    def cancel_job(job_id: str, auth=Depends(session)):
        with worker.lock:
            with store.db() as db:
                row = db.execute("SELECT * FROM jobs WHERE id=?", (job_id,)).fetchone()
            if not row:
                raise HTTPException(404, "Job not found.")
            if row["status"] == "submitted":
                printers.cancel(row["cups_id"])
            elif row["status"] not in {"queued", "preparing"}:
                raise HTTPException(409, "This job cannot be cancelled.")
            store.update_job(job_id, status="cancelled", reason="Cancelled in Paperboy")
        return {"ok": True}

    @app.post("/api/jobs/{job_id}/retry")
    def retry_job(job_id: str, auth=Depends(session)):
        with worker.lock, store.db() as db:
            row = db.execute("SELECT * FROM jobs WHERE id=?", (job_id,)).fetchone()
            if (
                not row
                or row["status"] != "failed"
                or row["cups_id"] is not None
                or row["printer_queue"] is not None
            ):
                raise HTTPException(409, "Only a file that failed before delivery can be retried.")
            if not store.allowed(row["sender"]) and row["sender"] != "Paperboy test":
                raise HTTPException(403, "This sender is no longer approved.")
            db.execute(
                "UPDATE jobs SET status='queued',reason=NULL,updated_at=? WHERE id=?",
                (now(), job_id),
            )
        worker.wake.set()
        return {"ok": True}

    @app.get("/")
    def index():
        return FileResponse(WEB / "index.html")

    app.mount("/assets", StaticFiles(directory=WEB), name="assets")
    return app


def seed_demo(store):
    store.set(
        inbox="print@home.resend.app",
        api_key="preview",
        setup_complete=True,
        receive_since=now(),
        last_sync=now(),
        printer={
            "name": "Brother HL-L2460DW",
            "uri": "ipp://192.168.1.42:631/ipp/print",
            "queue": "preview",
        },
    )
    with store.db() as db:
        for email, name in [
            ("alex@example.com", "Alex"),
            ("sam@example.com", "Sam"),
            ("jordan@example.com", "Jordan"),
        ]:
            db.execute("INSERT OR IGNORE INTO senders VALUES (?,?,?)", (email, name, now()))
        if not db.execute("SELECT 1 FROM jobs").fetchone():
            for filename, sender, pages, state in [
                ("Weekend itinerary.pdf", "alex@example.com", 3, "completed"),
                ("School permission slip.docx", "sam@example.com", 1, "completed"),
                ("Family photo.jpg", "jordan@example.com", 1, "completed"),
            ]:
                job_id = str(uuid.uuid4())
                db.execute(
                    "INSERT INTO jobs (id,email_id,attachment_id,sender,filename,status,pages,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?)",
                    (job_id, job_id, job_id, sender, filename, state, pages, now(), now()),
                )


app = create_app(demo=os.getenv("PAPERBOY_DEMO") == "1")
