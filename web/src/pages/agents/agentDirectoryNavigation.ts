/** Opaque server cursors; never infer a total or an offset from their contents. */
export const AGENT_DIRECTORY_PAGE_SIZE = 25;
const keys = { search: "agent_search", status: "agent_status", owner: "agent_owner", cursor: "agent_cursor" } as const;
type Field = keyof typeof keys;
export function agentDirectoryLocation(params: URLSearchParams) {
  const status = params.get(keys.status) ?? undefined;
  return {
    search: (params.get(keys.search) ?? "").slice(0, 255),
    status: status && ["draft", "active", "archived"].includes(status) ? status : undefined,
    owner: params.get(keys.owner) || undefined,
    cursor: params.get(keys.cursor) || undefined,
  };
}
export function agentDirectoryParams(params: URLSearchParams, patch: Partial<Record<Field, string | undefined>>) {
  const next = new URLSearchParams(params);
  for (const key of Object.keys(patch) as Field[]) {
    const value = patch[key];
    if (value) next.set(keys[key], value); else next.delete(keys[key]);
  }
  return next;
}
export function agentDirectoryNextCursor(value: unknown, current?: string): string | undefined {
  return typeof value === "string" && value.length > 0 && value !== current ? value : undefined;
}
