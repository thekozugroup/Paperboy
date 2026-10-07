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
- Before the Rust migration, the running container matched all 30 application/web files.
  Combined SHA-256 at that checkpoint:
  `6ff750305446dea35c26fa1565a3e3a432fa82c92612f1ceb078a5cb7abb8d07`.

## Rust migration — converter checkpoint

The first Rust checkpoint replaced the converter with `paperboy-tools`, a native binary
with unsafe code forbidden in Paperboy's own crate. The service migration follows below.

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

## Rust migration — native service

The API, owner access/recovery, Resend client, polling, persistent queue, mDNS discovery,
IPP/CUPS communication, health command, and converter now use Rust. The production app
image has no Python interpreter. The previous Python implementation lives only under
`tests/legacy/` as a migration reference and is excluded from the runtime images.

- 31 native Rust tests passed, including request limits, server-computed authentication,
  exact senders, password/session compatibility, encrypted settings, daily limits, cutoff
  dates, 2,100-message backlog continuation, preserved duplicate IDs, owner claim races,
  cancellation/pause/revocation, unsafe retry rejection, and interrupted submission recovery.
  Formatting and Clippy with warnings denied passed.
- All 56 legacy reference tests passed after relocation. These test the frozen Python
  reference; they are additional compatibility evidence, not native-runtime test counts.
- The 24 app layout/accessibility checks, eight interactions, and two complete mocked
  onboarding flows (eight accessibility checks) passed against the native Rust preview.
- `scripts/runtime-qa.py` opens Python-era SQLite/Fernet/scrypt/session fixtures in the
  actual Rust Docker image. Existing people and settings survived; preparing jobs returned
  to the queue and interrupted delivery became uncertain. Native password recovery preserved
  print configuration and invalidated old sessions.
- The Docker test discovered and paired `ippeveprinter`, converted the owner-requested
  test page through the isolated Unix socket, submitted it using native Rust IPP to CUPS,
  and observed completion. Exactly one sanitized PDF reached the software printer;
  temporary work files were removed. Restart retained the completed job and duplicate records.
  All fixtures use disposable volumes and synthetic credentials; Resend resolves to loopback
  in the fixture container. No real account or household printer participates.
- Image conversion coverage now also includes monochrome fax TIFF, 16-bit grayscale TIFF,
  and CMYK TIFF. Subprocess stdout is bounded to 1 MiB; deadlines kill the process group.
- Both images include a SHA-256 manifest of the Cargo inputs and Rust source used to build
  their native binaries, allowing comparison with the checked-out source.
- The committed native service was rebuilt and restarted locally. The running Rust source
  manifest and served HTML/CSS/JavaScript match the checkout. Its owner screen passed six
  desktop/mobile/light/dark accessibility checks; no account or printer actions were taken
  against the live data volume. Both production services are healthy.
- The converter now handles Docker's SIGTERM stop signal and exits normally while idle,
  within the three-second integration-test bound. The application stops its queue worker
  as shutdown begins. The complete Docker migration/delivery test passed with this change.
- GitHub's Rust, Docker/reference, and native-browser jobs passed for service commit
  `2f244c3`. The shutdown follow-up repeats these checks on its push.

Local performance sample: fresh, uninitialized production containers, same Docker/Colima host,
10 warm-up requests and 100 sequential `/api/auth/status` requests per implementation:

| App process | Resident memory | Median response | 95th percentile |
| --- | ---: | ---: | ---: |
| Python baseline | 95,536 KiB (93.3 MiB) | 2.764 ms | 4.866 ms |
| Rust service | 6,784 KiB (6.6 MiB) | 1.083 ms | 1.538 ms |

Memory is process RSS, excluding CUPS and the converter. These are local idle measurements,
not a whole-container budget, loaded-printing benchmark, or universal fastest-performance claim.
The baseline image was `d5bd6a977cc7`; measured Rust image was `85a88e0f3f36`.

## Limits of this evidence

- Resend was verified against current official receiving API documentation. No live account
  credentials were supplied. Provider behavior in automated tests is synthetic.
- The IPP printer was a real software printer (`ippeveprinter`), not physical hardware.
  CUPS completion does not prove a page emerged from a household printer.
- UI checks used desktop Chrome with responsive viewports, not native iOS/Android hardware.
  The interface follows Apple HIG principles; Apple has not certified it.
- Only the local ARM64 Docker images were exercised here. GitHub CI builds Linux AMD64.
- Python reference/PDF-validation dependencies emit deprecation warnings; they do not fail
  the checks and are absent from the production app.
- Initial acceptance remains: supply your Resend key/address, pair your physical printer,
  add your sending address, and email a sample attachment. Confirm the actual output.

GitHub Actions repeats backend, image-build/health, and browser checks on pushes and pull requests.
