# Contributing

Thanks for contributing to AI Gateway. This repository contains a FastAPI gateway, assistant and knowledge microservices, a React frontend, SDKs, and Docker Compose deployment tooling.

## Before You Start

- Read `README.md` for the local quickstart.
- Read `DEPLOY.md` before touching deployment, migration, backup, or restore paths.
- Never commit `.env`, credentials, API keys, connection strings, generated backups, logs, or screenshots with sensitive data.
- Keep changes scoped to one problem. Avoid unrelated rewrites and formatting churn.

## Local Setup

```bash
cp .env.example .env
make validate-config
make quickstart
```

If your env file lives outside the repo, use `ENV_FILE=/path/to/.env` with Make targets.

## Useful Checks

Run the checks that match your change:

```bash
git diff --check
docker compose --env-file .env.example config --quiet
uv run ruff check src apps packages tests
uv run --extra dev --extra test pytest -q --no-cov tests/scripts/test_web_runtime_config_entrypoint.py tests/scripts/test_validate_env_quickstart.py
pnpm -C web type-check
pnpm -C web lint
pnpm -C web build
```

Generated planning and harness files under root `docs/` are ignored by Git. Keep
durable operational follow-ups in tracked locations such as `deploy/runbooks/`.

If a check cannot run locally, include the exact command, the blocker, and what a passing result would prove.

## Pull Requests

Every pull request should include:

- What changed and why.
- The user-visible or operational impact.
- Validation commands and results.
- Screenshots or Playwright artifact paths for UI changes.
- Migration, rollback, and data-safety notes for database or deployment changes.
- A statement that no secrets were committed.

## Security-Sensitive Changes

Do not open public issues for suspected vulnerabilities. Follow `SECURITY.md`.

Security-sensitive areas include authentication, HMAC service communication, session handling, rate limits, CORS, signed document URLs, sandboxing, provider keys, migrations, backups, and release workflows.

## Commit Style

Use concise imperative commit messages, for example:

```text
Add release config validation
Harden assistant model selection
Document open-source contribution flow
```

## Maintainer Review Expectations

Maintainers may ask for narrower scope, stronger tests, rollback notes, or explicit release blockers. A change is not release-ready until the relevant validation and operational gates pass or the remaining risk is documented.
