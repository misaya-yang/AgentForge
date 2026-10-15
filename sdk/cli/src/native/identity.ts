import { createHash } from "node:crypto";
import { lstatSync, readFileSync } from "node:fs";

export const NATIVE_ARTIFACT_SCHEMA = "ai-gateway-cli/native-artifact/v1";
export const NATIVE_SOURCE_SCHEMA = "ai-gateway-cli/native-source/v1";

export interface NativeSourceIdentity {
  schema_version: string;
  upstream_sha: string;
  overlay_sha256: string;
}

export interface NativeArtifactReceipt {
  schema_version: string;
  upstream_sha: string;
  overlay_sha256: string;
  target_system: string;
  target_arch: string;
  binary: string;
  sha256: string;
}

export function verifyNativeArtifact(
  binaryPath: string,
  receiptPath: string,
  platform: NodeJS.Platform,
  arch: string,
  expectedSource?: NativeSourceIdentity,
): void {
  const stat = lstatSync(binaryPath);
  if (!stat.isFile() || stat.isSymbolicLink()) {
    throw new Error(`Native Agent Runtime must be a regular non-symlink file: ${binaryPath}`);
  }
  let receipt: NativeArtifactReceipt;
  try {
    receipt = JSON.parse(readFileSync(receiptPath, "utf8")) as NativeArtifactReceipt;
  } catch (error) {
    throw new Error(`Cannot read native Agent Runtime receipt ${receiptPath}: ${message(error)}`);
  }
  const expectedSystem = platform === "win32" ? "windows" : platform;
  if (receipt.schema_version !== NATIVE_ARTIFACT_SCHEMA) throw new Error("Native Agent Runtime receipt schema mismatch");
  if (!/^[0-9a-f]{40}$/.test(receipt.upstream_sha)) throw new Error("Native Agent Runtime upstream SHA is invalid");
  if (!/^[0-9a-f]{64}$/.test(receipt.overlay_sha256)) throw new Error("Native Agent Runtime overlay SHA is invalid");
  if (expectedSource) {
    validateNativeSourceIdentity(expectedSource);
    if (receipt.upstream_sha !== expectedSource.upstream_sha) throw new Error("Native Agent Runtime upstream SHA mismatch");
    if (receipt.overlay_sha256 !== expectedSource.overlay_sha256) throw new Error("Native Agent Runtime overlay SHA mismatch");
  }
  if (receipt.target_system !== expectedSystem || receipt.target_arch !== arch) {
    throw new Error(`Native Agent Runtime target mismatch: expected ${expectedSystem}-${arch}`);
  }
  const expectedName = platform === "win32" ? "codex.exe" : "codex";
  if (receipt.binary !== expectedName) throw new Error("Native Agent Runtime binary name mismatch");
  const digest = createHash("sha256").update(readFileSync(binaryPath)).digest("hex");
  if (!/^[0-9a-f]{64}$/.test(receipt.sha256) || digest !== receipt.sha256) {
    throw new Error("Native Agent Runtime binary SHA-256 mismatch");
  }
}

export function readNativeSourceIdentity(path: string): NativeSourceIdentity {
  try {
    const value = JSON.parse(readFileSync(path, "utf8")) as NativeSourceIdentity;
    validateNativeSourceIdentity(value);
    return value;
  } catch (error) {
    throw new Error(`Cannot read native source identity ${path}: ${message(error)}`);
  }
}

export function assertNativeSourceMatch(expected: NativeSourceIdentity, actual: NativeSourceIdentity): void {
  validateNativeSourceIdentity(expected);
  validateNativeSourceIdentity(actual);
  if (actual.upstream_sha !== expected.upstream_sha) throw new Error("Packaged native source upstream SHA mismatch");
  if (actual.overlay_sha256 !== expected.overlay_sha256) throw new Error("Packaged native source overlay SHA mismatch");
}

function validateNativeSourceIdentity(value: NativeSourceIdentity): void {
  if (value.schema_version !== NATIVE_SOURCE_SCHEMA) throw new Error("Native source identity schema mismatch");
  if (!/^[0-9a-f]{40}$/.test(value.upstream_sha)) throw new Error("Native source upstream SHA is invalid");
  if (!/^[0-9a-f]{64}$/.test(value.overlay_sha256)) throw new Error("Native source overlay SHA is invalid");
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
