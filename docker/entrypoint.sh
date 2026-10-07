#!/bin/sh
set -eu
mkdir -p /data /run/cups /var/spool/cups /var/cache/cups
chown paperboy:paperboy /data
cupsd -f &
cups_pid=$!
runuser -u paperboy -- paperboy &
app_pid=$!
trap 'kill "$app_pid" "$cups_pid" 2>/dev/null || true; wait || true' TERM INT EXIT
# Stop the container if either required process exits; Docker can then restart both.
while kill -0 "$app_pid" 2>/dev/null && kill -0 "$cups_pid" 2>/dev/null; do
  sleep 2
done
exit 1
