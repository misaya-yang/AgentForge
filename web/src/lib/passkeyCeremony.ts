import type { User } from "@/store/useAuthStore";
import type { AssertionChallenge, RegistrationChallenge, AssertionWire, RegistrationWire } from "./passkey";
export interface EnterpriseGrant { session_type: "cookie"; csrf_token: string; user: User & { tenant_id: string }; force_password_change: boolean }
/** Project only the existing opaque-session grant, never bearer/token fields. */
export function enterpriseGrant(value: unknown, tenant?: string, userId?: string): EnterpriseGrant {
  if (!value || typeof value !== "object") throw new Error("Invalid enterprise session receipt");
  const row = value as Record<string, unknown>, user = row.user as Record<string, unknown> | undefined;
  const nullable = (v: unknown): v is string | null => v === null || typeof v === "string";
  const strings = (v: unknown): v is string[] => Array.isArray(v) && v.every(item => typeof item === "string");
  if (row.session_type !== "cookie" || typeof row.csrf_token !== "string" || !/^[a-f0-9]{64}$/.test(row.csrf_token) || typeof row.force_password_change !== "boolean"
    || !user || typeof user.user_id !== "string" || !user.user_id || typeof user.tenant_id !== "string" || !user.tenant_id
    || (tenant !== undefined && user.tenant_id !== tenant) || (userId !== undefined && user.user_id !== userId)
    || !nullable(user.email) || !nullable(user.display_name) || !nullable(user.department) || !strings(user.roles) || !strings(user.permissions) || typeof user.tier !== "string") throw new Error("Invalid enterprise session receipt");
  return { session_type: "cookie", csrf_token: row.csrf_token, force_password_change: row.force_password_change,
    user: { user_id: user.user_id, tenant_id: user.tenant_id, email: user.email, display_name: user.display_name, department: user.department, roles: [...user.roles], permissions: [...user.permissions], tier: user.tier } };
}
const current = (isCurrent: () => boolean) => { if (!isCurrent()) throw new Error("Enterprise ceremony ownership changed"); };
export async function registerConsolePasskey(client: {
  start(): Promise<RegistrationChallenge>; create(challenge: RegistrationChallenge): Promise<RegistrationWire>; finish(flow: string, credential: RegistrationWire): Promise<unknown>;
}, isCurrent: () => boolean, tenant: string, user: string) {
  current(isCurrent); const challenge = await client.start(); current(isCurrent);
  const credential = await client.create(challenge); current(isCurrent);
  const value = await client.finish(challenge.flow_id, credential); current(isCurrent);
  const grant = enterpriseGrant(value, tenant, user), codes = (value as Record<string, unknown>).recovery_codes;
  if (!Array.isArray(codes) || codes.length !== 10 || !codes.every(code => typeof code === "string" && /^[a-f0-9]{32}$/.test(code)) || new Set(codes).size !== codes.length) throw new Error("Invalid one-time recovery receipt");
  return { grant, codes: [...codes] as string[] };
}
/** This establishes a NEW session. It is never used as current-session step-up. */
export async function signInWithPasskey(client: {
  start(): Promise<AssertionChallenge>; assertion(challenge: AssertionChallenge): Promise<AssertionWire>; finish(flow: string, credential: AssertionWire): Promise<unknown>;
}, isCurrent: () => boolean, tenant: string) {
  current(isCurrent); const challenge = await client.start(); current(isCurrent);
  const credential = await client.assertion(challenge); current(isCurrent);
  const value = await client.finish(challenge.flow_id, credential); current(isCurrent);
  return enterpriseGrant(value, tenant);
}
