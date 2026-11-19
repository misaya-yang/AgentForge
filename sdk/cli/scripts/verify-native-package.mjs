import { RELEASE_TARGETS, verifyReleaseSignature } from "../src/native/release_signature.mjs";
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
  for (const path of [distSourcePath]) {
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
    if (!RELEASE_TARGETS.includes(target)) fail(`Unsupported native package target: ${target}`);
    const [platform,arch]=target.split("-");
    const binaryName=platform==="win32"?"codex.exe":"codex";
    const directory = resolve(vendorRoot, target);
    const targetSource=readJson(existsSync(resolve(directory,"source.json"))?resolve(directory,"source.json"):resolve(vendorRoot,"source.json"));
    if(targetSource.schema_version!==SOURCE_SCHEMA || targetSource.upstream_sha!==upstreamSha || targetSource.overlay_sha256!==overlaySha)fail("Native target source identity mismatch");
    const binaryPath = resolve(directory, binaryName);
    const receiptPath = resolve(directory, "artifact.json");
    if (!existsSync(binaryPath) || !existsSync(receiptPath)) fail(`Missing native binary or receipt for ${target}`);
    if (!lstatSync(directory).isDirectory() || !lstatSync(receiptPath).isFile()) fail(`Native package target or receipt is not a regular entry for ${target}`);
    const info = lstatSync(binaryPath);
    if (!info.isFile() || info.isSymbolicLink() || (platform!=="win32" && !(info.mode & 0o111))) fail(`Native binary is not an executable regular file for ${target}`);
    const binary = readFileSync(binaryPath);
    verifyBinaryHeader(binary,platform,arch);
    const receipt = readJson(receiptPath);
    const expected = {schema_version: SCHEMA, upstream_sha: upstreamSha, overlay_sha256: overlaySha, target_system: platform==="win32"?"windows":platform, target_arch: arch, binary: binaryName};
    for (const [key, value] of Object.entries(expected)) {
      if (receipt[key] !== value) fail(`Native receipt ${key} mismatch for ${target}`);
    }
    if (receipt.sha256 !== digest(binary)) fail(`Native binary SHA-256 mismatch for ${target}`);
    const expectedRelease=readJson(distSourcePath);
    if(expectedRelease.release_version && receipt.release_version!==expectedRelease.release_version)fail("Native release version mismatch");
    const pins=expectedRelease.release_keys;
    verifyReleaseSignature(receipt,pins);
    const helperName=platform==="win32"?"platformctl.exe":"platformctl";
    const helperPath=resolve(directory,helperName);
    const helperReceiptPath=resolve(directory,"platformctl-artifact.json");
    if(!existsSync(helperPath)||!existsSync(helperReceiptPath))fail(`Missing signed credential helper for ${target}`);
    const helperInfo=lstatSync(helperPath);
    if(!helperInfo.isFile()||helperInfo.isSymbolicLink()||(platform!=="win32"&&!(helperInfo.mode&0o111)))fail(`Native credential helper is not executable regular file for ${target}`);
    const helper=readFileSync(helperPath);verifyBinaryHeader(helper,platform,arch);
    const helperReceipt=readJson(helperReceiptPath);
    for(const [key,value] of Object.entries({...expected,binary:helperName}))if(helperReceipt[key]!==value)fail(`Credential helper receipt ${key} mismatch`);
    if(helperReceipt.sha256!==digest(helper))fail("Credential helper SHA-256 mismatch");
    if(expectedRelease.release_version && helperReceipt.release_version!==expectedRelease.release_version)fail("Credential helper release version mismatch");
    verifyReleaseSignature(helperReceipt,pins);
    for (const [name, expectedHash] of Object.entries(materials)) {
      const path = resolve(directory, name);
      if (!/^[0-9a-f]{64}$/.test(expectedHash || "") || receipt.materials?.[name] !== expectedHash || helperReceipt.materials?.[name] !== expectedHash || !existsSync(path) || !lstatSync(path).isFile() || digest(readFileSync(path)) !== expectedHash) {
        fail(`Missing or mismatched native package material ${name} for ${target}`);
      }
    }
  }
  return targets;
}

export function verifyBinaryHeader(binary,platform,arch) {
  const valid=platform==="linux" ? binary.length>=64 && binary.subarray(0,4).toString("hex")==="7f454c46" && binary[4]===2 && binary[5]===1 && binary.readUInt16LE(18)===(arch==="x64"?62:183)
    : platform==="darwin" ? binary.length>=32 && binary.readUInt32LE(0)===0xfeedfacf && binary.readUInt32LE(4)===(arch==="x64"?0x01000007:0x0100000c)
    : platform==="win32" && arch==="x64" && binary.length>=256 && binary.subarray(0,2).toString()==="MZ" && (()=>{const offset=binary.readUInt32LE(0x3c);return offset+6<=binary.length && binary.subarray(offset,offset+4).toString("hex")==="50450000" && binary.readUInt16LE(offset+4)===0x8664;})();
  if (!valid) fail("Native binary ELF architecture / Mach-O / PE target mismatch");
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
