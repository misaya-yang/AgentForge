import { readdirSync, rmSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const target = process.argv[2];
if (!/^(linux|darwin|win32)-(x64|arm64)$/.test(target ?? "")) {
  fail("Usage: prepare-native-vendor.mjs <linux|darwin|win32>-<x64|arm64>");
}

const cliRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const vendorRoot = resolve(process.env.AI_GATEWAY_CLI_VENDOR_ROOT || resolve(cliRoot, "vendor"));

for (const entry of readdirSync(vendorRoot, { withFileTypes: true })) {
  if (entry.isDirectory() && /^(linux|darwin|win32)-(x64|arm64)$/.test(entry.name)) {
    rmSync(resolve(vendorRoot, entry.name), { recursive: true, force: true });
  }
}

console.log(`Prepared native vendor staging for ${target}`);

function fail(message) {
  console.error(`ERROR: ${message}`);
  process.exit(2);
}
