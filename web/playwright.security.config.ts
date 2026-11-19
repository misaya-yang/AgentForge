import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e", testMatch: "console-browser-security.spec.ts",
  outputDir: process.env.SECURITY_TEST_RESULTS || "/tmp/agentforge-browser-security-results",
  workers: 1, timeout: 60_000, reporter: "list",
  use: { baseURL: "http://127.0.0.1:4182", trace: "retain-on-failure" },
  projects: [{ name: "chromium", use: devices["Desktop Chrome"] }],
  webServer: { command: "node scripts/browser-security-server.mjs", url: "http://127.0.0.1:4182", timeout: 120_000, reuseExistingServer: false },
});
