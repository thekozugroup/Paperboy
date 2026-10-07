# Install and update Paperboy

Each server checks the official [Paperboy releases](https://github.com/thekozugroup/Paperboy/releases).
Production images are public at `ghcr.io/thekozugroup/paperboy` and
`ghcr.io/thekozugroup/paperboy-converter`, for Linux AMD64 and ARM64.
No GitHub login, compiler, source checkout, inbound internet port, or subscription is required.

## Linux and Unraid

Install Docker Engine and Docker Compose v2. On Unraid, install a Compose manager that provides
Docker Compose v2, and place the installation folder on persistent storage, such as
`/mnt/user/appdata/paperboy`. This release is a Compose stack, not a Community Applications listing.

Download `install.sh` from the latest release, review it, and run it from an empty folder:

```sh
mkdir -p paperboy
cd paperboy
curl -fL https://github.com/thekozugroup/Paperboy/releases/latest/download/install.sh -o install.sh
sh install.sh
docker compose -f compose.release.yaml -f compose.linux.yaml --profile updates up -d
docker compose -f compose.release.yaml logs paperboy
```

Open `http://YOUR-SERVER-IP:8025`, enter the setup code from the logs, and finish setup.
Host networking on Linux allows local printer discovery. Keep port 8025 on your trusted LAN.
The installer downloads release files and checks their SHA-256 checksums. It never overwrites an
existing installation and never starts Docker or enables automatic updates on its own.

The updater needs access to Docker's socket. The installer records its group in `.env`:

```sh
stat -c %g /var/run/docker.sock
# .env: PAPERBOY_DOCKER_GID=<the number above>
```

Only the optional companion has this Docker access; the web app and isolated converter do not.
The companion runs as uid 1000, has no app-data/API-key mount, accepts a small request protocol,
and limits replacement to official images and containers labeled for this Compose project.
Docker socket access is privileged host access: run this companion only on your trusted server.

Named Docker volumes persist settings, encryption keys, approved senders, print records, CUPS
configuration/spool, and update preferences. On Unraid, keep Docker's storage location persistent
and make sure the stack is configured to start after a server reboot in your Compose manager.
Never use `docker compose down -v` on an installation you want to keep.

## Choose how each server updates

- **Manual (default):** Settings → Updates shows a new release. Select **Install update**.
- **Automatic:** turn on **Automatic updates** in the same screen. New stable releases are checked
  hourly and installed once printing is idle. Each server stores its own preference.
- **Pinned:** set `PAPERBOY_PIN_VERSION=0.3.0` in `.env`, then recreate the stack. The updater
  still shows newer releases but cannot change this server's chosen version.

To install a chosen release initially:

```sh
PAPERBOY_INSTALL_VERSION=0.3.0 sh install.sh
```

To remove a pin, delete `PAPERBOY_PIN_VERSION` from `.env` and recreate all services with the
same Compose command. Automatic updates remain a separate preference.
Use the same files and project name for future changes so the same volumes are reused.

## Install without the companion

Omit `--profile updates`. Paperboy still checks GitHub every six hours and shows available
releases in Settings. Your server can then manage image updates. Update both images together:

```sh
docker compose -f compose.release.yaml -f compose.linux.yaml pull
docker compose -f compose.release.yaml -f compose.linux.yaml up -d --wait
```

For macOS/Windows or a localhost-only Linux install, omit `-f compose.linux.yaml` and use a
manual printer IP address if multicast discovery is unavailable. The companion needs a local
Docker socket; Docker Desktop/remote Docker contexts have not been certified for automatic updates.

## Upgrade an existing source install

Keep the existing project name `paperboy`. Download the release Compose files into your current
installation folder; do not create a new project or erase your data. Inspect existing `.env`
settings, set the socket group if enabling the updater, then start the release stack using the
command above. Its volume names match the original `compose.yaml` installation.
Source development continues to use `compose.yaml` and local builds.

## What happens during an update

The updater verifies the latest stable GitHub release and official image labels, downloads both
versioned images, and asks Paperboy to stop starting new prints. Current conversions/deliveries
finish. Waiting files remain saved. It then stops the app, replaces the converter and app using
their existing mounts/settings/resource limits, and verifies both health checks.

If startup fails, it restores the previous containers. A durable journal recovers interruptions
during replacement. Volumes are never deleted or automatically rolled back; restoring old print
records could print pages twice. Completed and uncertain submissions retain their existing rules.
Only releases with compatible, additive data migrations should be published to this stable channel.
After success the updater replaces itself. A short reconnect delay in the browser is expected.

If recovery needs attention, the queue stays held while an incomplete journal exists. Inspect
the updater logs before manually changing containers. Do not delete the journal or restore a
database snapshot while jobs might have reached the printer.

```sh
docker compose -f compose.release.yaml logs updater
```

An unreachable GitHub service or unavailable image does not stop normal printing. A failed
release is not repeatedly auto-installed; select **Check again**, then **Install update** to retry.

## Publish the next release

Update `Cargo.toml`/`Cargo.lock`, Dockerfile's default version, and `docs/releases/VERSION.md`.
Commit and push to main. Run the **Release** workflow with the matching version. It runs all
checks, builds and validates native AMD64/ARM64 images, verifies anonymous registry access,
publishes the GitHub release assets, and promotes `stable`/`latest`.
Only published stable releases containing installation assets are offered to installed servers.
Never overwrite a published version tag; create a new patch release for fixes.
