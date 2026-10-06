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
