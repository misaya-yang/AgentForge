import { createPublicKey, verify } from "node:crypto";
export const RELEASE_TARGETS = ["linux-x64", "linux-arm64", "darwin-x64", "darwin-arm64", "win32-x64"];
export function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") return `{${Object.keys(value).sort().map(k => `${JSON.stringify(k)}:${canonical(value[k])}`).join(",")}}`;
  if (typeof value === "number" && !Number.isFinite(value)) throw new Error("Invalid signed release payload");
  return JSON.stringify(value);
}
export function verifyReleaseSignature(receipt, keys) {
  const signature = receipt?.signature;
  if (!signature || signature.algorithm !== "Ed25519" || !/^[A-Za-z0-9_.-]{1,64}$/.test(signature.key_id ?? "")
      || typeof keys?.[signature.key_id] !== "string" || typeof signature.value !== "string") throw new Error("Native release signature or pinned key missing");
  const key = createPublicKey(keys[signature.key_id]);
  const bytes = Buffer.from(signature.value, "base64url");
  const { signature: _, ...payload } = receipt;
  if (key.asymmetricKeyType !== "ed25519" || bytes.length !== 64 || bytes.toString("base64url") !== signature.value
      || !verify(null, Buffer.from(canonical(payload)), key, bytes)) throw new Error("Native release signature verification failed");
}
