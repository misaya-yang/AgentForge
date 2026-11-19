import path from "path";
import { fileURLToPath } from "url";
import { defineConfig, devices } from "@playwright/test";

function requireEnv(name: string): string {
  const value = process.env[name];
  if (!value) {
    throw new Error(`Missing required E2E env: ${name}`);
  }
  return value;
}

const baseURL = requireEnv("E2E_BASE_URL");
const configDir = path.dirname(fileURLToPath(import.meta.url));
const authStatePath = path.resolve(configDir, ".playwright/auth-state.json");
const globalSetupPath = path.resolve(configDir, "e2e/global.setup.ts");

export default defineConfig({
  testDir: "./e2e",
  testIgnore: ["platform-convergence-live.spec.ts"],
  timeout: 120_000,
  globalSetup: globalSetupPath,
  expect: {
    timeout: 15_000,
  },
  fullyParallel: true,
  retries: process.env.CI ? 1 : 0,
  reporter: [["list"], ["html", { open: "never" }]],
  use: {
    baseURL,
    storageState: authStatePath,
    trace: "retain-on-first-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
});
