# Phase 00 - Integrate and repair the staged Grok correctness batch

- PHASE_ID: SPD-00
- FEATURE_ID: SPD-F001
- DEPENDS_ON: none

## Outcome

The staged Grok hardening remains intact and every confirmed integration regression is
closed through its public or runtime entry point.

## Scope

In:

- Platform-admin bootstrap role and migration compatibility.
- Assistant new-chat persistence, restore epochs, terminal stream projection, and scoped browser state.
- Knowledge embedding cancellation slot ownership.
- Public quiz attempt lifecycle and typed failure mapping.
- Connector callback console redirect selection.

Out:

- Provider canaries, hot-path tuning, bundle splitting, and broad Docker acceptance.
- Weakening collision, tenant, path, SSRF, token-encryption, or tool-pairing safeguards.

## Done when

- [ ] Bootstrap platform admin reaches global APIs while tenant admins remain denied.
- [ ] New chat persists or reconciles a same-owner 409 and cannot be overwritten by stale restore work.
- [ ] Quiz attempts use an opaque start token, per-attempt deadlines, typed error mapping, and atomic single consumption.
- [ ] Cancelled embedding workers release capacity only when the underlying thread finishes.
- [ ] Terminal stream state and user-scoped local storage cannot reopen or cross users.
- [ ] The SPD-00 targeted Python/Web suite has zero failures; the PostgreSQL migration gate runs in local Docker.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Targeted Python | `uv run --all-packages --extra test pytest -q --no-cov tests/api/test_platform_admin_guard.py tests/api/test_service_model_override_validation.py tests/api/test_quiz_api.py tests/api/test_artifact_shares.py tests/database/test_artifact_share_migration.py tests/services/knowledge/test_document_upload_generation.py` | Guards, share lifecycle, migration, and cancellation pass. |
| Targeted Web | `node --experimental-strip-types --test web/src/pages/assistant/lastModel.test.ts web/src/features/chat/newChatStream.test.ts` plus relevant reducer tests | Session and per-user browser state contracts pass. |
| Static | `pnpm -C web type-check` and changed-file Ruff | Modified code is type/lint clean. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Do not run migrations outside the repository-owned local Compose database.
