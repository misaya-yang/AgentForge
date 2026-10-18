import type { AssertionChallenge, AssertionWire } from "./passkey";

export interface StepUpStatus { enabled: boolean; verified: boolean; passkey_enrolled: boolean }
/** Override the application-wide network/5xx retry policy for sensitive writes. */
export const sensitiveMutationOptions = { retry: false } as const;
export interface StepUpClient {
  status: (signal?: AbortSignal) => Promise<StepUpStatus>;
  start: (signal?: AbortSignal) => Promise<AssertionChallenge>;
  assertion: (challenge: AssertionChallenge, signal?: AbortSignal) => Promise<AssertionWire>;
  finish: (flowId: string, credential: AssertionWire, signal?: AbortSignal) => Promise<{ verified: boolean }>;
}
export function sensitiveConsoleMutation(method: string, path: string): boolean {
  if (["GET", "HEAD", "OPTIONS"].includes(method.toUpperCase())) return false;
  const under = (root: string) => path === root || path.startsWith(`${root}/`);
  return ["/api/v1/api-keys", "/api/v1/config/api-keys", "/api/v1/providers", "/api/v1/roles",
    "/api/v1/config/auth", "/api/v1/sso", "/api/v1/config/sso",
    "/api/auth/enterprise/identities", "/api/auth/enterprise/scim-credentials"].some(under)
    || (under("/api/v1/users") && !path.endsWith("/profile"));
}
export function validateStepUpStatus(value: StepUpStatus): StepUpStatus {
  if (typeof value?.enabled !== "boolean" || typeof value.verified !== "boolean"
    || typeof value.passkey_enrolled !== "boolean" || (!value.enabled && value.verified)) {
    throw new Error("Invalid passkey step-up policy");
  }
  return value;
}
/** Authorization happens before the single business mutation is dispatched.
 * No mutation is supplied here, so failures cannot trigger a replay. */
export async function authorizeConsoleMutation(
  client: StepUpClient, isCurrent: () => boolean, signal?: AbortSignal, force = false,
): Promise<void> {
  const current = () => { if (!isCurrent() || signal?.aborted) throw new Error("Console session changed before sensitive request"); };
  current();
  const status = validateStepUpStatus(await client.status(signal));
  current();
  if (!status.enabled) {
    if (force) throw new Error("Enterprise passkey step-up is not enabled");
    return;
  }
  if (status.verified && !force) return;
  if (!status.passkey_enrolled) throw new Error("An enrolled passkey is required. Enroll from a recent MFA session; password fallback is unavailable.");
  const challenge = await client.start(signal);
  current();
  const credential = await client.assertion(challenge, signal);
  current();
  const receipt = await client.finish(challenge.flow_id, credential, signal);
  current();
  if (receipt?.verified !== true) throw new Error("Passkey step-up was not verified");
}
