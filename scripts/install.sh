#!/bin/sh
# Install from an actual GitHub release. Run from the folder where you want Compose saved.
set -eu
release=${PAPERBOY_INSTALL_VERSION:-latest}
case "$release" in
  latest) base=https://github.com/thekozugroup/Paperboy/releases/latest/download ;;
  *) case "$release" in *[!0-9.v]*|'') echo 'Use a stable version such as 0.3.0.' >&2; exit 1;; esac
     base="https://github.com/thekozugroup/Paperboy/releases/download/v${release#v}" ;;
esac
for file in compose.release.yaml compose.linux.yaml; do
  if [ -e "$file" ]; then echo "$file already exists. Install in an empty folder, or use your existing Compose files." >&2; exit 1; fi
done
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' 0 HUP INT TERM
for file in compose.release.yaml compose.linux.yaml SHA256SUMS; do
  curl --fail --location --proto '=https' --tlsv1.2 "$base/$file" -o "$temporary/$file"
done
(cd "$temporary" && if command -v sha256sum >/dev/null 2>&1; then sha256sum --check SHA256SUMS; else shasum -a 256 --check SHA256SUMS; fi)
cp "$temporary/compose.release.yaml" "$temporary/compose.linux.yaml" .
if [ ! -e .env ]; then
  socket_gid=$(stat -c %g /var/run/docker.sock 2>/dev/null || printf 0)
  umask 077
  printf 'PAPERBOY_DOCKER_GID=%s\n' "$socket_gid" > .env
  if [ "$release" != latest ]; then printf 'PAPERBOY_PIN_VERSION=%s\n' "${release#v}" >> .env; fi
fi
printf '\nInstalled Paperboy Compose files. Start on a Linux NAS with:\n\ndocker compose -f compose.release.yaml -f compose.linux.yaml --profile updates up -d\n\nOpen http://YOUR-SERVER-IP:8025. Updates are manual until enabled in Settings.\n'
