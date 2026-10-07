import hashlib
import ipaddress
import re
import socket
import subprocess
import time
from urllib.parse import urlparse, urlunparse

from zeroconf import IPVersion, ServiceBrowser, ServiceListener, Zeroconf


class PrinterError(Exception):
    pass


def validate_uri(uri):
    try:
        parsed = urlparse(uri.strip())
        port = parsed.port
    except ValueError:
        raise PrinterError("Enter a valid printer address.") from None
    if (
        parsed.scheme not in ("ipp", "ipps")
        or not parsed.hostname
        or parsed.username
        or parsed.password
        or parsed.query
        or parsed.fragment
        or port not in (None, 631, 443)
        or re.search(r"[\s\x00-\x1f]", uri)
    ):
        raise PrinterError("Use a local IPP address, such as ipp://192.168.1.50:631/ipp/print.")
    try:
        addresses = socket.getaddrinfo(parsed.hostname, port or 631, type=socket.SOCK_STREAM)
    except OSError:
        raise PrinterError(
            "The printer address could not be found. Check its IP address."
        ) from None
    if not addresses:
        raise PrinterError("The printer address could not be found.")
    for address in addresses:
        ip = ipaddress.ip_address(address[4][0].split("%")[0])
        local = (
            (
                ip in ipaddress.ip_network("10.0.0.0/8")
                or ip in ipaddress.ip_network("172.16.0.0/12")
                or ip in ipaddress.ip_network("192.168.0.0/16")
            )
            if ip.version == 4
            else (ip in ipaddress.ip_network("fc00::/7") or ip.is_link_local)
        )
        if not local or ip.is_loopback or ip.is_multicast or ip.is_unspecified:
            raise PrinterError("Choose a printer on your local network.")
    # Pin the resolved LAN address to prevent hostname changes between validation and use.
    host = addresses[0][4][0]
    if ":" in host:
        host = f"[{host}]"
    return urlunparse(
        (parsed.scheme, f"{host}:{port or 631}", parsed.path or "/ipp/print", "", "", "")
    )


def connection():
    try:
        import cups

        return cups.Connection()
    except Exception:
        raise PrinterError(
            "The print service is unavailable. Check that the Docker service is running."
        ) from None


class Listener(ServiceListener):
    def __init__(self):
        self.printers = {}

    def add_service(self, zc, service_type, name):
        info = zc.get_service_info(service_type, name, timeout=1500)
        if not info:
            return
        addresses = info.parsed_addresses(IPVersion.V4Only)
        if not addresses:
            return
        properties = {
            k.decode(errors="replace"): v.decode(errors="replace")
            for k, v in info.properties.items()
            if v is not None
        }
        scheme = "ipps" if service_type.startswith("_ipps") else "ipp"
        uri = (
            f"{scheme}://{addresses[0]}:{info.port}/{properties.get('rp', 'ipp/print').lstrip('/')}"
        )
        label = properties.get("ty") or name.split("._ipp")[0]
        self.printers[uri] = {
            "name": label,
            "uri": uri,
            "location": properties.get("note", "On your network"),
        }

    def update_service(self, zc, service_type, name):
        self.add_service(zc, service_type, name)

    def remove_service(self, zc, service_type, name):
        pass


def discover():
    listener = Listener()
    try:
        with Zeroconf(ip_version=IPVersion.V4Only) as zc:
            browser = ServiceBrowser(zc, ["_ipp._tcp.local.", "_ipps._tcp.local."], listener)
            time.sleep(3)
            browser.cancel()
    except OSError:
        raise PrinterError(
            "Discovery is unavailable here. Add your printer by IP address."
        ) from None
    return list(listener.printers.values())


def pair(name, uri):
    uri = validate_uri(uri)
    endpoint = urlparse(uri)
    try:
        with socket.create_connection((endpoint.hostname, endpoint.port or 631), timeout=3):
            pass
    except OSError:
        raise PrinterError("The printer did not respond. Check its IP address and power.") from None
    queue = "paperboy_" + hashlib.sha256(uri.encode()).hexdigest()[:12]
    try:
        subprocess.run(
            [
                "lpadmin",
                "-p",
                queue,
                "-E",
                "-v",
                uri,
                "-m",
                "everywhere",
                "-D",
                name[:100],
                "-o",
                "printer-error-policy=retry-job",
            ],
            capture_output=True,
            check=True,
            timeout=25,
        )
        attrs = connection().getPrinterAttributes(queue)
    except (subprocess.SubprocessError, OSError):
        raise PrinterError(
            "The printer did not respond. Check the address and that it supports AirPrint or IPP."
        ) from None
    if attrs.get("printer-state") == 5:
        raise PrinterError("The printer queue is stopped. Check the printer and pair it again.")
    return {"name": name[:100], "uri": uri, "queue": queue}


def status(printer):
    if not printer:
        return {"state": "unpaired", "message": "Choose a printer"}
    try:
        attrs = connection().getPrinterAttributes(printer["queue"])
        state = attrs.get("printer-state", 5)
        reasons = attrs.get("printer-state-reasons", [])
        if state == 5:
            return {"state": "offline", "message": "Printer needs attention", "reasons": reasons}
        # CUPS queue state alone is not physical reachability. Check the paired LAN endpoint.
        uri = urlparse(printer["uri"])
        with socket.create_connection((uri.hostname, uri.port or 631), timeout=2):
            pass
        return {
            "state": "printing" if state == 4 else "ready",
            "message": "Printing" if state == 4 else "Ready to print",
            "reasons": reasons,
        }
    except Exception:
        return {"state": "offline", "message": "Printer unavailable"}


def submit(printer, path, job_id, settings):
    try:
        return connection().printFile(
            printer["queue"],
            str(path),
            f"Paperboy {job_id}",
            {
                "media": settings["paper"],
                "print-color-mode": settings["color"],
                "sides": settings["sides"],
                "copies": "1",
                "fit-to-page": "true",
            },
        )
    except Exception:
        # Submission may have reached CUPS before the connection failed. Never auto-retry.
        raise PrinterError(
            "Delivery could not be confirmed. Check the printer before sending again."
        ) from None


def job_state(cups_id):
    try:
        return connection().getJobAttributes(cups_id).get("job-state")
    except Exception:
        return None


def cancel(cups_id):
    try:
        connection().cancelJob(cups_id)
    except Exception:
        raise PrinterError("The print job could not be cancelled. Check the printer.") from None
