import { createHash } from "node:crypto";
import { existsSync, lstatSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SCHEMA = "ai-gateway-cli/native-artifact/v1";
const SOURCE_SCHEMA = "ai-gateway-cli/native-source/v1";
const defaultCliRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

export function verifyNativePackage({
  repositoryRoot = resolve(defaultCliRoot, "../.."),
  vendorRoot = resolve(process.env.AI_GATEWAY_CLI_VENDOR_ROOT || resolve(defaultCliRoot, "vendor")),
  distSourcePath = resolve(process.env.AI_GATEWAY_CLI_DIST_SOURCE || resolve(defaultCliRoot, "dist/native-source.json")),
  targets = (process.env.AI_GATEWAY_CLI_PACKAGE_TARGETS || `${process.platform}-${process.arch}`).split(",").map(value => value.trim()).filter(Boolean),
} = {}) {
  const sourceReceipt = readJson(resolve(repositoryRoot, "deploy/agent-runtime-source/source-receipt.json"));
  const overlayManifest = readJson(resolve(repositoryRoot, "rust/agent-runtime-overlay/manifest.json"));
  const sourceLock = readJson(resolve(repositoryRoot, "deploy/agent-runtime-source/lock.json"));
  const upstreamSha = sourceReceipt.source?.upstream_sha;
  const overlaySha = overlayManifest.sha256;
  if (!/^[0-9a-f]{40}$/.test(upstreamSha || "") || !/^[0-9a-f]{64}$/.test(overlaySha || "")) {
    fail("Invalid native source identity");
  }
  if (sourceReceipt.overlay?.sha256 !== overlaySha || sourceLock.build?.overlay_sha256 !== overlaySha) {
    fail("Agent Runtime source receipt, overlay manifest, and lock disagree");
  }
  if (overlayManifest.upstream_sha !== upstreamSha || sourceLock.source?.upstream_sha !== upstreamSha) {
    fail("Agent Runtime upstream identity disagrees across source records");
  }
  for (const path of [resolve(vendorRoot, "source.json"), distSourcePath]) {
    const source = readJson(path);
    if (source.schema_version !== SOURCE_SCHEMA || source.upstream_sha !== upstreamSha || source.overlay_sha256 !== overlaySha) {
      fail("Packaged native source identity does not match the repository source records");
    }
  }
  const materials = {
    LICENSE: sourceLock.license?.upstream_license_sha256,
    NOTICE: sourceLock.license?.upstream_notice_sha256,
    "NOTICE.ai-platform.md": sourceLock.license?.notice_sha256,
    "sbom.cdx.json": sourceLock.build?.sbom_sha256,
    "source-receipt.json": sourceLock.build?.source_receipt_sha256,
  };
  if (!targets.length) fail("AI_GATEWAY_CLI_PACKAGE_TARGETS selected no targets");
  for (const target of targets) {
    if (!/^linux-(x64|arm64)$/.test(target)) fail(`Unsupported native package target: ${target}`);
    const arch = target.slice("linux-".length);
    const directory = resolve(vendorRoot, target);
    const binaryPath = resolve(directory, "codex");
    const receiptPath = resolve(directory, "artifact.json");
    if (!existsSync(binaryPath) || !existsSync(receiptPath)) fail(`Missing native binary or receipt for ${target}`);
    if (!lstatSync(directory).isDirectory() || !lstatSync(receiptPath).isFile()) fail(`Native package target or receipt is not a regular entry for ${target}`);
    const info = lstatSync(binaryPath);
    if (!info.isFile() || !(info.mode & 0o111)) fail(`Native binary is not an executable regular file for ${target}`);
    const binary = readFileSync(binaryPath);
    if (binary.length < 64 || binary.subarray(0, 4).toString("hex") !== "7f454c46" || binary[4] !== 2 || binary[5] !== 1 || binary.readUInt16LE(18) !== (arch === "x64" ? 62 : 183)) {
      fail(`Native binary ELF architecture mismatch for ${target}`);
    }
    const receipt = readJson(receiptPath);
    const expected = {schema_version: SCHEMA, upstream_sha: upstreamSha, overlay_sha256: overlaySha, target_system: "linux", target_arch: arch, binary: "codex"};
    for (const [key, value] of Object.entries(expected)) {
      if (receipt[key] !== value) fail(`Native receipt ${key} mismatch for ${target}`);
    }
    if (receipt.sha256 !== digest(binary)) fail(`Native binary SHA-256 mismatch for ${target}`);
    for (const [name, expectedHash] of Object.entries(materials)) {
      const path = resolve(directory, name);
      if (!/^[0-9a-f]{64}$/.test(expectedHash || "") || receipt.materials?.[name] !== expectedHash || !existsSync(path) || !lstatSync(path).isFile() || digest(readFileSync(path)) !== expectedHash) {
        fail(`Missing or mismatched native package material ${name} for ${target}`);
      }
    }
  }
  return targets;
}

function digest(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function readJson(path) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch {
    fail(`Cannot read native package identity: ${path}`);
  }
}

function fail(message) {
  throw new Error(message);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const targets = verifyNativePackage();
    console.log(`Verified ${targets.length} native CLI package target(s): ${targets.join(", ")}`);
  } catch (error) {
    console.error(`ERROR: ${error instanceof Error ? error.message : String(error)}`);
    process.exit(1);
  }
}
