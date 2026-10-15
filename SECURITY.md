# Security Policy

## Reporting a Vulnerability

Please do not report security vulnerabilities through public GitHub issues.

Use GitHub private vulnerability reporting for this repository when available. If private reporting is not available, contact the maintainers through the repository owner profile and include only enough detail to establish impact and reproduction. Do not include live secrets, production data, or third-party credentials.

## What to Include

- Affected component or path.
- Impact and attacker capability.
- Minimal reproduction steps using local or test data.
- Relevant logs with secrets redacted.
- Whether the issue affects authentication, authorization, tenant isolation, service-to-service HMAC, provider keys, signed artifact URLs, rate limits, sandboxing, migrations, backups, or release workflows.

## Supported Versions

Security fixes target the current `main` branch unless a maintained release branch is explicitly documented in `CHANGELOG.md` or release notes.

## Secret Handling

- Never commit `.env` or real credentials.
- Do not paste API keys, database URLs, JWT secrets, Redis passwords, HMAC keys, signed URLs, or session tokens into issues, pull requests, logs, or screenshots.
- Use `.env.example` for names and placeholder values only.
- The validation scripts should report missing or weak secret names without printing their values.

## Deployment and Data Safety

Production deployment, production migrations, restore operations, credential rotation, package publishing, and destructive Docker commands require explicit maintainer approval. Prefer dry-runs, backups, rollback notes, and local reproduction before any shared-environment action.

## Security Design Areas

AI Gateway relies on:

- Public gateway entrypoints with private internal assistant and knowledge services.
- HMAC-protected gateway-to-service calls.
- Explicit CORS and auth-domain validation for non-local deployments.
- Runtime config injection for frontend browser settings.
- Release gates for metrics, readiness, migrations, rollback, and no-secret output.

Changes in these areas should include targeted regression tests and a release-risk note.
