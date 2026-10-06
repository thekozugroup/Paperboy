from fastapi.testclient import TestClient

from paperboy.app import create_app
from paperboy.store import now


def test_setup_flow_and_connection_change(tmp_path, monkeypatch):
    class Resend:
        def __init__(self, key):
            assert key.startswith("re_")

        def inbox(self):
            return {"data": [], "has_more": False}

        def close(self):
            pass

    monkeypatch.setattr("paperboy.app.Resend", Resend)
    monkeypatch.setattr(
        "paperboy.app.printers.pair", lambda name, uri: {"name": name, "uri": uri, "queue": "test"}
    )
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
        assert client.post("/api/setup/complete").status_code == 400
        assert (
            client.put(
                "/api/settings/email",
                json={"api_key": "re_test_setup", "inbox": "print@home.resend.app"},
            ).status_code
            == 200
        )
        assert (
            client.put(
                "/api/printer", json={"name": "Home", "uri": "ipp://192.168.1.20:631/ipp/print"}
            ).status_code
            == 200
        )
        assert (
            client.post(
                "/api/senders", json={"name": "Alex", "email": "alex@example.com"}
            ).status_code
            == 200
        )
        assert client.post("/api/setup/complete").status_code == 200
        assert store.get("setup_complete") is True
        assert client.put("/api/printing", json={"paused": True}).status_code == 200
        assert store.get("paused") is True
        assert client.post("/api/printer/test").status_code == 200
        assert client.post("/api/printer/test").status_code == 409
        assert len(store.jobs()) == 1
        assert (
            client.put("/api/settings/email", json={"inbox": "new@home.resend.app"}).status_code
            == 200
        )
        assert store.get("api_key") == "re_test_setup"
        assert store.jobs()[0]["status"] == "cancelled"
        assert store.get("receive_since") <= now()
        assert client.put("/api/settings/preferences", json={"max_pages": 101}).status_code == 422


def test_owner_recovery_preserves_print_configuration(tmp_path, monkeypatch):
    import sys

    from paperboy.manage import main
    from paperboy.store import Store

    store = Store(tmp_path)
    store.set(
        password_hash="old",
        password_salt="salt",
        inbox="print@home.resend.app",
        setup_complete=True,
    )
    with store.db() as db:
        db.execute("INSERT INTO sessions VALUES (?,?,?)", ("token", "csrf", 9999999999))
    monkeypatch.setenv("PAPERBOY_DATA_DIR", str(tmp_path))
    monkeypatch.setattr(sys, "argv", ["manage", "reset-password"])
    main()
    assert store.get("password_hash") is None
    assert store.get("inbox") == "print@home.resend.app"
    assert store.get("setup_complete") is True
    with store.db() as db:
        assert db.execute("SELECT COUNT(*) FROM sessions").fetchone()[0] == 0
