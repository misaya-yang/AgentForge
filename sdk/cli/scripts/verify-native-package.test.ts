import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { afterEach, describe, expect, it } from "vitest";
import { canonical } from "../src/native/release_signature.mjs";
import { verifyNativePackage } from "./verify-native-package.mjs";

let fixtureRoot = "";
afterEach(() => {
  if (fixtureRoot) rmSync(fixtureRoot, { recursive: true, force: true });
  fixtureRoot = "";
});
const hash = (bytes: Buffer | string) => createHash("sha256").update(bytes).digest("hex");

function fixture(target="linux-arm64") {
  fixtureRoot = mkdtempSync(join(tmpdir(), "native-package-contract-"));
  const vendorRoot = join(fixtureRoot, "vendor");
  const [platform,arch]=target.split("-");
  const directory = join(vendorRoot, target);
  const deploy = join(fixtureRoot, "deploy/agent-runtime-source");
  const overlay = join(fixtureRoot, "rust/agent-runtime-overlay");
  for (const path of [directory, deploy, overlay]) mkdirSync(path, { recursive: true });
  const keys=generateKeyPairSync("ed25519");
  const release_keys={fixture:keys.publicKey.export({type:"spki",format:"pem"}).toString()};
  const source = { release_keys, schema_version: "ai-gateway-cli/native-source/v1", upstream_sha: "a".repeat(40), overlay_sha256: "b".repeat(64) };
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
  const binary=Buffer.alloc(platform==="win32"?256:64);
  if(platform==="linux"){binary.set([0x7f,0x45,0x4c,0x46,2,1]);binary.writeUInt16LE(arch==="x64"?62:183,18)}
  else if(platform==="darwin"){binary.writeUInt32LE(0xfeedfacf,0);binary.writeUInt32LE(arch==="x64"?0x01000007:0x0100000c,4)}
  else {binary.write("MZ");binary.writeUInt32LE(128,0x3c);binary.write("PE\0\0",128);binary.writeUInt16LE(0x8664,132)}
  for(const name of [platform==="win32"?"codex.exe":"codex",platform==="win32"?"platformctl.exe":"platformctl"]){
    writeFileSync(join(directory,name),binary,{mode:0o755});
    const receipt={schema_version:"ai-gateway-cli/native-artifact/v1",upstream_sha:source.upstream_sha,overlay_sha256:source.overlay_sha256,target_system:platform==="win32"?"windows":platform,target_arch:arch,binary:name,sha256:hash(binary),materials};
    writeFileSync(join(directory,name.startsWith("codex")?"artifact.json":"platformctl-artifact.json"),JSON.stringify({...receipt,signature:{algorithm:"Ed25519",key_id:"fixture",value:sign(null,Buffer.from(canonical(receipt)),keys.privateKey).toString("base64url")}}));
  }
  return { repositoryRoot: fixtureRoot, vendorRoot, distSourcePath, targets: [target], directory };
}

describe("native package verifier", () => {
  it.each(["linux-x64","linux-arm64","darwin-x64","darwin-arm64","win32-x64"])("verifies signed release fixture %s",target=>{expect(verifyNativePackage(fixture(target))).toEqual([target])});
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
    expect(() => verifyNativePackage({ ...paths, targets: ["win32-arm64"] })).toThrow(/Unsupported/);
  });
});
