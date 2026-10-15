# End-to-End Tests

**Every Playwright spec in this repository lives in this directory. There is no other correct
place for one.** `make harness-check` fails the build when a `*.spec.ts` appears outside
`web/e2e/`, `web/src/`, or `sdk/`, and when a screenshot or `.har` is left at the repository root.

## Layout

| Path | Holds | Committed |
| --- | --- | --- |
| `web/e2e/*.spec.ts` | Test specs, one file per user-visible flow | yes |
| `web/e2e/support/` | Shared helpers and page objects (`helpers.ts`) | yes |
| `web/e2e/fixtures/` | Static test data: seed payloads, sample uploads, golden JSON | yes |
| `web/e2e/global.setup.ts` | Auth bootstrap that runs before the projects | yes |
| `web/.playwright/` | Auth state, stack metadata, run screenshots | no |
| `web/test-results/` | Per-test output, traces, videos | no |
| `web/playwright-report/` | HTML report | no |
| `tmp/browser/` | Ad-hoc agent browsing artifacts — screenshots, console dumps | no |

The three artifact directories are ignored by [`web/.gitignore`](../.gitignore); `tmp/` is ignored
at the repository root. Never commit a run artifact, and never write one to the repository root.

## Configs

All five configs live in `web/` and share `playwright.config.ts` as their base.

| Config | Use | Stack |
| --- | --- | --- |
| `playwright.config.ts` | Default local run | Uses the repository-owned Docker stack |
| `playwright.opensource.config.ts` | Route smoke run in CI — no provider key needed | Static build |
| `playwright.live.config.ts` | Regression against an already-running Docker stack | Reuses `:8081`/`:8080`/`:8092`; no `webServer` |
| `playwright.e2e-remote.config.ts` | Against a remote deployment | External |
| `playwright.model-tester.config.ts` | Model-tester role scenarios | Existing stack |

Adding a sixth config is almost always the wrong move — prefer a new project inside an existing
config, or an env-var switch.

## Running

```bash
pnpm -C web e2e:opensource      # CI smoke: routes, eval trace, agent studio, publish
```

```bash
pnpm -C web e2e                 # full stack E2E against make quickstart/deploy
```

```bash
pnpm -C web e2e:headed          # same, with a visible browser
```

Against a stack you already started with `make quickstart`:

Local E2E defaults to the existing dedicated account. Missing credentials,
failed login, or a forced password change stop setup; it never provisions or
resets accounts as a fallback. `E2E_EXISTING_ACCOUNT_ONLY=1` is an optional
additional guard. Only a fresh isolated environment with no persisted account
may initialize accounts by explicitly setting `E2E_PROVISION_ACCOUNTS=1`;
setup checks that model-tester accounts do not already exist before writing.

```bash
cd web && E2E_EXISTING_ACCOUNT_ONLY=1 \
  E2E_BASE_URL=http://localhost:8081 E2E_API_URL=http://localhost:8080 \
  pnpm exec playwright test -c playwright.live.config.ts --workers=1
```

The platform-convergence release flow is separately opt-in because it spends
provider quota and mutates only the authenticated E2E user's session state:

```bash
cd web && E2E_EXISTING_ACCOUNT_ONLY=1 RUN_PLATFORM_CONVERGENCE_LIVE=1 \
  E2E_BASE_URL=http://localhost:8081 E2E_API_URL=http://localhost:8080 \
  pnpm exec playwright test -c playwright.live.config.ts \
    e2e/platform-convergence-live.spec.ts --workers=1
```

Without `RUN_PLATFORM_CONVERGENCE_LIVE=1` the spec is intentionally skipped.
A listed or skipped test is not release evidence and must not be recorded as PASS.

## Writing a spec

1. One spec file per user-visible flow; name it after the flow (`agent-publish.spec.ts`), not
   after the component.
2. Put shared setup in `support/helpers.ts`. Do not copy a login block between specs.
3. Static data goes in `fixtures/` and is imported — never inlined as a 200-line literal.
4. A spec that needs a live model provider belongs in a `*-live.spec.ts` file and must be excluded
   from `e2e:opensource`, which has to pass without any provider key.
5. Screenshots taken for debugging go to `tmp/browser/`, never to the repo root and never into git.

## For agents driving a browser directly

The Playwright MCP server writes console logs and page dumps to `.playwright-mcp/` in the working
directory by default, which is how the repository root previously accumulated stray files. Point it
at the scratch directory instead:

```bash
--output-dir tmp/browser
```

Ad-hoc verification screenshots are scratch, not evidence. If a screenshot is genuinely evidence for
a review, put it under `reports/<area>/` and reference it from the report — otherwise leave it in
`tmp/`.
