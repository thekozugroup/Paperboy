# Paperboy verification

Verified locally on October 6, 2026, on Apple Silicon macOS with Docker/Colima.

| Check | Result |
| --- | --- |
| Backend/security suite | 56 passed |
| Python lint and formatting | Passed |
| JavaScript syntax | Passed |
| Docker Compose standard and Linux host-network configurations | Valid |
| Both Docker images built | Passed, linux/arm64 |
| App and isolated converter health checks | Healthy |
| CUPS connection as the non-root app user | Passed |
| DOCX, XLSX, PPTX, PDF, PNG, UTF-8 TXT conversion in Docker | Each produced a valid one-page PDF without embedded attachments |
| Real CUPS → virtual IPP printer delivery | Accepted and completed; generated PDF reached the virtual printer |
| Final-image pipeline with an approved synthetic email | DOCX converted through the isolated service, submitted to CUPS, completed at virtual printer |
| Duplicate ingestion and temporary-file removal | One job, working files removed |
| UI layout/accessibility matrix | 24 checks: 1440/768/390/320px × light/dark × Activity/People/Settings |
| UI accessibility | Zero WCAG A/AA findings from axe in the checked screens |
| Guided setup accessibility | 8 checks across two complete flows, with explicitly mocked Resend/printer providers |
| UI interactions | Filters, add/cancel sender form, remove/keep confirmation, disclosures, setup validation and completion passed |
| Browser runtime errors | None in the checked flows |
| Real Docker owner setup UI | Setup code and password created an owner session; email setup displayed |

Tests cover unapproved addresses, forged authentication headers, missing/failed Resend verdicts,
exact sender matching, encrypted API-key storage, private settings, CSRF, cross-origin requests,
sign-in throttling, oversized/chunked requests, document/page limits, removal of PDF links and
embedded files, daily limits, duplicate IDs, backlog continuation, cancellation/pause/revocation
during conversion, interrupted submissions, unsafe retries, history cleanup, owner recovery,
and prevention of preview-mode overwrites of a live configuration.

The isolated converter was inspected with Docker: network mode `none`, non-root user,
read-only root filesystem, and 768 MiB memory limit. The application talks to it through a
shared Unix socket; it has no API key or access to the application's persistent data volume.

## October 6 UI refinement

The revised interface uses apple.com-inspired typography, white space, pill-shaped actions,
quiet navigation, and open activity/people lists. The owner screen, guided setup, and all
three app sections share the updated design tokens. No printing, authentication, or queue
logic changed.

- Repeated all 24 app layout/accessibility checks and both complete mocked setup flows
  (8 setup accessibility checks). No accessibility findings or browser runtime errors.
- Inspected desktop, mobile, light, dark, and setup screenshots. Updated the README preview.
- Checked 44px interactive targets across 18 app variants, primary-button hover contrast,
  and keyboard access through the skip link to the first main action.
- Rebuilt and restarted the local Docker app. Its owner screen passed six responsive/theme
  checks, with no accessibility findings, overflow, or runtime errors. No owner setup or
  printer action was performed during this refinement.
- The running container matches all 30 local application/web files. Combined SHA-256:
  `6ff750305446dea35c26fa1565a3e3a432fa82c92612f1ceb078a5cb7abb8d07`.

## Rust migration — converter checkpoint

The converter service now runs `paperboy-tools`, a native Rust binary with unsafe code
forbidden in Paperboy's own crate. The API/queue migration is the next stage.

- Six Rust checks passed: PDF rebuilding, invalid options, process deadlines, request body
  bounds without a content length, and concurrency limits. Clippy and formatting passed.
- The actual read-only, networkless Docker image passed seven repeatable conversion checks
  (`scripts/converter-qa.py`): PDF sanitization, RTF, PNG, UTF-8 text, multipage TIFF, and
  rejection of locked PDFs and excessive pages.
- DOCX, XLSX, PPTX, PDF, PNG, TXT, and TIFF also passed through the live Unix-socket service.
  Output PDFs opened successfully, used Letter pages, and contained no links or embedded files.
- Both services are healthy. The existing app accepts the Rust converter's response contract.
- Rust orchestrates LibreOffice and Poppler; their native parsers remain in the isolated
  container. This is not a claim that every dependency or printer driver is memory-safe.

## Limits of this evidence

- Resend was verified against current official receiving API documentation. No live account
  credentials were supplied. Provider behavior in automated tests is synthetic.
- The IPP printer was a real software printer (`ippeveprinter`), not physical hardware.
  CUPS completion does not prove a page emerged from a household printer.
- UI checks used desktop Chrome with responsive viewports, not native iOS/Android hardware.
  The interface follows Apple HIG principles; Apple has not certified it.
- Only the local ARM64 Docker images were exercised here. GitHub CI builds Linux AMD64.
- Upstream PyMuPDF and Starlette emit deprecation warnings; they do not fail the suite.
- Initial acceptance remains: supply your Resend key/address, pair your physical printer,
  add your sending address, and email a sample attachment. Confirm the actual output.

GitHub Actions repeats backend, image-build/health, and browser checks on pushes and pull requests.
