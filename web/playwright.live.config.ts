import baseConfig from "./playwright.config";

process.env.E2E_DOCKER_LIVE_STACK = "1";

/**
 * Regression config for running Playwright against the ALREADY-RUNNING Docker
 * stack (frontend :8081, gateway :8080, knowledge :8092).
 *
 * It reuses the base project/auth setup but drops the `webServer` block so
 * Playwright does NOT try to spawn the dev stack (which would clash with the
 * live containers, e.g. postgres already bound to :5432). The live stack is
 * expected to be healthy before invoking:
 *
 *   E2E_EXISTING_ACCOUNT_ONLY=1 E2E_BASE_URL=https://console.example.test \
 *     E2E_API_URL=http://localhost:8080 pnpm exec playwright test -c playwright.live.config.ts --workers=1
 * Console setup is existing-account-only and requires trusted HTTPS plus active
 * BFF origins/signing configuration. API initialization is a separate fixture.
 */
export default {
  ...baseConfig,
  testIgnore: [],
  webServer: undefined,
};
