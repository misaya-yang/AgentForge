import { createHash } from "node:crypto";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { afterEach, describe, expect, it } from "vitest";
import { verifyNativePackage } from "./verify-native-package.mjs";

let fixtureRoot = "";
afterEach(() => {
  if (fixtureRoot) rmSync(fixtureRoot, { recursive: true, force: true });
  fixtureRoot = "";
});
const hash = (bytes: Buffer | string) => createHash("sha256").update(bytes).digest("hex");

function fixture() {
  fixtureRoot = mkdtempSync(join(tmpdir(), "native-package-contract-"));
  const vendorRoot = join(fixtureRoot, "vendor");
  const target = "linux-arm64";
  const directory = join(vendorRoot, target);
  const deploy = join(fixtureRoot, "deploy/agent-runtime-source");
  const overlay = join(fixtureRoot, "rust/agent-runtime-overlay");
  for (const path of [directory, deploy, overlay]) mkdirSync(path, { recursive: true });
  const source = { schema_version: "ai-gateway-cli/native-source/v1", upstream_sha: "a".repeat(40), overlay_sha256: "b".repeat(64) };
  const receipt = JSON.stringify({ source: { upstream_sha: source.upstream_sha }, overlay: { sha256: source.overlay_sha256 } });
  writeFileSync(join(deploy, "source-receipt.json"), receipt);
  writeFileSync(join(overlay, "manifest.json"), JSON.stringify({ upstream_sha: source.upstream_sha, sha256: source.overlay_sha256 }));
  const materialBytes: Record<string, string> = { LICENSE: "license", NOTICE: "notice", "NOTICE.ai-platform.md": "platform notice", "sbom.cdx.json": '{"components":[]}', "source-receipt.json": receipt };
  const materials = Object.fromEntries(Object.entries(materialBytes).map(([name, content]) => {
    writeFileSync(join(directory, name), content);
    return [name, hash(content)];
  }));
  writeFileSync(join(deploy, "lock.json"), JSON.stringify({
    source: { upstream_sha: source.upstream_sha },
    build: { overlay_sha256: source.overlay_sha256, sbom_sha256: materials["sbom.cdx.json"], source_receipt_sha256: materials["source-receipt.json"] },
    license: { upstream_license_sha256: materials.LICENSE, upstream_notice_sha256: materials.NOTICE, notice_sha256: materials["NOTICE.ai-platform.md"] },
  }));
  writeFileSync(join(vendorRoot, "source.json"), JSON.stringify(source));
  const distSourcePath = join(fixtureRoot, "dist-source.json");
  writeFileSync(distSourcePath, JSON.stringify(source));
  const binary = Buffer.alloc(64);
  binary.set([0x7f, 0x45, 0x4c, 0x46, 2, 1]);
  binary.writeUInt16LE(183, 18);
  writeFileSync(join(directory, "codex"), binary, { mode: 0o755 });
  writeFileSync(join(directory, "artifact.json"), JSON.stringify({
    schema_version: "ai-gateway-cli/native-artifact/v1", upstream_sha: source.upstream_sha, overlay_sha256: source.overlay_sha256,
    target_system: "linux", target_arch: "arm64", binary: "codex", sha256: hash(binary), materials,
  }));
  return { repositoryRoot: fixtureRoot, vendorRoot, distSourcePath, targets: [target], directory };
}

describe("native package verifier", () => {
  it("accepts matched ELF identity and all locked package materials", () => {
    const paths = fixture();
    expect(verifyNativePackage(paths)).toEqual(["linux-arm64"]);
  });
  it.each(["codex", "artifact.json", "LICENSE", "NOTICE", "NOTICE.ai-platform.md", "sbom.cdx.json", "source-receipt.json"])("rejects missing %s", (name) => {
    const paths = fixture();
    rmSync(join(paths.directory, name));
    expect(() => verifyNativePackage(paths)).toThrow(/Missing/);
  });
  it("rejects binary tampering even if its ELF header remains valid", () => {
    const paths = fixture();
    const binary = readFileSync(join(paths.directory, "codex"));
    binary[63] = 1;
    writeFileSync(join(paths.directory, "codex"), binary);
    expect(() => verifyNativePackage(paths)).toThrow(/SHA-256 mismatch/);
  });
  it.each(["text-file", "wrong-arch", "not-executable"])("rejects unusable binary %s", (failure) => {
    const paths = fixture();
    if (failure === "not-executable") chmodSync(join(paths.directory, "codex"), 0o644);
    else if (failure === "text-file") writeFileSync(join(paths.directory, "codex"), "synthetic-native-cli");
    else {
      const binary = readFileSync(join(paths.directory, "codex"));
      binary.writeUInt16LE(62, 18);
      writeFileSync(join(paths.directory, "codex"), binary);
    }
    expect(() => verifyNativePackage(paths)).toThrow(/ELF architecture|executable regular/);
  });
  it("rejects a stale overlay and a tampered notice", () => {
    const paths = fixture();
    writeFileSync(join(paths.directory, "NOTICE"), "tampered");
    expect(() => verifyNativePackage(paths)).toThrow(/material NOTICE/);
    const source = JSON.parse(readFileSync(paths.distSourcePath, "utf8"));
    source.overlay_sha256 = "c".repeat(64);
    writeFileSync(paths.distSourcePath, JSON.stringify(source));
    expect(() => verifyNativePackage(paths)).toThrow(/source identity/);
  });
  it("does not advertise a missing second architecture or unsupported platform", () => {
    const paths = fixture();
    expect(() => verifyNativePackage({ ...paths, targets: ["linux-arm64", "linux-x64"] })).toThrow(/Missing native binary/);
    expect(() => verifyNativePackage({ ...paths, targets: ["darwin-arm64"] })).toThrow(/Unsupported/);
  });
});
