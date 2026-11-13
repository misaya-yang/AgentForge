import type { RequestBody } from "@/api/contracts";
import { api } from "@/lib/api";

export type SpaceVisibility = "private" | "members" | "tenant";
export type ItemKind = "page" | "folder" | "file" | "link" | "slide" | "site" | "image" | "external";
export type ItemView = "all" | "recent" | "starred" | "mine" | "shared";

export interface SpaceRecord {
  space_id: string;
  name: string;
  icon: string;
  kind: string;
  visibility: SpaceVisibility;
  dataset_id: string;
  created_by: string;
  created_at: string;
  updated_at: string;
}

export interface SpaceItem {
  item_id: string;
  space_id: string;
  space_name: string;
  space_icon: string;
  parent_id: string | null;
  kind: ItemKind;
  title: string;
  icon: string;
  position: string;
  source: string;
  created_by: string;
  updated_by: string;
  content_format: string;
  current_revision: number;
  starred: boolean;
  shared: boolean;
  last_activity_at: string;
  updated_at: string;
  deleted_at: string | null;
}

export interface ItemPage {
  items: SpaceItem[];
  next_cursor: string | null;
}

export interface ItemContent {
  item_id: string;
  revision: number;
  content_format: string;
  blocks: unknown;
  markdown: string;
  content_sha256: string;
}

export interface ItemQuery {
  view?: ItemView;
  kind?: ItemKind;
  q?: string;
  space_id?: string;
  cursor?: string;
  limit?: number;
}

const space = (spaceId: string) => `/api/v1/knowledge/spaces/${encodeURIComponent(spaceId)}`;
const item = (itemId: string) => `/api/v1/knowledge/space-items/${encodeURIComponent(itemId)}`;

export async function listSpaces() {
  const { data } = await api.get<{ spaces?: SpaceRecord[] }>("/api/v1/knowledge/spaces");
  return data.spaces ?? [];
}

export async function createSpace(body: RequestBody<"POST /api/v1/knowledge/spaces">) {
  const { data } = await api.post<SpaceRecord>("/api/v1/knowledge/spaces", body);
  return data;
}

function page(data: Partial<ItemPage> | undefined): ItemPage {
  return { items: data?.items ?? [], next_cursor: data?.next_cursor ?? null };
}

export async function listItems(query: ItemQuery) {
  const { data } = await api.get<Partial<ItemPage>>("/api/v1/knowledge/space-items", { params: query });
  return page(data);
}

export async function listTrash(query: Omit<ItemQuery, "view"> = {}) {
  const { data } = await api.get<Partial<ItemPage>>("/api/v1/knowledge/space-trash", { params: query });
  return page(data);
}

export async function createItem(spaceId: string, body: RequestBody<"POST /api/v1/knowledge/spaces/{space_id}/items">) {
  const { data } = await api.post<SpaceItem>(`${space(spaceId)}/items`, body);
  return data;
}

export async function getItem(itemId: string) {
  const { data } = await api.get<Partial<SpaceItem>>(item(itemId));
  if (!data.item_id) throw new Error("missing item");
  return data as SpaceItem;
}

export async function patchItem(itemId: string, body: RequestBody<"PATCH /api/v1/knowledge/space-items/{item_id}">) {
  const { data } = await api.patch<SpaceItem>(item(itemId), body);
  return data;
}

export async function deleteItem(itemId: string) {
  await api.delete(item(itemId));
}

export async function restoreItem(itemId: string) {
  const { data } = await api.post<SpaceItem>(`${item(itemId)}/restore`);
  return data;
}

export async function starItem(itemId: string, on: boolean) {
  if (on) await api.put(`${item(itemId)}/star`);
  else await api.delete(`${item(itemId)}/star`);
}

export async function replaceShares(itemId: string, body: RequestBody<"PUT /api/v1/knowledge/space-items/{item_id}/shares">) {
  const { data } = await api.put<{ shares: Array<{ principal_type: string; principal_id: string; role: string }> }>(`${item(itemId)}/shares`, body);
  return data.shares;
}

export async function getContent(itemId: string) {
  const response = await api.get<ItemContent>(`${item(itemId)}/content`);
  return response.data;
}

export async function putContent(itemId: string, revision: number, body: RequestBody<"PUT /api/v1/knowledge/space-items/{item_id}/content">) {
  const { data } = await api.put<ItemContent>(`${item(itemId)}/content`, body, { headers: { "If-Match": `"${revision}"` } });
  return data;
}
