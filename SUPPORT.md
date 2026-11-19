# Support

AI Gateway is an open-source project. Support is best-effort unless a separate commercial or internal agreement exists.

## Where to Ask

- Use GitHub issues for reproducible bugs, documentation gaps, and feature requests.
- Use pull requests for proposed fixes.
- Use `SECURITY.md` for suspected vulnerabilities.

## Before Opening an Issue

Please include:

- Repository commit or release version.
- Operating system and Docker version when relevant.
- Exact command that failed.
- Redacted logs or screenshots.
- Whether you used root `.env` or `ENV_FILE=/path/to/.env`.
- Validation output from `make validate-config`, `make status`, or the relevant test command when safe.

Do not include secret values, provider keys, database URLs, tokens, private documents, or production data.

## Response Expectations

Maintainers prioritize issues with clear reproduction steps, current-version evidence, and safety impact. Questions that require private credentials, production data, or external dashboards may be closed until a local reproduction is provided.
