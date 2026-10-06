import socket

import pytest
from fastapi.testclient import TestClient

from paperboy.app import create_app
from paperboy.printers import PrinterError, validate_uri
from paperboy.resend import ResendError, authenticated, validate_download_url


@pytest.fixture
def client(tmp_path):
    app = create_app(tmp_path, start_worker=False)
    with TestClient(app) as client:
        yield client


def own(client):
    store = client.app.state.store
    response = client.post(
        "/api/auth/setup",
        json={
            "setup_code": (store.root / "setup.code").read_text(),
            "password": "test-owner-password",
        },
    )
    assert response.status_code == 200
    client.headers["X-Paperboy-CSRF"] = response.json()["csrf"]
    return store


def test_settings_are_private_and_key_is_encrypted(client):
    assert client.get("/api/state").status_code == 401
    store = own(client)
    store.set(api_key="re_private_test_key")
    response = client.get("/api/state")
    assert response.status_code == 200
    assert response.json()["settings"]["api_key_set"] is True
    assert "re_private_test_key" not in response.text
    assert b"re_private_test_key" not in store.path.read_bytes()
    assert store.get("api_key") == "re_private_test_key"
    assert not (store.root / "setup.code").exists()


def test_setup_requires_code_and_cannot_replace_owner(client):
    assert (
        client.post(
            "/api/auth/setup", json={"setup_code": "wrong", "password": "long-password"}
        ).status_code
        == 403
    )
    own(client)
    assert (
        client.post(
            "/api/auth/setup", json={"setup_code": "wrong", "password": "long-password"}
        ).status_code
        == 409
    )


def test_csrf_and_cross_origin_requests_are_blocked(client):
    own(client)
    body = {"email": "alex@example.com", "name": "Alex"}
    client.headers.pop("X-Paperboy-CSRF")
    assert client.post("/api/senders", json=body).status_code == 403
    client.headers["X-Paperboy-CSRF"] = client.get("/api/auth/status").json()["csrf"]
    assert (
        client.post(
            "/api/senders", json=body, headers={"Origin": "https://attacker.example"}
        ).status_code
        == 403
    )
    assert client.post("/api/senders", json=body).status_code == 200


def test_validation_never_echoes_password(client):
    response = client.post("/api/auth/login", json={"password": "SECRET"})
    assert response.status_code == 422
    assert "SECRET" not in response.text


def test_login_logout_and_rate_limit(client):
    own(client)
    assert client.post("/api/auth/logout").status_code == 200
    assert client.get("/api/state").status_code == 401
    response = client.post("/api/auth/login", json={"password": "test-owner-password"})
    assert response.status_code == 200
    for _ in range(14):
        response = client.post("/api/auth/login", json={"password": "wrong-password"})
    assert response.status_code == 429


def test_sender_addresses_are_exact_normalized_and_unique(client):
    store = own(client)
    assert (
        client.post(
            "/api/senders", json={"email": "  ALEX@Example.com ", "name": "Alex"}
        ).status_code
        == 200
    )
    assert store.allowed("alex@example.com")
    assert not store.allowed("alex+other@example.com")
    assert (
        client.post("/api/senders", json={"email": "alex@example.com", "name": "Alex"}).status_code
        == 409
    )
    assert (
        client.post(
            "/api/senders", json={"email": "Alex <alex@example.com>", "name": "Alex"}
        ).status_code
        == 422
    )
    assert client.delete("/api/senders/alex@example.com").status_code == 200
    assert not store.allowed("alex@example.com")


@pytest.mark.parametrize(
    "auth,expected",
    [
        ({"dmarc": "pass"}, True),
        ({"dmarc": "gray", "dkim": "pass"}, True),
        ({"dmarc": "fail", "dkim": "pass"}, False),
        ({"dmarc": "gray", "spf": "pass", "dkim": "fail"}, False),
        ({"dmarc": "unknown", "dkim": "pass"}, False),
        ({}, False),
    ],
)
def test_only_server_verdicts_authenticate_sender(auth, expected):
    assert (
        authenticated(
            {
                "authentication": auth,
                "headers": {"authentication-results": "forged; dmarc=pass; dkim=pass"},
            }
        )
        is expected
    )


@pytest.mark.parametrize(
    "uri",
    [
        "http://192.168.1.20/",
        "ipp://127.0.0.1:631/print",
        "ipp://8.8.8.8:631/print",
        "ipp://169.254.169.254:631/print",
        "file:///etc/passwd",
        "ipp://user:pass@192.168.1.20:631/print",
        "ipp://192.168.1.20:22/print",
        "ipp://192.168.1.20:631/print\n--bad",
    ],
)
def test_printer_uri_rejects_non_lan_targets(uri):
    with pytest.raises(PrinterError):
        validate_uri(uri)


def test_printer_hostname_is_pinned_to_local_ip(monkeypatch):
    monkeypatch.setattr(
        socket,
        "getaddrinfo",
        lambda *a, **kw: [(socket.AF_INET, socket.SOCK_STREAM, 6, "", ("192.168.1.20", 631))],
    )
    assert validate_uri("ipp://printer.local:631/ipp/print") == "ipp://192.168.1.20:631/ipp/print"


@pytest.mark.parametrize(
    "url",
    [
        "http://inbound-cdn.resend.com/file",
        "https://attacker.example/file",
        "https://resend.com.attacker.example/file",
        "https://user:pass@resend.com/file",
        "https://resend.com:22/file",
    ],
)
def test_download_urls_reject_untrusted_hosts(url):
    with pytest.raises(ResendError):
        validate_download_url(url)


def test_download_url_rejects_private_dns_answer(monkeypatch):
    monkeypatch.setattr(
        socket,
        "getaddrinfo",
        lambda *a: [(socket.AF_INET, socket.SOCK_STREAM, 6, "", ("127.0.0.1", 443))],
    )
    with pytest.raises(ResendError):
        validate_download_url("https://inbound-cdn.resend.com/file")


def test_demo_cannot_mutate_settings(tmp_path):
    with TestClient(create_app(tmp_path, start_worker=False, demo=True)) as client:
        assert client.get("/api/state").json()["demo"] is True
        assert client.put("/api/printing", json={"paused": True}).status_code == 403


def test_preview_cannot_overwrite_live_configuration(tmp_path):
    from paperboy.store import Store

    store = Store(tmp_path)
    store.set(api_key="re_live_configuration", inbox="print@home.resend.app")
    with pytest.raises(ValueError, match="separate data directory"):
        create_app(tmp_path, start_worker=False, demo=True)
    assert store.get("api_key") == "re_live_configuration"


def test_request_body_limit_applies_without_content_length(client):
    response = client.post(
        "/api/auth/login",
        content=iter([b"x" * 10_000, b"x" * 10_000]),
        headers={"Content-Type": "application/json"},
    )
    assert response.status_code == 413
