import { defineConfig } from "@playwright/test";
import config from "./playwright.opensource.config";

// Measure already-running production assets. Never start Vite or any service.
if (!process.env.E2E_BASE_URL) throw new Error("Set E2E_BASE_URL to an existing production frontend");
// defineConfig merges and keeps an inherited webServer when the override is
// undefined, so drop it from the base explicitly.
const { webServer: _webServer, ...base } = config;
export default defineConfig(base, { workers: 1, retries: 0, reporter: [["list"]] });
