/**
 * Standalone Playwright config for model_tester role tests.
 * No global setup — each test performs its own login.
 */
import { defineConfig, devices } from "@playwright/test";
import { requireHttpsConsoleOrigin } from "./e2e/support/bff-session.ts";

const baseURL = requireHttpsConsoleOrigin(process.env.E2E_BASE_URL);

export default defineConfig({
  testDir: "./e2e",
  testMatch: "model-tester-role.spec.ts",
  timeout: 60_000,
  expect: { timeout: 15_000 },
  fullyParallel: false,
  retries: 1,
  reporter: [["list"]],
  use: {
    baseURL,
    ignoreHTTPSErrors: false,
    trace: "retain-on-first-failure",
    screenshot: "only-on-failure",
  },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
  ],
});
