# Migration reference

The previous Python service is retained here only as a regression reference for the
existing 56 compatibility/security checks. It is excluded from the Docker runtime.
Paperboy's production service and converter are the native binaries in `rust/`.

Native behavior is tested with `cargo test`, `scripts/runtime-qa.py`, and browser checks
against the Rust server. The Docker runtime check creates Python-era SQLite, Fernet,
password, and session fixtures, then opens them in Rust and verifies owner recovery
and software-printer delivery. These fixtures do not use real credentials or hardware.
