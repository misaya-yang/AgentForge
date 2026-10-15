# PC-04 — Final verification (comprehensive)

The user asked for a complete final check (全面检查最终结果).

## Steps

1. Docs final review: CHANGELOG sections, `.env.example` final diff, README vs first screen
   consistency, `open-source-env-readiness-todo.md`, docs/README.md index.
2. Local CI-equivalent full run:
   `make harness-check validate-example-config test-isolation migrate-status` (live-DB items
   reported explicitly); `pnpm -C web type-check && lint && build && i18n:check`;
   `pnpm -C web e2e:opensource`; full affected-area pytest; `make verify-agent-studio` if the
   live stack allows, otherwise explicit not-run statement with the manual command.
3. rg terminal checks:
   - `qwen3.7-plus` absent from SDK/assistant/gateway/schema defaults (docs/examples allowed)
   - confluence fossil zero references (`rg -i confluence src/ web/src/` → connectors stack only)
   - `/exams` zero references; `ai_gateway_core.quiz` zero reverse references from src/
   - product surface: nav = 9 items in 3 groups; first-run path login → banner → /services
4. loop-state.json all phases verified, blockers empty; report to
   `reports/code-review/product-convergence-2026-08.md`; progress-log closed.
5. No commit/push/PR (user has not asked); list suggested PR split.

## Evidence (fill on verify)

- [ ] gate outputs per step
- [ ] rg terminal checks output
- [ ] report file referenced from docs/README.md
