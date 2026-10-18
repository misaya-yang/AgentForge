import { binaryOperations, nonJsonRequestMediaTypes, createClient, queryOptions as generatedQueryOptions, queryKey as generatedQueryKey, type ApiRequest, type ApiResponse, type ApiEvent, type UnaryOperation, type StreamOperation } from "../../../sdk/typescript/generated";
import { api } from "@/lib/api";
import { sseFetchEvents, type SSEEvent, type SSEFetchOptions } from "@/lib/sse";

/** Origin/CSRF/cookies are browser authority, never generated request input. */
type AutomaticHeader = "Origin" | "Sec-Fetch-Site" | "X-CSRF-Token" | "Authorization" | "Cookie";
export type ConsoleApiRequest<K extends UnaryOperation | StreamOperation> = Omit<ApiRequest<K>, "header" | "cookie"> & {
  header?: ApiRequest<K> extends { header?: infer H } ? Omit<NonNullable<H>, AutomaticHeader> : never;
};
interface WireInput { path?: Record<string, unknown>; query?: Record<string, unknown>; header?: Record<string, unknown>; body?: unknown; content_type?: string }
export function operationRequest(operation: string, value: unknown) {
  const input = value as WireInput;
  const split = operation.indexOf(" ");
  if (split < 1) throw new Error("Invalid generated operation");
  const method = operation.slice(0, split), template = operation.slice(split + 1);
  const url = template.replace(/\{([^}]+)\}/g, (_, name: string) => {
    const part = input.path?.[name];
    if ((typeof part !== "string" && typeof part !== "number") || String(part).length === 0) throw new Error("Missing generated path parameter");
    return encodeURIComponent(String(part));
  });
  if (!url.startsWith("/api/") && !url.startsWith("/v1/")) throw new Error("Console operation must use its same-origin API");
  const headers = new Headers();
  for (const [name, value] of Object.entries(input.header ?? {})) {
    if (["origin", "sec-fetch-site", "x-csrf-token", "authorization", "cookie"].includes(name.toLowerCase())) throw new Error("Generated request cannot replace browser authority");
    if (value !== undefined) headers.set(name, String(value));
  }
  if (input.content_type) headers.set("Content-Type", input.content_type);
  return { method, url, headers, input };
}

/** Encode the published body media without changing the existing HTTP authority. */
export function operationBody(operation: string, input: WireInput, headers: Headers): unknown {
  const declared = (nonJsonRequestMediaTypes as Record<string, readonly string[]>)[operation];
  if (declared && declared.length > 1 && !input.content_type) throw new Error("Generated request media selection is required");
  const media = input.content_type ?? (declared?.length === 1 ? declared[0] : undefined);
  if (media && declared && !declared.includes(media)) throw new Error("Unsupported generated request media type");
  if (media !== "multipart/form-data") {
    if (media && media !== "*/*" && input.body !== undefined && !headers.has("Content-Type")) headers.set("Content-Type", media);
    return input.body;
  }
  if (input.body === undefined) return undefined;
  headers.delete("Content-Type"); // The browser owns the multipart boundary.
  if (input.body instanceof FormData) return input.body;
  if (!input.body || typeof input.body !== "object" || Array.isArray(input.body)) throw new Error("Generated multipart body must be an object");
  const form = new FormData();
  for (const [key, field] of Object.entries(input.body)) {
    for (const value of Array.isArray(field) ? field : [field]) {
      if (value === undefined || value === null) continue;
      if (value instanceof Blob) form.append(key, value);
      else form.append(key, typeof value === "object" ? JSON.stringify(value) : String(value));
    }
  }
  return form;
}

/** Dynamic server-provided URLs may only read a published binary GET route. */
export function binaryOperationForUrl(value: string): { operation: string; url: string } {
  const forbidden = (text: string) => text.includes("\\") || [...text].some(char => char.charCodeAt(0) <= 32);
  if (!value.startsWith("/api/") || forbidden(value) || value.includes("#")) throw new Error("Invalid same-origin binary URL");
  const pathname = value.split("?")[0];
  // Reject path normalization/encoded separators before a browser or proxy can
  // reinterpret a purported artifact URL as a different operation.
  for (const segment of pathname.split("/")) {
    let decoded: string; try { decoded = decodeURIComponent(segment); } catch { throw new Error("Invalid binary URL encoding"); }
    if (decoded === "." || decoded === ".." || decoded.includes("/") || forbidden(decoded)) throw new Error("Invalid binary URL path");
  }
  for (const operation of binaryOperations) {
    if (!operation.startsWith("GET ")) continue;
    const template = operation.slice(4);
    const escaped = template.replace(/[.*+?^${}()|[\]\\]/g, "\\$&").replace(/\\\{[^}]+\\\}/g, "[^/]+");
    if (new RegExp(`^${escaped}$`).test(pathname)) return { operation, url: value };
  }
  throw new Error("URL is not a published binary operation");
}
export async function readApiBlob(value: string, signal?: AbortSignal): Promise<Blob> {
  const { url } = binaryOperationForUrl(value);
  return (await api.get<Blob>(url, { responseType: "blob", signal })).data;
}

// Delegate to the existing shared BFF adapter, preserving passkey step-up,
// synchronous account fences, CSRF, error identity and single-attempt writes.
const generated = createClient({
  async request<K extends UnaryOperation>(operation: K, value: ApiRequest<K>, signal?: AbortSignal): Promise<ApiResponse<K>> {
    const { method, url, headers, input } = operationRequest(operation, value);
    const data = operationBody(operation, input, headers);
    const response = await api.request<ApiResponse<K>>({ method, url, params: input.query,
      data, headers: Object.fromEntries(headers), signal });
    return response.data;
  },
  stream() {
    // Returning data alone would discard SSE resume/reset/control frames.
    // Console callers use streamEvents below, retaining the actual envelope.
    throw new Error("Console streams require streamEvents to preserve control frames");
  },
});
export function request<K extends UnaryOperation>(operation: K, value: ConsoleApiRequest<K>, signal?: AbortSignal): Promise<ApiResponse<K>> {
  return generated.request(operation, value as ApiRequest<K>, signal);
}
/** The same generated key is used for reads and typed cache invalidation. */
export function readKey<K extends Extract<UnaryOperation, `GET ${string}`>>(scope: string, operation: K, input: ConsoleApiRequest<K>) {
  return generatedQueryKey(scope, operation, input as ApiRequest<K>);
}
export function readOptions<K extends Extract<UnaryOperation, `GET ${string}`>>(scope: string, operation: K, input: ConsoleApiRequest<K>) {
  return generatedQueryOptions(generated, scope, operation, input as ApiRequest<K>);
}
export async function* streamEvents<K extends StreamOperation>(operation: K, value: ConsoleApiRequest<K>, options: Omit<SSEFetchOptions, "method" | "body" | "headers">): AsyncGenerator<SSEEvent<ApiEvent<K>>> {
  const { method, url, headers, input } = operationRequest(operation, value);
  const query = new URLSearchParams();
  for (const [name, value] of Object.entries(input.query ?? {})) if (value != null) query.set(name, String(value));
  if (input.body !== undefined && !headers.has("Content-Type")) headers.set("Content-Type", "application/json");
  yield* sseFetchEvents<ApiEvent<K>>(`${url}${query.size ? `?${query}` : ""}`, { ...options, method, headers,
    body: input.body === undefined ? undefined : JSON.stringify(input.body), preserveControlFrames: true });
}
