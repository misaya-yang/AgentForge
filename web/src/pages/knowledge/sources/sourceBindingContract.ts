import type { SourceBindingReceipt, SourceBindingRequest, SourceSpace } from "@/api/knowledge";

const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
export const sourceBindingStorageKey = (userId: string, datasetId: string) => `kb-source-binding:${JSON.stringify([userId, datasetId])}`;
export function validateSourceSpace(value: SourceSpace, connectionId: string, spaceKey: string): SourceSpace {
  if (!value || value.connection_id !== connectionId || value.space_key !== spaceKey ||
      typeof value.space_id !== "string" || !value.space_id || typeof value.space_name !== "string" || !value.space_name ||
      typeof value.root_page_id !== "string" || !value.root_page_id || typeof value.root_page_title !== "string") {
    throw new Error("Source space identity unavailable");
  }
  return value;
}
export function validateSourceBinding(value: SourceBindingReceipt, datasetId: string, request: SourceBindingRequest): SourceBindingReceipt {
  validateSourceSpace(value, request.connection_id, request.space_key);
  if (value.binding_id !== request.request_id || value.dataset_id !== datasetId || value.space_id !== request.space_id || value.root_page_id !== request.root_page_id || value.sync_mode !== "manual") {
    throw new Error("Source binding receipt identity unavailable");
  }
  return value;
}
export function readSourceBindingRequest(key: string): SourceBindingRequest | null {
  const raw = sessionStorage.getItem(key);
  if (!raw) return null;
  const value = JSON.parse(raw) as SourceBindingRequest;
  if (!value || typeof value.request_id !== "string" || !uuid.test(value.request_id) ||
      typeof value.connection_id !== "string" || !value.connection_id ||
      typeof value.space_key !== "string" || !/^[A-Za-z0-9_~-]+$/.test(value.space_key) ||
      typeof value.space_id !== "string" || !value.space_id ||
      typeof value.root_page_id !== "string" || !value.root_page_id) throw new Error("Invalid binding recovery identity");
  return value;
}
export function sourceBindingNotAccepted(error: unknown): boolean {
  const detail = (error as { response?: { data?: { detail?: { code?: unknown; accepted?: unknown } } } })?.response?.data?.detail;
  return detail?.code === "CONFLUENCE_BINDING_NOT_ACCEPTED" && detail.accepted === false;
}
