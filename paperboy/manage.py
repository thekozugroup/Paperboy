"""Local recovery commands. Run with the service stopped."""

import os
import sys

from paperboy.store import Store


def main():
    if sys.argv[1:] != ["reset-password"]:
        raise SystemExit("Usage: python -m paperboy.manage reset-password")
    store = Store(os.getenv("PAPERBOY_DATA_DIR", "./data"))
    with store.db() as db:
        db.execute("DELETE FROM settings WHERE key IN ('password_hash','password_salt')")
        db.execute("DELETE FROM sessions")
    (store.root / "setup.code").unlink(missing_ok=True)
    print("Owner access reset. Restart Paperboy and use the new setup code in its logs.")


if __name__ == "__main__":
    main()
