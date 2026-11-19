import assert from "node:assert/strict";
import test from "node:test";
import { assertionOptions, decodeBase64url, encodeBase64url } from "./passkey.ts";

test("WebAuthn bytes round trip without UTF-8 conversion or padding", () => {
  const bytes = new Uint8Array([0, 255, 128, 64, 1]);
  const wire = encodeBase64url(bytes.buffer);
  assert.equal(wire, "AP-AQAE");
  assert.deepEqual(new Uint8Array(decodeBase64url(wire)), bytes);
  for (const invalid of ["", "a=", "a+b", "a/b", " a"]) assert.throws(() => decodeBase64url(invalid));
});
test("assertion decoding preserves RP and requires user verification", () => {
  const challenge = { flow_id: "flow", publicKey: { challenge: "YWJj", userVerification: "required" as const,
    rpId: "console.example", allowCredentials: [{ type: "public-key" as const, id: "AP8", transports: ["internal" as const] }] } };
  const options = assertionOptions(challenge);
  assert.equal(options.userVerification, "required"); assert.equal(options.rpId, "console.example");
  assert.deepEqual(new Uint8Array(options.challenge as ArrayBuffer), new Uint8Array([97, 98, 99]));
  assert.deepEqual(new Uint8Array(options.allowCredentials![0].id as ArrayBuffer), new Uint8Array([0, 255]));
  for (const userVerification of [undefined, "preferred", "discouraged"] as const) {
    assert.throws(() => assertionOptions({ ...challenge, publicKey: { ...challenge.publicKey, userVerification } }), /verification is required/);
  }
});

test("registration decodes challenge, user handle and excluded IDs without weakening UV", async () => {
  const { registrationOptions } = await import("./passkey.ts");
  const options = registrationOptions({ flow_id: "flow", publicKey: { challenge: "YWJj", rp: { name: "Console", id: "console.example" }, user: { id: "AP8", name: "user", displayName: "User" }, pubKeyCredParams: [{ type: "public-key", alg: -7 }], authenticatorSelection: { userVerification: "preferred" }, excludeCredentials: [{ type: "public-key", id: "AP8" }] } });
  assert.equal(options.authenticatorSelection!.userVerification, "required"); assert.equal(options.rp.id, "console.example");
  assert.deepEqual(new Uint8Array(options.user.id as ArrayBuffer), new Uint8Array([0, 255]));
  assert.deepEqual(new Uint8Array(options.excludeCredentials![0].id as ArrayBuffer), new Uint8Array([0, 255]));
});
test("native registration sends only WebAuthn attestation bytes, transport hints and extensions", async () => {
  const { createPasskey } = await import("./passkey.ts");
  const names = ["navigator", "PublicKeyCredential", "isSecureContext"] as const;
  const saved = names.map(name => Object.getOwnPropertyDescriptor(globalThis, name));
  let calls = 0;
  try {
    Object.defineProperty(globalThis, "isSecureContext", { configurable: true, value: true });
    Object.defineProperty(globalThis, "PublicKeyCredential", { configurable: true, value: class {} });
    Object.defineProperty(globalThis, "navigator", { configurable: true, value: { credentials: { create: async (options: CredentialCreationOptions) => {
      calls++; assert.equal(options.publicKey!.authenticatorSelection!.userVerification, "required");
      return { id: "AP8", rawId: new Uint8Array([0, 255]).buffer, type: "public-key", response: { attestationObject: new Uint8Array([1, 2]).buffer, clientDataJSON: new Uint8Array([3, 4]).buffer, getTransports: () => ["internal"] }, getClientExtensionResults: () => ({ credProps: { rk: true } }) };
    } } } });
    const credential = await createPasskey({ flow_id: "flow", publicKey: { challenge: "YWJj", rp: { name: "Console" }, user: { id: "AP8", name: "u", displayName: "User" }, pubKeyCredParams: [{ type: "public-key", alg: -7 }] } });
    assert.deepEqual(credential, { id: "AP8", rawId: "AP8", type: "public-key", response: { attestationObject: "AQI", clientDataJSON: "AwQ", transports: ["internal"] }, extensions: { credProps: { rk: true } } });
    assert.equal(calls, 1);
  } finally { names.forEach((name, i) => { if (saved[i]) Object.defineProperty(globalThis, name, saved[i]!); else delete (globalThis as unknown as Record<string, unknown>)[name]; }); }
});
