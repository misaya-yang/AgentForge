import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { afterEach, describe, expect, it } from "vitest";

const cliRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
let vendorRoot = "";

afterEach(() => {
  if (vendorRoot) rmSync(vendorRoot, { recursive: true, force: true });
  vendorRoot = "";
});

describe("native vendor release staging", () => {
  it("removes stale target directories while preserving vendor metadata files", () => {
    vendorRoot = mkdtempSync(join(tmpdir(), "ai-gateway-native-vendor-test-"));
    mkdirSync(join(vendorRoot, "linux-arm64"));
    mkdirSync(join(vendorRoot, "darwin-x64"));
    writeFileSync(join(vendorRoot, "README.md"), "metadata");
    writeFileSync(join(vendorRoot, "source.json"), "{}");

    const result = runPrepare("linux-x64");

    expect(result.status).toBe(0);
    expect(existsSync(join(vendorRoot, "linux-arm64"))).toBe(false);
    expect(existsSync(join(vendorRoot, "darwin-x64"))).toBe(false);
    expect(existsSync(join(vendorRoot, "README.md"))).toBe(true);
    expect(existsSync(join(vendorRoot, "source.json"))).toBe(true);
  });

  it("rejects an invalid target before removing anything", () => {
    vendorRoot = mkdtempSync(join(tmpdir(), "ai-gateway-native-vendor-test-"));
    mkdirSync(join(vendorRoot, "linux-arm64"));

    const result = runPrepare("not-a-target");

    expect(result.status).toBe(2);
    expect(existsSync(join(vendorRoot, "linux-arm64"))).toBe(true);
  });
});

function runPrepare(target: string) {
  return spawnSync(process.execPath, [join(cliRoot, "scripts", "prepare-native-vendor.mjs"), target], {
    cwd: cliRoot,
    encoding: "utf8",
    env: {
      ...process.env,
      AI_GATEWAY_CLI_VENDOR_ROOT: vendorRoot,
    },
  });
}
