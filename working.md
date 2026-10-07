# Paperboy

Goal: a minimal Docker app that prints emailed files from approved family addresses.

Current: native Rust service and isolated Rust converter implemented and verified locally.
Saved settings, owner access, sessions, and queue data remain compatible. Committing/pushing
the service migration to main, then verifying the committed Docker build and GitHub checks.
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
Docker checks verified legacy-data migration, recovery, discovery, conversion, software-printer
delivery, temporary-file cleanup, and no repeated completed job after restart.

Measured idle app-process RSS: Python 93.3 MiB; Rust 6.6 MiB on this host. Document engines
and CUPS are separate from that measurement. No claim of universal fastest performance.

Next: verify main's committed Docker build and GitHub checks. Household acceptance still
needs a real Resend account and physical printer.
See evidence/verification.md for the precise verification scope.
