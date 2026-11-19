# Contributing

**Last verified:** 2026-10-17 (source/command review; no live acceptance claim).

Thanks for contributing to AI Gateway. Platform services and operational tools are Rust; the console is React/TypeScript, with SDKs
and Docker Compose deployment tooling. The optional Python user-code sandbox is retained.

## Before You Start

- Read `README.md` for the local quickstart.
- Read `DEPLOY.md` before touching deployment, migration, backup, or restore paths.
- Never commit `.env`, credentials, API keys, connection strings, generated backups, logs, or screenshots with sensitive data.
- Keep changes scoped to one problem. Avoid unrelated rewrites and formatting churn.

## Local Setup

```bash
make doctor
make quickstart
make status
```

If your env file lives outside the repo, use `ENV_FILE=/path/to/.env` with Make targets.

## Useful Checks

Select the checks needed for the current task using the [task harness](docs/harness/README.md).
The commands below are alternatives for different work, not a sequence to replay after each edit:

```bash
git diff --check
make validate-example-config
make harness-check
make native-check
pnpm -C web type-check
pnpm -C web lint
pnpm -C web build
```

Use the pinned toolchains and affected tests in [the command catalog](docs/harness/commands.md).
Keep plans in `docs/plans/` and execution status in the existing program ledger. Reuse applicable
test results; only release/data/security requirements need separate durable evidence in `reports/`.
Build output and private captures stay external.

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
fix(deploy): validate release image identities
feat(assistant): enforce model admission
docs: update native contribution workflow
```

## Maintainer Review Expectations

Maintainers may ask for narrower scope, stronger tests, rollback notes, or explicit release blockers. A change is not release-ready until the relevant validation and operational gates pass or the remaining risk is documented.
