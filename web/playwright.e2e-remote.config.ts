/**
 * Playwright config for running E2E tests against a running remote server.
 * Usage: E2E_BASE_URL=https://... E2E_API_URL=https://... npx playwright test --config=playwright.e2e-remote.config.ts
 */
import path from "path";
import { fileURLToPath } from "url";
import { defineConfig, devices } from "@playwright/test";

const baseURL = process.env.E2E_BASE_URL!;
const configDir = path.dirname(fileURLToPath(import.meta.url));
const authStatePath = path.resolve(configDir, ".playwright/auth-state.json");
const globalSetupPath = path.resolve(configDir, "e2e/global.setup.ts");

export default defineConfig({
  testDir: "./e2e",
  timeout: 120_000,
  globalSetup: globalSetupPath,
  expect: { timeout: 15_000 },
  fullyParallel: true,
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL,
    storageState: authStatePath,
    ignoreHTTPSErrors: true,
    trace: "retain-on-first-failure",
    screenshot: "only-on-failure",
  },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
  ],
  // No webServer — assumes backend & frontend are already running
});
