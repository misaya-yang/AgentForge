# PC-02C — Default model de-hardcoding (track C, worktree)

One deployment default, served by the platform; no concrete model ID in any SDK default.

## Contract

1. `DEFAULT_MODEL` (unprefixed) env var = single source. Assistant-service settings
   (`apps/assistant-service/src/assistant_service/config/settings.py`) gains `default_model`;
   new `core/models/defaults.py` constant; 9 fallback sites re-pointed (client.py:231,
   assistant_models.py:99, assistant_service.py:180, content_generator.py:133/779,
   streaming_writer.py:201/620, agent_loop_models.py:226, subagent_manager.py:1010,
   api/routes/chat.py param default, startup_fingerprint.py).
2. Gateway `src/config/settings.py` gains `default_model` (validation_alias DEFAULT_MODEL);
   `src/api/schemas/assistant.py:136` default → `None`.
3. SDKs: python 5 sites, dart 2, java 1 → null/omitted; CLI fetches server default at startup,
   falls back to omitting; docs/examples updated (sdk/python/README.md, sdk/cli/README.md,
   Java Javadoc, Dart doc, cli.tsx help/agents.ts doc).
4. web 5 fallbacks (types/agents.ts:543, assistant/index.tsx:495, AgentCreatePage.tsx:127,
   AgentStudioPage.tsx:498, DatasetDetail.tsx:162) → server default, empty fallback.
5. `.env.example` gains `DEFAULT_MODEL=qwen3.7-plus` + comment; validate-env.sh optional format
   check (never required); test_validate_env_quickstart.py round-trip case.
6. Untouched: eval judge defaults, docgen DEFAULT_MODEL, pricing catalog.

## Gate

```bash
uv run --all-packages --extra test pytest -q --no-cov tests/scripts tests/api tests/services/assistant
uv run --all-packages --extra dev ruff check src/ apps/ packages/ sdk/python
pnpm -C web type-check && pnpm -C web lint && pnpm -C web build
make validate-example-config
```

## Evidence (fill on verify)

- [ ] `rg -n 'qwen3.7-plus' sdk/` → only docs/examples where intended (no code defaults)
- [ ] pytest + ruff + pnpm + validate-example-config outputs
