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

## Versioned releases and updates — v0.3.1

- Published [v0.3.0](https://github.com/thekozugroup/Paperboy/releases/tag/v0.3.0) from
  `30b0ce420584e75f003199c1a7c6873f25cdb02a`, followed by the reliability patch
  [v0.3.1](https://github.com/thekozugroup/Paperboy/releases/tag/v0.3.1) from
  `a2963965920969e60e8aada97102691304ca1d8d`.
- The [v0.3.1 release workflow](https://github.com/thekozugroup/Paperboy/actions/runs/37693872529)
  passed native Rust, Docker/reference, browser, AMD64, ARM64, and publication jobs.
  Both architectures ran the ten converter checks on native runners. Public app/converter
  `0.3.1`, `stable`, and `latest` tags are published. Anonymous registry reads verified
  the versioned manifests contain both Linux architectures.
- Forty-four native tests passed, including transaction recovery, unhealthy-startup rollback,
  interrupted self-handoffs, protected project scope, tampered journal references, monotonic
  cached image channels, and authenticated update controls. Formatting and Clippy passed;
  the existing 56 Python reference checks remain compatibility evidence.
- Browser checks passed 24 app and eight setup layout/accessibility cases, eight interactions,
  and 30 update cases across desktop, 390px, 320px, light, dark, and reduced motion.
  Update-provider scenarios are explicit fixtures, including the fictional 0.4.0 screenshots.
  Taste Skill preserve-mode guidance was read from the official site's linked skill source.
- The actual published installer ran inside Linux and verified the Compose asset checksums,
  detected Docker's socket group, produced a version pin when requested, validated both
  standard and host-network Compose variants, and refused an existing installation.
  The latest installer follows stable and leaves automatic updates off.
- `scripts/updates-runtime-qa.py` exercised published **0.3.0 → 0.3.1** twice: manual and
  automatic, with the original 0.3.0 controller. A third automatic scenario used the patched
  0.3.1 controller to exercise its own install/cached-tag/handoff path against the older app.
  Each stack began with old stable/latest images deliberately cached. A later Compose restart
  without pulling retained 0.3.1 and saved jobs. These are isolated stacks on a test daemon;
  the script intentionally changes that daemon's cached channel tags.
- Every scenario reported the new release while pinned, rejected installation/automatic
  policy changes under that pin, waited for a held active CUPS job without replacing any
  service early, then replaced app/converter/updater. Owner session, password hashes, encrypted
  API key, approved sender, printer queue, waiting jobs, completed status, and uncertain status
  survived. Unsafe retries and installing the current version were rejected. Exactly one PDF
  reached the software printer per scenario; no retired containers remained.
- Linux update acceptance ran on the local ARM64 Colima daemon. The Resend host resolves to
  loopback inside these fixtures, and credentials are synthetic. No physical printer or real
  Resend account was used. A preliminary 0.2.9 version-label fixture also passed both modes;
  it was not a historically published 0.2.9 release.
- The primary local stack now runs the public v0.3.1 images at `http://127.0.0.1:8025`, reusing
  its original four data/printer volumes and adding one update-control volume. App/converter
  health checks pass; the controller heartbeat is current, latest is 0.3.1, and automatic
  updates are off. Source hash manifests and served HTML/CSS/JavaScript match the checkout.
  Its owner screen passed six further desktop/mobile/light/dark accessibility checks.
  No owner account was claimed and no print was requested on this primary stack.
- Release asset SHA-256 values: `compose.release.yaml`
  `e44518c57c83ab1c9c1f2a26758400b4902231b3cff1d4879440ea447a299339`;
  `compose.linux.yaml` `cd14e3fd6f6b180a2575b625e80e9c6191b338f16e817325bada632058cce50b`.
  Both are covered by the published `SHA256SUMS`. Published version tags are now protected
  by the release preflight; future fixes require a new version.

## Limits of this evidence

- Resend was verified against current official receiving API documentation. No live account
  credentials were supplied. Provider behavior in automated tests is synthetic.
- The IPP printer was a real software printer (`ippeveprinter`), not physical hardware.
  CUPS completion does not prove a page emerged from a household printer.
- UI checks used desktop Chrome with responsive viewports, not native iOS/Android hardware.
  The interface follows Apple HIG principles; Apple has not certified it.
- Update acceptance used local ARM64 Linux Docker. Release builds and conversion checks used
  native AMD64 and ARM64 GitHub runners. Unraid hardware and Docker Desktop were not exercised.
- Python reference/PDF-validation dependencies emit deprecation warnings; they do not fail
  the checks and are absent from the production app.
- Initial acceptance remains: supply your Resend key/address, pair your physical printer,
  add your sending address, and email a sample attachment. Confirm the actual output.

GitHub Actions repeats backend, image-build/health, and browser checks on pushes and pull requests.
