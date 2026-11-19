import type { SourceConnectionIntent, SourceConnectionReceipt } from "@/api/knowledge";
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
export const cloudHostname = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.atlassian\.net$/;
export const connectionStorageKey = (user: string, dataset: string) => `kb-source-connection:${JSON.stringify([user, dataset])}`;
export function readConnectionIntent(key: string): SourceConnectionIntent | null {
  const raw = sessionStorage.getItem(key); if (!raw) return null;
  const value = JSON.parse(raw) as SourceConnectionIntent;
  if (!value || Object.keys(value).sort().join() !== "domain,name,request_id" || !uuid.test(value.request_id) ||
      typeof value.name !== "string" || !value.name.trim() || value.name.length > 255 || typeof value.domain !== "string" || !cloudHostname.test(value.domain)) throw Error("Invalid recovery identity");
  return value;
}
export function validateConnectionReceipt(value: SourceConnectionReceipt, intent: SourceConnectionIntent) {
  if (!value || value.connection_id !== intent.request_id || value.name !== intent.name || value.domain !== intent.domain ||
      typeof value.status !== "string" || !value.status || value.sync_mode !== "manual" ||
      Object.keys(value).sort().join() !== "connection_id,domain,name,status,sync_mode") throw Error("Invalid connection receipt");
  return value;
}
export function connectionNotAccepted(error: unknown): boolean {
  const detail = (error as { response?: { data?: { detail?: { code?: unknown; accepted?: unknown } } } })?.response?.data?.detail;
  return detail?.code === "CONFLUENCE_CONNECTION_NOT_ACCEPTED" && detail.accepted === false;
}

export function connectionRejectionReason(error: unknown): "encryptionUnavailable" | "rejected" {
  const reason = (error as { response?: { data?: { detail?: { reason?: unknown } } } })?.response?.data?.detail?.reason;
  return reason === "encryption_unavailable" ? "encryptionUnavailable" : "rejected";
}
