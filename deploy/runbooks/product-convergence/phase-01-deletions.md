# PC-01 — Deletions (Confluence fossil + exams/quiz wave 1)

Pure subtraction; no behavior a user can observe changes except broken entrances disappearing.

## Contract

1a. Confluence fossil fully removed from runtime and console:
- Delete `src/api/v1/confluence.py`, `src/api/schemas/confluence.py`,
  `src/services/knowledge/confluence/`, `web/src/pages/confluence/` (8 files),
  `web/src/api/confluence.ts`.
- Remove `src/api/router.py` confluence include; remove `src/main.py` 503 comment block and
  `app.state.confluence_sync_service = None`; resolve `src/api/deps.py:65-68` stub by rg.
- Remove 4 confluence routes in `web/src/router.tsx`; delete `tests/api/test_confluence_disabled.py`
  if it exists.

1b. exams gone, quiz trimmed to load-bearing shim:
- Delete `src/api/v1/exams.py` + router registration + `web/src/pages/exams/` + `/exams` routes +
  `web/src/api/exams.ts` + `tests/api/test_exam_report_tenant_isolation.py`.
- `src/api/v1/quiz.py`: delete `POST /generate/stream`, `GET /list`,
  `GET /{quiz_id}/attempts/export`. Keep (load-bearing, verified): `POST /generate`,
  `GET /{quiz_id}`, `POST /{quiz_id}/submit`, `GET /{quiz_id}/attempts`, `DELETE /{quiz_id}`,
  share create/revoke, public `GET/POST /quiz/shared/{share_code}`. Mark module header DEPRECATED.
- Trim `web/src/api/quiz.ts` to surviving endpoints; delete `tests/api/test_quiz_exam_readiness.py`
  (verified: not wired into CI); update `tests/services/assistant/test_internal_exception_logging_gate.py`
  path mapping.
- Nav: remove `/exams` item in `web/src/layouts/AppLayout.tsx` + its i18n keys.

DB tables 011/041/042/043/050 stay (migrations are immutable history). CHANGELOG updated.

## Gate

```bash
uv run --all-packages --extra test pytest -q --no-cov tests/api tests/services/quiz tests/services/assistant
uv run --all-packages --extra dev ruff check src/ apps/ packages/
pnpm -C web type-check && pnpm -C web lint && pnpm -C web build
pnpm -C web e2e:opensource
```

## Evidence (fill on verify)

- [ ] pytest output
- [ ] ruff clean
- [ ] pnpm chain green
- [ ] e2e:opensource green
- [ ] `rg -i confluence src/ web/src/` → only the new connectors stack + KB-service internals
- [ ] `rg -i exams src/ web/src/` → zero
