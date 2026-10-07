"""Exercise the native Docker service with legacy data and a software IPP printer.

All accounts, credentials, data volumes, and print output here are disposable fixtures.
Resend is mapped to loopback in the fixture container. No household printer or real
Resend account is used.
"""

import hashlib
import json
import os
import sqlite3
import subprocess
import tempfile
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path

import httpx
import pymupdf
from cryptography.fernet import Fernet


def docker(*args):
    result = subprocess.run(
        ["docker", *args], check=True, capture_output=True, text=True, timeout=120
    )
    return result.stdout.strip()


def wait_for(check, seconds=90):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            result = check()
            if result:
                return result
        except (httpx.HTTPError, subprocess.CalledProcessError):
            pass
        time.sleep(0.5)
    raise AssertionError("The fixture service did not become ready.")


def seed_legacy(root):
    """Create the exact SQLite/Fernet/scrypt layout written by the previous release."""
    key = Fernet.generate_key()
    (root / "encryption.key").write_bytes(key)
    db = sqlite3.connect(root / "paperboy.sqlite3")
    db.executescript(
        """
        CREATE TABLE settings (key TEXT PRIMARY KEY,value TEXT NOT NULL);
        CREATE TABLE senders (email TEXT PRIMARY KEY,name TEXT NOT NULL,created_at TEXT NOT NULL);
        CREATE TABLE messages (id TEXT PRIMARY KEY,sender TEXT,subject TEXT,status TEXT,reason TEXT,created_at TEXT NOT NULL);
        CREATE TABLE jobs (id TEXT PRIMARY KEY,email_id TEXT NOT NULL,attachment_id TEXT NOT NULL,sender TEXT NOT NULL,filename TEXT NOT NULL,status TEXT NOT NULL,reason TEXT,pages INTEGER,cups_id INTEGER,printer_queue TEXT,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,UNIQUE(email_id,attachment_id));
        CREATE TABLE sessions (token TEXT PRIMARY KEY,csrf TEXT NOT NULL,expires REAL NOT NULL);
        """
    )
    salt = "00112233445566778899aabbccddeeff"
    saved = {
        "password_salt": salt,
        "password_hash": hashlib.scrypt(
            b"legacy-owner-password", salt=bytes.fromhex(salt), n=16384, r=8, p=1
        ).hex(),
        "api_key": Fernet(key).encrypt(b"re_disposable_fixture_only").decode(),
        "inbox": "print@fixture.resend.app",
        "setup_complete": False,
    }
    db.executemany(
        "INSERT INTO settings VALUES (?,?)", [(k, json.dumps(v)) for k, v in saved.items()]
    )
    now = datetime.now(timezone.utc).isoformat()
    db.execute("INSERT INTO senders VALUES (?,?,?)", ("alex@example.com", "Alex", now))
    db.execute(
        "INSERT INTO messages VALUES (?,?,?,?,?,?)",
        ("seen-email", "alex@example.com", "Fixture", "accepted", None, now),
    )
    for job_id, status in [("legacy-pending", "preparing"), ("legacy-delivery", "submitting")]:
        db.execute(
            "INSERT INTO jobs (id,email_id,attachment_id,sender,filename,status,printer_queue,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?)",
            (job_id, job_id, job_id, "alex@example.com", "family.pdf", status, None, now, now),
        )
    db.execute(
        "INSERT INTO sessions VALUES (?,?,?)",
        (hashlib.sha256(b"legacy-session-token").hexdigest(), "legacy-csrf", time.time() + 600),
    )
    db.commit()
    db.close()
    return key


def run():
    image = os.getenv("PAPERBOY_APP_IMAGE", "paperboy:local")
    converter = os.getenv("PAPERBOY_CONVERTER_IMAGE", "paperboy-converter:local")
    printer_image = os.getenv("PAPERBOY_PRINTER_IMAGE", "paperboy-printer:qa")
    port = int(os.getenv("PAPERBOY_QA_PORT", "8027"))
    suffix = uuid.uuid4().hex[:10]
    app, printer, tools = [f"paperboy-qa-{role}-{suffix}" for role in ("app", "printer", "tools")]
    network, data, socket = [
        f"paperboy-qa-{role}-{suffix}" for role in ("network", "data", "socket")
    ]
    containers, volumes = [], []
    client = httpx.Client(base_url=f"http://127.0.0.1:{port}", timeout=65, trust_env=False)
    try:
        docker("network", "create", network)
        for volume in (data, socket):
            docker("volume", "create", volume)
            volumes.append(volume)
        docker(
            "run",
            "-d",
            "--name",
            tools,
            "--network",
            "none",
            "--read-only",
            "--memory",
            "768m",
            "--cpus",
            "1",
            "--pids-limit",
            "64",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges:true",
            "--tmpfs",
            "/tmp:size=512m,mode=1777",
            "-v",
            f"{socket}:/run/paperboy",
            converter,
        )
        containers.append(tools)
        wait_for(lambda: docker("exec", tools, "paperboy-tools", "health") == "")
        docker(
            "run",
            "-d",
            "--name",
            printer,
            "--hostname",
            "printer",
            "--network",
            network,
            printer_image,
        )
        containers.append(printer)
        printer_ip = json.loads(docker("inspect", printer))[0]["NetworkSettings"]["Networks"][
            network
        ]["IPAddress"]
        docker(
            "create",
            "--name",
            app,
            "--network",
            network,
            "--memory",
            "512m",
            "--pids-limit",
            "128",
            "--add-host",
            "api.resend.com:127.0.0.1",
            "-p",
            f"127.0.0.1:{port}:8025",
            "-v",
            f"{data}:/data",
            "-v",
            f"{socket}:/run/paperboy",
            image,
        )
        containers.append(app)
        with tempfile.TemporaryDirectory(prefix="paperboy-legacy-") as tmp:
            key = seed_legacy(Path(tmp))
            docker("cp", f"{tmp}/.", f"{app}:/data/")
        docker(
            "run",
            "--rm",
            "--entrypoint",
            "chown",
            "-v",
            f"{data}:/data",
            image,
            "-R",
            "1000:1000",
            "/data",
        )
        docker("start", app)
        wait_for(lambda: client.get("/api/health").status_code == 200)
        assert client.get("/api/health").json()["runtime"] == "Rust"
        assert client.get("/api/state").status_code == 401
        client.cookies.set("paperboy_session", "legacy-session-token")
        client.headers["X-Paperboy-CSRF"] = "legacy-csrf"
        state = client.get("/api/state").json()
        assert state["settings"]["inbox"] == "print@fixture.resend.app"
        assert state["settings"]["api_key_set"] is True
        assert "re_disposable_fixture_only" not in json.dumps(state)
        jobs = {job["id"]: job for job in state["jobs"]}
        assert jobs["legacy-pending"]["status"] == "queued"
        assert jobs["legacy-delivery"]["status"] == "uncertain"
        assert client.post("/api/jobs/legacy-delivery/retry").status_code == 409
        assert client.post("/api/jobs/legacy-pending/cancel").status_code == 200
        client.cookies.clear()
        login = client.post("/api/auth/login", json={"password": "legacy-owner-password"})
        assert login.status_code == 200
        client.headers["X-Paperboy-CSRF"] = login.json()["csrf"]
        print(
            "Passed: Python-era encrypted settings, password, session, people, and safe queue recovery."
        )

        docker("stop", "--time", "10", app)
        docker(
            "run",
            "--rm",
            "--entrypoint",
            "runuser",
            "-v",
            f"{data}:/data",
            image,
            "-u",
            "paperboy",
            "--",
            "paperboy",
            "reset-password",
        )
        docker("start", app)
        wait_for(lambda: client.get("/api/health").status_code == 200)
        assert client.get("/api/state").status_code == 401
        code = docker("exec", "--user", "1000", app, "cat", "/data/setup.code")
        response = client.post(
            "/api/auth/setup", json={"setup_code": code, "password": "new-owner-password"}
        )
        assert response.status_code == 200
        client.headers["X-Paperboy-CSRF"] = response.json()["csrf"]
        state = client.get("/api/state").json()
        assert state["settings"]["inbox"] == "print@fixture.resend.app"
        assert state["senders"][0]["email"] == "alex@example.com"
        assert len(state["jobs"]) == 2
        print(
            "Passed: native owner recovery preserves mail, people, and queue; old sessions stop working."
        )

        uri = f"ipp://{printer_ip}:631/ipp/print"
        discovered = client.get("/api/printers/discover").json()["printers"]
        assert any(printer_ip in item["uri"] for item in discovered), discovered
        wait_for(
            lambda: (
                client.put(
                    "/api/printer", json={"name": "Software printer", "uri": uri}
                ).status_code
                == 200
            )
        )
        assert client.post("/api/printer/test").status_code == 200
        assert client.post("/api/printer/test").status_code == 409
        assert client.post("/api/setup/complete").status_code == 200

        def completed():
            state = client.get("/api/state").json()
            tests = [job for job in state["jobs"] if job["sender"] == "Paperboy test"]
            assert len(tests) == 1
            assert tests[0]["status"] not in ("failed", "uncertain"), tests[0]["reason"]
            return tests[0] if tests[0]["status"] == "completed" else None

        job = wait_for(completed, seconds=110)
        assert job["pages"] == 1 and job["cups_id"] > 0
        docker("exec", app, "sh", "-c", 'test -z "$(ls -A /data/spool)"')
        with tempfile.TemporaryDirectory(prefix="paperboy-printed-") as tmp:
            docker("cp", f"{printer}:/tmp/printed/.", tmp)
            outputs = list(Path(tmp).glob("*.pdf"))
            assert len(outputs) == 1, [p.name for p in Path(tmp).iterdir()]
            with pymupdf.open(outputs[0]) as pdf:
                assert len(pdf) == 1 and pdf.embfile_count() == 0
                assert pdf[0].get_links() == []
        print(
            "Passed: native API → isolated Rust conversion → CUPS → software IPP printer, one clean PDF."
        )

        docker("restart", "--time", "10", app)
        wait_for(lambda: client.get("/api/health").status_code == 200)
        assert completed()["id"] == job["id"]
        docker("stop", "--time", "10", app)
        with tempfile.TemporaryDirectory(prefix="paperboy-saved-") as tmp:
            docker("cp", f"{app}:/data/.", tmp)
            db = sqlite3.connect(Path(tmp) / "paperboy.sqlite3")
            encrypted = json.loads(
                db.execute("SELECT value FROM settings WHERE key='api_key'").fetchone()[0]
            )
            assert Fernet(key).decrypt(encrypted.encode()) == b"re_disposable_fixture_only"
            assert (
                db.execute("SELECT COUNT(*) FROM messages WHERE id='seen-email'").fetchone()[0] == 1
            )
            db.close()
        print(
            "Passed: restart keeps the completed job and duplicate records; saved Fernet key remains compatible."
        )
    finally:
        client.close()
        for container in reversed(containers):
            subprocess.run(["docker", "rm", "-f", container], capture_output=True, timeout=30)
        for volume in reversed(volumes):
            subprocess.run(["docker", "volume", "rm", volume], capture_output=True, timeout=30)
        subprocess.run(["docker", "network", "rm", network], capture_output=True, timeout=30)


if __name__ == "__main__":
    run()
