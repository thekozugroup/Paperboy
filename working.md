# Paperboy

Goal: a minimal Docker app that prints emailed files from approved family addresses.

Current: implementation and apple.com-inspired UI refinement complete and locally verified.
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

Next: configure real Resend receiving and a physical printer, then confirm a household attachment prints.
See evidence/verification.md for the precise verification scope.
