# PC-02D — First-run onboarding + nav groups + i18n CI (track D, worktree)

## Contract

1. New `src/api/v1/setup.py`: `GET /api/v1/setup/state` → `{configured, missing[], mode,
   default_model}`. Derivation reuses `src/api/v1/health.py:117-164` provider-config logic;
   `mode` from gateway settings field `model_setup_mode` (env `MODEL_SETUP_MODE`).
   Auth: JWT **and** api-key (CLI consumes it); requires the Services-view capability rather
   than an admin role. Registered in router.py.
2. `web/src/api/setup.ts` `useSetupState` (react-query, staleTime 5min, invalidate on route
   change) → `web/src/components/SetupBanner.tsx` in AppLayout above content: persistent when
   unconfigured, links to `/services`, auto-disappears when configured; dismiss in localStorage
   → `web/src/pages/dashboard/components/SetupChecklist.tsx`: 3-step checklist (provider → KB →
   first chat) replacing dashboard first screen when unconfigured.
3. Nav regroup (AppLayout.tsx:58-70): `NavItem.group` field; dashboard top-level ungrouped;
   使用 = assistant + agents; 构建 = knowledge + playground(label → 模型调试) + eval;
   治理 = services + users + tasks + settings; empty groups collapse; model_tester collapse
   preserved; group labels + assistant/playground/agents empty-state copy in all 6 i18n bundles
   (check-i18n-keys.mjs parity).
4. ci.yml frontend job gains `pnpm -C web i18n:check` step.
5. e2e: new `web/e2e/first-run.spec.ts` (banner + checklist) and nav-group assertions, registered
   in `e2e:opensource`.
6. README.md:44-46 rewritten (no "web setup" claim; describe banner flow);
   `deploy/runbooks/open-source-env-readiness-todo.md` updated.

## Gate

```bash
uv run --all-packages --extra test pytest -q --no-cov tests/api/test_setup_state.py tests/api
pnpm -C web type-check && pnpm -C web lint && pnpm -C web build && pnpm -C web i18n:check
pnpm -C web e2e:opensource
make harness-check
```

## Evidence (fill on verify)

- [ ] pytest + pnpm chain (incl. i18n:check) + e2e:opensource + harness-check outputs
