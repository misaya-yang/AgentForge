import type { RequestBody } from "@/api/contracts";
export type AssertionWire = RequestBody<"POST /api/auth/enterprise/passkeys/authenticate/finish">["credential"];
export type RegistrationWire = RequestBody<"POST /api/auth/enterprise/passkeys/register/finish">["credential"];
/** WebAuthn wire conversion. Assertion bytes stay in the ceremony request only. */
export interface AssertionChallenge {
  flow_id: string;
  publicKey: Omit<PublicKeyCredentialRequestOptions, "challenge" | "allowCredentials"> & {
    challenge: string;
    allowCredentials?: (Omit<PublicKeyCredentialDescriptor, "id"> & { id: string })[];
  };
}
export function decodeBase64url(value: string): ArrayBuffer {
  if (!value || !/^[A-Za-z0-9_-]+$/.test(value)) throw new Error("Invalid passkey challenge");
  const bytes = Uint8Array.from(atob(value.replace(/-/g, "+").replace(/_/g, "/")), c => c.charCodeAt(0));
  return bytes.buffer;
}
export function encodeBase64url(value: ArrayBuffer): string {
  return btoa(Array.from(new Uint8Array(value), byte => String.fromCharCode(byte)).join(""))
    .replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
export function assertionOptions(challenge: AssertionChallenge): PublicKeyCredentialRequestOptions {
  if (!challenge.flow_id || !challenge.publicKey || challenge.publicKey.userVerification !== "required") {
    throw new Error("Passkey user verification is required");
  }
  const key = challenge.publicKey;
  return { ...key, challenge: decodeBase64url(key.challenge),
    allowCredentials: key.allowCredentials?.map(item => ({ ...item, id: decodeBase64url(item.id) })) };
}
export async function getPasskeyAssertion(challenge: AssertionChallenge, signal?: AbortSignal): Promise<AssertionWire> {
  if (!globalThis.isSecureContext || typeof PublicKeyCredential === "undefined" || !navigator.credentials?.get) {
    throw new Error("Passkeys require a supported browser and a secure connection");
  }
  const credential = await navigator.credentials.get({ publicKey: assertionOptions(challenge), signal }) as PublicKeyCredential | null;
  if (!credential || credential.type !== "public-key") throw new Error("Passkey verification cancelled");
  const response = credential.response as AuthenticatorAssertionResponse;
  return { id: credential.id, rawId: encodeBase64url(credential.rawId), type: credential.type,
    response: { authenticatorData: encodeBase64url(response.authenticatorData),
      clientDataJSON: encodeBase64url(response.clientDataJSON), signature: encodeBase64url(response.signature),
      userHandle: response.userHandle ? encodeBase64url(response.userHandle) : null },
    extensions: credential.getClientExtensionResults() };
}

export interface RegistrationChallenge {
  flow_id: string;
  publicKey: Omit<PublicKeyCredentialCreationOptions, "challenge" | "user" | "excludeCredentials"> & {
    challenge: string;
    user: Omit<PublicKeyCredentialUserEntity, "id"> & { id: string };
    excludeCredentials?: (Omit<PublicKeyCredentialDescriptor, "id"> & { id: string })[];
  };
}
export function registrationOptions(challenge: RegistrationChallenge): PublicKeyCredentialCreationOptions {
  const key = challenge?.publicKey;
  if (!challenge?.flow_id || !key?.user?.id || !key.rp?.name || !Array.isArray(key.pubKeyCredParams)) throw new Error("Invalid passkey registration challenge");
  return { ...key, challenge: decodeBase64url(key.challenge), user: { ...key.user, id: decodeBase64url(key.user.id) },
    // Native finish independently rejects registrations without actual UV.
    authenticatorSelection: { ...key.authenticatorSelection, userVerification: "required" },
    excludeCredentials: key.excludeCredentials?.map(item => ({ ...item, id: decodeBase64url(item.id) })) };
}
export async function createPasskey(challenge: RegistrationChallenge, signal?: AbortSignal): Promise<RegistrationWire> {
  if (!globalThis.isSecureContext || typeof PublicKeyCredential === "undefined" || !navigator.credentials?.create) throw new Error("Passkeys require a supported browser and a secure connection");
  const credential = await navigator.credentials.create({ publicKey: registrationOptions(challenge), signal }) as PublicKeyCredential | null;
  if (!credential || credential.type !== "public-key") throw new Error("Passkey registration cancelled");
  const response = credential.response as AuthenticatorAttestationResponse;
  return { id: credential.id, rawId: encodeBase64url(credential.rawId), type: credential.type,
    response: { attestationObject: encodeBase64url(response.attestationObject), clientDataJSON: encodeBase64url(response.clientDataJSON), transports: response.getTransports?.() },
    extensions: credential.getClientExtensionResults() };
}
