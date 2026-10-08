# Paperboy

Goal: a minimal Docker app that prints emailed files from approved family addresses.

PDF attachment fix: accept Resend's exact `cdn.resend.app` download host. A configured
server received a valid PDF from that host, but the attachment allowlist rejected it
before conversion and the activity view showed generic PDF-export advice. Preserve
HTTPS, public-address validation, DNS pinning, redirect/proxy denial and download bounds.
Regression coverage accepts the exact CDN and rejects deceptive hosts, unrelated
`resend.app` subdomains, credentials, HTTP and alternate ports. Production credentials,
signed URLs, personal email data and attachment contents remain outside this repository.
The same PDF exposed a second failure: page validation parsed `File size:` as page
dimensions. Limit that check to `Page` size lines; retain oversized-page rejection.
The Docker conversion fixture now contains over 14,400 bytes to cover this regression.
Version 0.3.2 prepares these fixes for the normal coordinated release updater.

Current: v0.3.1 is published and running locally from its public Docker images.
Release source: a2963965920969e60e8aada97102691304ca1d8d.
Release: https://github.com/thekozugroup/Paperboy/releases/tag/v0.3.1.
Verified: native AMD64/ARM64 builds and conversions, anonymous image access, checksummed
Linux installation, manual/automatic published upgrades, version pins, active-print waiting,
preserved owner/key/people/printer/queue, duplicate safety, and cached-channel Compose restart.
The patched updater's installation path passed a further automatic-upgrade check.
Taste Skill preserve-mode polish passed 62 responsive/theme/accessibility checks and eight
interactions. The local app and converter source manifests and served assets match the release;
the companion is connected and automatic updates remain off until enabled.

Previous: Rust migration complete. Native service and isolated converter ship on main,
with verified Docker builds. Saved settings, owner access, sessions, and queue remain
compatible. Clean Docker shutdown and software-printer delivery are verified.
Public repository: https://github.com/thekozugroup/Paperboy.

Acceptance:
- Resend receiving API polling without public inbound ports.
- Server-computed sender authentication plus an exact approved address list.
- IPP/AirPrint discovery with manual local printer address fallback.
- Persistent queue, bounded conversion, deduplication, safe restart behavior.
- Guided setup and clear activity, sender management, and preferences.
- Real Docker build and document conversion verification; browser desktop/mobile/accessibility QA.
- Public GitHub repository `thekozugroup/Paperboy`, committed as thekozugroup@gmail.com.

Release boundary: no real Resend credentials or family printer have been supplied.
Live delivery and physical output need a user's configured account and printer.

Completed: Docker app and networkless converter; Resend polling and sender authentication;
IPP discovery/manual pairing; persistent queue and crash safety; guided setup; responsive
Activity/People/Settings UI; owner access and recovery; print limits/defaults; documentation;
56 automated backend checks, 32 browser layout/accessibility checks, mocked onboarding flows,
real Docker conversions, and virtual IPP delivery.

UI refinement: white canvas, larger headings, more open spacing, quieter navigation,
pill-shaped controls, open activity/people lists, and consistent setup/settings surfaces.
Verified: 32 layout/accessibility checks, complete mocked setup flows, keyboard access,
44px touch targets, and six owner-screen checks against the rebuilt Docker service.

Rust migration: native API, owner access/recovery, Resend polling, mDNS discovery, IPP/CUPS
communication, queue, conversion, and health commands. No Python runtime in the app image.
31 native checks and 56 legacy reference checks passed; 32 UI checks and guided setup passed.
The rebuilt native Docker owner screen passed six responsive/theme/accessibility checks.
Docker checks verified legacy-data migration, recovery, discovery, conversion, software-printer
delivery, temporary-file cleanup, and no repeated completed job after restart. The converter
passed ten file checks and exits cleanly on Docker's stop signal; the worker stops when
service shutdown begins. GitHub verified all three jobs for the service migration.

Measured idle app-process RSS: Python 93.3 MiB; Rust 6.6 MiB on this host. Document engines
and CUPS are separate from that measurement. No claim of universal fastest performance.

Next: install using docs/updates.md on each server. Choose manual updates, enable automatic
updates in Settings, or set a version pin. Configure Resend and a physical printer, approve
a sending address, and confirm the actual output of an emailed attachment.
See evidence/verification.md for the precise verification scope.
