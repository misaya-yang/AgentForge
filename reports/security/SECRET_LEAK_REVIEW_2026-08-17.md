# Secret Leak Review — AI Gateway

**Date:** 2026-08-17
**Scope:** Current worktree + `git` history on `main` (`d333ee0`), focused on database DSNs, provider API keys, and bootstrap credentials.
**Repo visibility (unauthenticated):** `https://github.com/misaya-yang/AI--Platfform` returned HTTP 404 — treated as private or non-public.
**Overall risk:** MEDIUM (no live provider key or generated infra password in git; committed weak defaults remain).
**Recommendation:** CONDITIONAL — no emergency key rotation required for git leak; fix committed weak defaults and keep `.env` untracked.

No live secret values are written in this report.

---

## Executive Summary

| Severity | Count |
|----------|-------|
| CRITICAL | 0 |
| HIGH | 2 |
| MEDIUM | 4 |
| LOW | 3 |

**What is not leaked**

- Local `.env` holds a live DashScope key (`sk-…`) plus generated 64-hex `POSTGRES_PASSWORD`, `REDIS_PASSWORD`, `JWT_SECRET`, `GATEWAY_ENCRYPTION_KEY`, `GATEWAY_ASSISTANT_SHARED_SECRET`, and a password-bearing `GATEWAY_DATABASE__DSN`.
- Those exact values do **not** appear in any tracked file and have **0** `git log -S` hits across history.
- `.env` is gitignored, never added (`git ls-files` has no `.env`), mode `600`.
- `OPENAI_API_KEY`, `GOOGLE_API_KEY`, `GEMINI_API_KEY`, `ANTHROPIC_API_KEY`, `DEEPSEEK_API_KEY`, `TAVILY_API_KEY`, `KB_EMBEDDING_API_KEY`, `SILICONFLOW_API_KEY` are empty in the local env.
- Tracked `sk-` / `AIza` / `github_pat_` / `AKIA` hits are test canaries only.
- Helm `values.yaml` / `values-production.yaml` leave secret fields empty and require them at install time.
- Dockerfiles do not `COPY .env` or accept secret `ARG`s. Image publish passes only `APP_VERSION` / `VCS_REF`.
- Frontend has no `VITE_*` provider keys.

**What looks like a “database connection leak”**

Most DSN strings in the repo are **Compose/Helm interpolation templates**, not live passwords:

```text
postgresql://${POSTGRES_USER:-postgres}:${POSTGRES_PASSWORD:?…}@postgres:5432/gateway
```

Two committed files do embed **known-weak** credentials (not the local generated ones). That is the actual leak surface.

---

## Findings

### HIGH — `get_dsn()` still falls back to `postgres:postgres`

**File:** `database/cli.py:57`
**Test coverage:** NO for this fallback (Settings defaults are tested; CLI fallback is not).
**Status:** Previously reported 2026-08-02; still present.

`database/run_migration.py` was hardened to exit 2 when no DSN is configured. `database/cli.py` still returns:

```python
return "postgresql://postgres:postgres@localhost:5432/gateway"
```

`mask_dsn()` (`database/cli.py:60`) uses `r':([^:@]+)@'`, so a password containing `@` or `:` is only partially masked.

**Impact:** Anyone running `python database/cli.py` without env can authenticate to a local Postgres that still uses the stock password. The fallback is public in git.

**Fix:** Match `run_migration.py`: refuse to connect; print a redacted error; add a regression test that `cli.get_dsn` never contains `postgres:postgres`.

---

### HIGH — Published bootstrap admin password is a known default

**Files:**

- `src/core/auth/password.py:37` — `DEFAULT_PASSWORD = os.environ.get("DEFAULT_USER_PASSWORD", "ChangeMe-Admin-2026!")`
- `scripts/new/validate-env.sh:672` — accepts that same string as a “local default”
- `tests/scripts/test_validate_env_quickstart.py` — asserts the documented default

The local untracked `.env` uses **this same well-known password** for `DEFAULT_USER_PASSWORD`. Bootstrap user is `admin@example.com` (`database/migrations/005_account_permission_system.sql`).

Current runtime binds the gateway and frontend on all interfaces:

- `ai-gateway-backend` → `0.0.0.0:8080`
- `ai-gateway-frontend` → `0.0.0.0:8081`

Postgres / Redis / Qdrant stay on `127.0.0.1` (good).

**Impact:** Anyone who can reach `:8080`/`:8081` on this machine can try the documented admin password. This is not a git leak of a unique secret; it is a known-default credential on a LAN-exposed UI.

**Fix:** Generate a unique `DEFAULT_USER_PASSWORD` (init-env already can). Remove the source-code fallback. Stop treating `ChangeMe-Admin-2026!` as valid in `validate-env.sh`. Bind gateway/frontend to `127.0.0.1` unless sharing is intended.

---

### MEDIUM — Monitoring stack hardcodes DB password and Grafana admin

**File:** `docker/monitoring/docker-compose.monitoring.yml`

- `DATABASE_URL=postgresql://gateway:gateway123@postgres:5432/gateway` (L20)
- `POSTGRES_PASSWORD: gateway123` (L66)
- Redis has no password (L82–85)
- Grafana `admin` / `admin` (L145)
- Ports published on `0.0.0.0` (8001, 3000, 9090, 3001, 9093, 16686, OTLP)

This file is **not** the default quickstart (`docker-compose.yml`). It is still tracked and copy-pasteable.

**Fix:** Interpolate from env with `:?required` like the main compose file; bind ports to `127.0.0.1`; require a Redis password.

---

### MEDIUM — Dev helpers publish Postgres/Redis/Qdrant on all interfaces

**Files:**

- `scripts/new/setup-dev.sh:163` — `-p "${PG_PORT}:5432"` (no `127.0.0.1`)
- `docker-compose.override.yml.example:35-46` — `"5432:5432"`, `"6379:6379"`, `"6333:6333"`

Main `docker-compose.yml` correctly uses `127.0.0.1:${POSTGRES_PORT}:5432`. Copying the override example undoes that.

**Fix:** Prefix every host bind with `127.0.0.1:`.

---

### MEDIUM — Qdrant has no API key on the default stack

**File:** `docker-compose.yml` qdrant service.
Bound to `127.0.0.1` only, so LAN risk is low. Any local process can read/write vectors.

**Fix:** Optional `QDRANT_API_KEY` generated by `init-env.sh` and required in compose.

---

### MEDIUM — `docker compose config` will print interpolated secrets

Compose interpolates `${POSTGRES_PASSWORD}` into container env. `docker compose config` or `docker inspect` on this machine shows the live DSN. That is expected locally and is **not** a git leak. Do not paste compose render output into issues, chats, or reports.

---

### LOW — Documented local-only DSN fallbacks

Password-less or placeholder DSNs (not the live generated password):

| Location | Value |
|----------|--------|
| `src/config/settings.py:31` | `postgresql://localhost:5432/gateway` (no password; covered by `tests/config/test_settings_security.py`) |
| `apps/knowledge-service/.env.example:13` | `postgresql://kb_app:change_me@postgres:5432/gateway` |
| `.env.example` | `change_me_generate_with_openssl` placeholders only |

---

### LOW — Test canary keys only

`sk-abcdefghijklmnopqrstuvwxyz`, `sk-fixturecanary…`, `AIzaSyA1234…` appear only under `tests/`. Not live keys.

---

### LOW — Deleted public secret scanner

`scripts/__pycache__/scan_public_secrets.cpython-312.pyc` exists; `scripts/scan_public_secrets.py` is not in git. No automated public-tree secret gate beyond `tests/security/test_release_secret_regressions.py` (legacy `123456.dc` hash + Helm `required`).

---

## What was checked

| Check | Result |
|-------|--------|
| `.env` tracked? | No |
| `.env` ever committed? | No (`git log --all -- .env` empty except `.env.example` add) |
| Live local secret strings in worktree (excluding `.env`)? | No (except the **known** `ChangeMe-Admin-2026!` default) |
| Live local secret strings in git history? | 0 commits |
| Real `sk-[0-9a-f]{32}` in tracked files? | None |
| Hex-64 `PASSWORD=` / `SECRET=` assignments in HEAD? | None |
| AWS / GitHub / Slack live tokens in HEAD? | None |
| PEM / SSH private keys in HEAD? | None |
| `reports/` / `docs/` / `tmp/` live DSN or `sk-`? | None |
| `.env` file mode | `600` |
| Extra `.env.bak` / `.env.copy` | None found |
| `.claude/launch.json` | Untracked; no secrets |
| CI workflows | No secret echo; publish uses `GITHUB_TOKEN` only |
| Helm values | Empty secret slots |
| Running compose infra binds | Postgres/Redis/Qdrant on `127.0.0.1` |

**Not verified:** contents of GHCR image layers, GitHub Actions logs, private GitHub issues/PRs, other machines’ `.env` copies, whether the DashScope key was pasted into an external chat.

**Confidence:** HIGH for “is the live DashScope key or generated DB password in this git repo?” MEDIUM for “has that key ever left this laptop?”

---

## Recommendations

### Immediate

- [x] Fail closed in `database/cli.py`; harden `mask_dsn`.
- [x] Remove the Python fallback for `DEFAULT_USER_PASSWORD`.
- [x] Bind local Compose / setup-dev / override example / monitoring ports to `127.0.0.1`.
- [x] Parameterize the monitoring stack; drop `gateway123` and Grafana `admin/admin`.
- [ ] Do **not** commit `.env`, compose render output, or `docker inspect` env dumps.
- [ ] If this laptop is on a shared network: change `DEFAULT_USER_PASSWORD` to a unique value (current local `.env` still uses the documented bootstrap default so login keeps working).
- [ ] Recreate gateway/frontend only when you want the new loopback publish to take effect on the running stack.
- [ ] Rotate the DashScope key **only if** it was pasted outside this machine. Git does not contain it.

### Before next release

- [x] Delete the `postgres:postgres` fallback in `database/cli.py`; fail closed like `run_migration.py`.
- [x] Harden `mask_dsn`.
- [x] Stop shipping `ChangeMe-Admin-2026!` as a Python default. `validate-env.sh` still accepts it as a documented local bootstrap default so existing `.env` files keep validating.
- [x] Parameterize `docker/monitoring/docker-compose.monitoring.yml`; bind to localhost.
- [x] Bind `setup-dev.sh` and the override example to `127.0.0.1`.

### Do not do

- Do not print, commit, or paste the local `.env` values.
- Do not force-push or rewrite history for this finding — there is nothing to purge from git.

---

## Methodology

**Strategy:** SURGICAL secret hunt (repo is large; change-set is clean `main`).

**Techniques:**

- Worktree + `git log -S` match of every live local secret (existence counts only).
- Pattern scan for `sk-`, cloud tokens, PEM, hex-64 assignments, `postgresql://user:pass@`.
- Review of compose, Helm, CI, Dockerfiles, `.gitignore`, file mode, running port binds.
- Cross-check against `tests/security/test_release_secret_regressions.py` and the 2026-08-02 review item on `cli.py`.

**Limitations:** No gitleaks/trufflehog binary installed; history scan used exact local values plus regex, not entropy tools. Remote GitHub object search was not possible (`gh` missing; unauthenticated repo 404).
