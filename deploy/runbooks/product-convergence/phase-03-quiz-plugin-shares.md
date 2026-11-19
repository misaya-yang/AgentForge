# PC-03 — Quiz plugin final + share generalization

## Contract

1. `agent-plugins/ai-quiz/`: plugin.json + skills/quiz-generation.md (documents the
   `generate_quiz` tool contract, matching QUIZ_GENERATION_DEFINITION field names) +
   agents/quiz-expert.md. Data-only — **no mcp.json** (stdio children have no DB/KB/model
   credentials; the built-in tool already runs with KB context and no second LLM call).
2. Logic move: `quiz_generator.py` + `quiz_service.py` →
   `apps/assistant-service/src/assistant_service/core/quiz/`; `quiz_grader.py` stays in
   ai-gateway-core (gateway conversation_shares.py grades anonymous submissions — reverse
   imports forbidden); `exam_service.py` deleted (already orphaned by PC-01).
3. Share generalization: migration `083_artifact_shares.sql` (share_code, kind, title, payload,
   answer_keys, tenant_id, user_id, expires_at, max_attempts, attempt_count, created_at,
   revoked_at) + backfill INSERT..SELECT from quiz_shares; new
   `packages/ai-gateway-core/src/ai_gateway_core/sharing/artifact_share_manager.py`
   (kind-generic create/get_public/submit_attempt/revoke). `/quiz/shared/{share_code}` public
   routes become thin aliases over artifact_shares kind='quiz' — legacy links stay valid.
   `web/src/pages/assistant/components/Quiz/QuizShareDialog.tsx` re-points to the new endpoint.
4. quiz.py wave 2: delete `POST /generate` + share endpoints; permanent shim =
   GET /{quiz_id} + POST /{quiz_id}/submit + the two public aliases (load-bearing for
   QuizCard hydration/submission; re-pointing into assistant-service would churn the OpenAPI
   snapshot — out of scope).
5. Tests: `tests/services/quiz/test_quiz_generator.py` imports re-pointed; quiz-history.spec.ts
   seeding switches to DB seed or in-chat generation; delete dead quiz_share_manager.py.
6. README mentions ai-quiz as the plugin-ecosystem sample; CHANGELOG updated.

## Gate

```bash
uv run --all-packages --extra test pytest -q --no-cov tests/
uv run --all-packages --extra dev ruff check src/ apps/ packages/
pnpm -C web type-check && pnpm -C web lint && pnpm -C web build && pnpm -C web i18n:check
pnpm -C web e2e:opensource
make test-isolation validate-example-config migrate-status
```

## Evidence (fill on verify)

- [ ] full pytest + ruff + pnpm chain + e2e:opensource
- [ ] test-isolation / validate-example-config / migrate-status (or explicit not-run)
- [ ] `rg -n 'ai_gateway_core.quiz' src/` → zero reverse references
