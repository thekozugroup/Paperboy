import ipaddress
import socket
import time
from urllib.parse import quote, urlparse

import httpx


class ResendError(Exception):
    pass


class Resend:
    def __init__(self, api_key, client=None):
        self.client = client or httpx.Client(
            base_url="https://api.resend.com",
            timeout=30,
            headers={"Authorization": f"Bearer {api_key}", "User-Agent": "Paperboy/0.1"},
            follow_redirects=False,
        )
        self.last_request = 0.0

    def close(self):
        self.client.close()

    def get(self, path, params=None):
        # Stay below Resend's default two requests/second, including attachment requests.
        time.sleep(max(0, 0.6 - (time.monotonic() - self.last_request)))
        self.last_request = time.monotonic()
        try:
            response = self.client.get(path, params=params)
        except httpx.HTTPError:
            raise ResendError(
                "Resend could not be reached. Check your internet connection."
            ) from None
        if response.status_code in (401, 403):
            raise ResendError("Resend rejected the API key. Use a key with full access.")
        if response.status_code == 429:
            raise ResendError("Resend is busy. Paperboy will check again shortly.")
        if response.is_error:
            raise ResendError(
                f"Resend returned an error ({response.status_code}). Try again shortly."
            )
        return response.json()

    def inbox(self, after=None):
        return self.get("/emails/receiving", {"limit": 100, **({"after": after} if after else {})})

    def email(self, email_id):
        return self.get(f"/emails/receiving/{quote(email_id, safe='')}")

    def attachments(self, email_id):
        items, after = [], None
        for _ in range(5):
            response = self.get(
                f"/emails/receiving/{quote(email_id, safe='')}/attachments",
                {"limit": 100, **({"after": after} if after else {})},
            )
            items.extend(response["data"])
            if not response.get("has_more"):
                return items
            if not response["data"]:
                break
            after = response["data"][-1]["id"]
        raise ResendError("Too many attachments in this message.")


def authenticated(email):
    # Use Resend's server-computed verdict. Message headers are sender-controlled.
    auth = email.get("authentication") or {}
    return auth.get("dmarc") == "pass" or (
        auth.get("dmarc") == "gray" and auth.get("dkim") == "pass"
    )


def validate_download_url(url):
    parsed = urlparse(url)
    host = (parsed.hostname or "").lower()
    trusted = (
        host == "resend.com" or host.endswith(".resend.com") or host.endswith(".cloudfront.net")
    )
    if (
        parsed.scheme != "https"
        or not trusted
        or parsed.username
        or parsed.password
        or parsed.port not in (None, 443)
    ):
        raise ResendError("Resend provided an unexpected attachment address.")
    try:
        addresses = socket.getaddrinfo(host, 443)
    except OSError:
        raise ResendError("The attachment server could not be reached.") from None
    if not addresses or any(not ipaddress.ip_address(a[4][0]).is_global for a in addresses):
        raise ResendError("The attachment address is not a public server.")


def download(url, path, max_bytes):
    validate_download_url(url)
    # Separate client: never send the Resend API key to a CDN, never follow redirects.
    try:
        with httpx.stream(
            "GET", url, timeout=60, follow_redirects=False, trust_env=False
        ) as response:
            if response.status_code != 200:
                raise ResendError("The attachment could not be downloaded. Try again.")
            total = 0
            with path.open("wb") as file:
                for chunk in response.iter_bytes(64 * 1024):
                    total += len(chunk)
                    if total > max_bytes:
                        raise ResendError("This attachment exceeds your file size limit.")
                    file.write(chunk)
    except httpx.HTTPError:
        raise ResendError("The attachment download was interrupted. Try again.") from None
