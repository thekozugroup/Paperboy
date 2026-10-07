# Paperboy

Goal: a minimal Docker app that prints emailed files from approved family addresses.

Current: Adding versioned Docker releases and per-server update controls.
Required: public AMD64/ARM64 images, release assets, optional manual/automatic updater,
version pinning, safe replacement/recovery, and browser/container evidence.

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

Next: configure Resend and a physical printer in the local UI, approve a sending address,
and confirm the actual output of an emailed attachment. No further migration work remains.
See evidence/verification.md for the precise verification scope.
