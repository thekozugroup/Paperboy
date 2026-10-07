"""Actual release/update acceptance on isolated Docker stacks and a software printer.

Requires the published baseline and target releases.
Only explicitly named disposable QA containers/volumes are removed.
"""

import json
import os
import re
import sqlite3
import subprocess
import tempfile
import time
import uuid
from pathlib import Path

import fitz
import httpx
from cryptography.fernet import Fernet

ROOT = Path(__file__).resolve().parent.parent
VERSION = os.environ.get("VERSION", "0.3.1")
BASELINE_VERSION = os.environ.get("BASELINE_VERSION", "0.3.0")
BASELINE = f"ghcr.io/thekozugroup/paperboy:{BASELINE_VERSION}"


def docker(*args):
    result = subprocess.run(["docker", *args], check=True, capture_output=True, text=True)
    return result.stdout.strip()


def wait_for(action, timeout=300):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            result = action()
            if result:
                return result
        except (
            httpx.HTTPError,
            subprocess.CalledProcessError,
            KeyError,
            ValueError,
            AssertionError,
        ):
            pass
        time.sleep(1)
    raise AssertionError("The isolated update fixture did not reach the expected state.")


def run(automatic):
    project = f"paperboy-update-qa-{uuid.uuid4().hex[:10]}"
    printer = f"{project}-printer"
    client = httpx.Client(base_url="http://127.0.0.1:8028", timeout=30, trust_env=False)
    compose_cli = ["docker", "compose"]
    if subprocess.run([*compose_cli, "version"], capture_output=True).returncode:
        compose_cli = ["docker-compose"]
    with tempfile.TemporaryDirectory(prefix=".update-runtime-qa-", dir=ROOT) as temporary:
        folder = Path(temporary)
        override = folder / "compose.yaml"
        gid = docker(
            "run",
            "--rm",
            "--entrypoint",
            "stat",
            "-v",
            "/var/run/docker.sock:/var/run/docker.sock",
            BASELINE,
            "-c",
            "%g",
            "/var/run/docker.sock",
        )
        config = {
            "services": {
                "paperboy": {
                    "image": BASELINE,
                    "extra_hosts": ["api.resend.com:127.0.0.1"],
                },
                "converter": {"image": f"ghcr.io/thekozugroup/paperboy-converter:{VERSION}"},
                "updater": {
                    "image": BASELINE,
                    "environment": {
                        "PAPERBOY_PROJECT": project,
                        "PAPERBOY_PIN_VERSION": BASELINE_VERSION,
                    },
                },
            }
        }
        override.write_text(json.dumps(config))
        command = [
            *compose_cli,
            "-p",
            project,
            "-f",
            str(ROOT / "compose.release.yaml"),
            "-f",
            str(override),
            "--profile",
            "updates",
        ]

        compose_env = dict(
            os.environ,
            PAPERBOY_DOCKER_GID=gid,
            PAPERBOY_PORT="8028",
            PAPERBOY_BIND="127.0.0.1",
            PAPERBOY_SECURE_COOKIE="0",
            PAPERBOY_AUTO_UPDATE="0",
        )

        def compose(*args):
            return subprocess.run(
                [*command, *args], check=True, capture_output=True, text=True, env=compose_env
            ).stdout.strip()

        def api(path, method="GET", data=None, expected=200):
            result = client.request(method, f"/api{path}", json=data)
            assert result.status_code == expected, (path, result.status_code, result.text)
            return result.json()

        def identities():
            return {
                role: compose("ps", "-q", role) for role in ("paperboy", "converter", "updater")
            }

        try:
            compose("up", "-d", "--wait")
            original = identities()
            app = original["paperboy"]
            wait_for(lambda: client.get("/api/health").is_success)
            setup_code = docker("exec", app, "cat", "/data/setup.code")
            auth = api(
                "/auth/setup",
                "POST",
                {"password": "paperboy-update-fixture-password", "setup_code": setup_code},
            )
            client.headers["X-Paperboy-CSRF"] = auth["csrf"]
            api("/senders", "POST", {"name": "Family", "email": "family@example.com"})
            api("/printing", "PUT", {"paused": True})
            docker(
                "run",
                "-d",
                "--name",
                printer,
                "--network",
                f"{project}_default",
                "paperboy-printer:qa",
            )
            ip = json.loads(docker("inspect", printer))[0]["NetworkSettings"]["Networks"][
                f"{project}_default"
            ]["IPAddress"]
            wait_for(
                lambda: api(
                    "/printer",
                    "PUT",
                    {"name": "Update QA printer", "uri": f"ipp://{ip}:631/ipp/print"},
                )
            )
            queue = api("/state")["settings"]["printer"]["queue"]
            pdf = folder / "held.pdf"
            document = fitz.open()
            document.new_page().insert_text(
                (72, 72), "Paperboy update acceptance: one software print."
            )
            document.save(pdf)
            document.close()
            docker("cp", str(pdf), f"{app}:/tmp/held.pdf")
            submitted = docker(
                "exec", app, "lp", "-d", queue, "-o", "job-hold-until=indefinite", "/tmp/held.pdf"
            )
            cups_id = int(re.search(r"-(\d+)\s", submitted).group(1))
            # Stop the synthetic app before editing its fixture DB. No live/user volume is accessed.
            compose("stop", "paperboy")
            database = folder / "paperboy.sqlite3"
            docker("cp", f"{app}:/data/paperboy.sqlite3", str(database))
            encryption_key = folder / "encryption.key"
            docker("cp", f"{app}:/data/encryption.key", str(encryption_key))
            key = Fernet(encryption_key.read_text().strip())
            db = sqlite3.connect(database)
            for setting, value in {
                "api_key": key.encrypt(b"re_update_fixture").decode(),
                "inbox": "print@update-fixture.resend.app",
                "setup_complete": True,
            }.items():
                db.execute(
                    "INSERT OR REPLACE INTO settings VALUES (?,?)", (setting, json.dumps(value))
                )
            now = "2026-10-07T21:00:00+00:00"
            for job, status in [
                ("waiting", "queued"),
                ("unknown", "uncertain"),
                ("active", "submitted"),
            ]:
                db.execute(
                    "INSERT INTO jobs (id,email_id,attachment_id,sender,filename,status,cups_id,printer_queue,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,?)",
                    (
                        job,
                        job,
                        job,
                        "family@example.com",
                        f"{job}.pdf",
                        status,
                        cups_id if job == "active" else None,
                        queue,
                        now,
                        now,
                    ),
                )
            db.commit()
            before_settings = db.execute(
                "SELECT key,value FROM settings WHERE key IN ('api_key','password_hash','password_salt') ORDER BY key"
            ).fetchall()
            db.close()
            docker("cp", str(database), f"{app}:/data/paperboy.sqlite3")
            docker(
                "run",
                "--rm",
                "--network",
                "none",
                "--entrypoint",
                "chown",
                "-v",
                f"{project}_paperboy-data:/data",
                BASELINE,
                "1000:1000",
                "/data/paperboy.sqlite3",
            )
            compose("start", "paperboy")
            wait_for(lambda: api("/updates").get("pin") == BASELINE_VERSION)
            wait_for(lambda: api("/updates").get("available"))
            api("/updates/install", "POST", {"version": VERSION}, expected=409)
            api("/updates/policy", "PUT", {"automatic": True}, expected=409)
            assert identities() == original
            print(
                "Passed: pinned server reports a new release and refuses installation/automatic updates.",
                flush=True,
            )
            config["services"]["updater"]["environment"]["PAPERBOY_PIN_VERSION"] = ""
            override.write_text(json.dumps(config))
            compose("up", "-d", "--no-deps", "updater")
            original = identities()
            wait_for(lambda: api("/updates").get("managed") and api("/updates").get("pin") == "")
            if automatic:
                api("/updates/policy", "PUT", {"automatic": True})
            else:
                api("/updates/install", "POST", {"version": VERSION})
            wait_for(lambda: api("/updates").get("phase") == "waiting", timeout=600)
            assert identities() == original
            assert api("/updates")["draining"] is True
            assert {j["id"]: j["status"] for j in api("/state")["jobs"]}["active"] == "submitted"
            print(
                f"Passed: {'automatic' if automatic else 'manual'} update waits for the active CUPS job; no container replaced early.",
                flush=True,
            )
            docker("exec", app, "lp", "-i", f"{queue}-{cups_id}", "-H", "resume")
            wait_for(
                lambda: (
                    api("/updates").get("current") == VERSION
                    and api("/updates").get("updater_version") == VERSION
                    and api("/updates").get("phase") == "idle"
                ),
                timeout=300,
            )
            updated = identities()
            assert all(updated[role] != original[role] for role in updated), (original, updated)
            state = api("/state")
            assert state["senders"][0]["email"] == "family@example.com"
            assert state["settings"]["printer"]["queue"] == queue
            assert state["settings"]["api_key_set"] is True
            jobs = {job["id"]: job["status"] for job in state["jobs"]}
            assert jobs == {"waiting": "queued", "unknown": "uncertain", "active": "completed"}, (
                jobs
            )
            assert state["updates"]["automatic"] is automatic
            assert state["updates"].get("error") is None
            for image in ("paperboy", "paperboy-converter"):
                for tag in ("stable", "latest"):
                    cached = json.loads(
                        docker("image", "inspect", f"ghcr.io/thekozugroup/{image}:{tag}")
                    )[0]
                    assert cached["Config"]["Labels"]["org.opencontainers.image.version"] == VERSION
            files = docker(
                "exec", printer, "sh", "-c", "find /tmp/printed -name '*.pdf' -type f | wc -l"
            )
            assert files == "1", files
            api("/updates/install", "POST", {"version": VERSION}, expected=409)
            api("/jobs/unknown/retry", "POST", expected=409)
            compose("stop", "paperboy")
            docker("cp", f"{updated['paperboy']}:/data/paperboy.sqlite3", str(database))
            db = sqlite3.connect(database)
            assert (
                db.execute(
                    "SELECT key,value FROM settings WHERE key IN ('api_key','password_hash','password_salt') ORDER BY key"
                ).fetchall()
                == before_settings
            )
            db.close()
            assert not docker(
                "ps",
                "-a",
                "--filter",
                f"label=com.docker.compose.project={project}",
                "--format",
                "{{.Names}} ",
            ).count("-paperboy-old-")
            print(
                f"Passed: {'automatic' if automatic else 'manual'} published {BASELINE_VERSION} → {VERSION}; app, converter, and updater replaced; owner, key, people, printer, queue, and duplicate safety retained; exactly one software print.",
                flush=True,
            )
        finally:
            client.close()
            subprocess.run(["docker", "rm", "-f", printer], capture_output=True)
            compose("down", "-v", "--remove-orphans")


if __name__ == "__main__":
    run(False)
    run(True)
