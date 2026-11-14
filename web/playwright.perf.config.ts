import { defineConfig } from "@playwright/test";
import config from "./playwright.opensource.config";

// Measure already-running production assets. Never start Vite or any service.
if (!process.env.E2E_BASE_URL) throw new Error("Set E2E_BASE_URL to an existing production frontend");
export default defineConfig(config, { webServer: undefined, workers: 1, retries: 0, reporter: [["list"]] });
