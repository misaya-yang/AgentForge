import { operationKeys, createFetchClient, type ApiOperations, type Operation } from "../../../sdk/typescript/generated.ts";

export class HttpHeaders {
  [key: string]: unknown;
  private entries = new Map<string, { name: string; value: string }>();
  constructor(input?: HeadersInit | HttpHeaders | Record<string, unknown>) {
    if (input instanceof HttpHeaders) input = input.toJSON();
    if (input && typeof (input as Headers).forEach === "function") (input as Headers).forEach((value, name) => this.set(name, value));
    else if (Array.isArray(input)) input.forEach(([name, value]) => this.set(name, value));
    else for (const [name, value] of Object.entries(input ?? {})) if (value !== undefined && value !== null) this.set(name, value);
    return new Proxy(this, { get: (target, name, receiver) => typeof name === "string" && !(name in target) ? target.get(name) ?? undefined : Reflect.get(target, name, receiver) });
  }
  set(name: string, value: unknown) { this.entries.set(name.toLowerCase(), { name, value: String(value) }); return this; }
  get(name: string): string | null { return this.entries.get(name.toLowerCase())?.value ?? null; }
  has(name: string): boolean { return this.entries.has(name.toLowerCase()); }
  delete(name: string): boolean { return this.entries.delete(name.toLowerCase()); }
  toJSON(): Record<string, string> { return Object.fromEntries([...this.entries.values()].map(({ name, value }) => [name, value])); }
}
export interface HttpProgress { loaded: number; total?: number; progress?: number }
export type RequestTransform = (data: unknown, headers: HttpHeaders) => unknown;
export interface RequestConfig {
  url?: string; method?: string; baseURL?: string; params?: object | URLSearchParams;
  data?: unknown; headers: HttpHeaders; signal?: AbortSignal; timeout?: number;
  withCredentials?: boolean; responseType?: "json" | "text" | "blob" | "arraybuffer" | "response";
  adapter?: RequestAdapter | "fetch"; transformRequest?: RequestTransform | RequestTransform[];
  onUploadProgress?: (progress: HttpProgress) => void;
  beforeSend?: () => void; authorityCurrent?: () => boolean;
  preflight?: () => Promise<(() => void)>; afterDispatch?: () => void;
}
export type RequestOptions = Omit<Partial<RequestConfig>, "headers"> & { headers?: HeadersInit | HttpHeaders | Record<string, unknown> };
export interface HttpResponse<T = unknown> { data: T; status: number; statusText: string; headers: Record<string, string>; config: RequestConfig }
export class HttpError<T = unknown> extends Error {
  readonly isHttpError = true; code?: string; config?: RequestConfig; response?: HttpResponse<T>; retryAfter?: number;
  constructor(message: string, code?: string, config?: RequestConfig, _request?: unknown, response?: HttpResponse<T>) {
    super(message); this.name = "HttpError"; this.code = code; this.response = response;
    Object.defineProperty(this, "config", { value: config, writable: true, enumerable: false });
  }
  toJSON() { return { name: this.name, message: this.message, code: this.code, status: this.response?.status }; }
}
export function isHttpError<T = unknown>(error: unknown): error is HttpError<T> { return Boolean(error && typeof error === "object" && (error as { isHttpError?: boolean }).isHttpError === true); }
export type RequestAdapter = (config: RequestConfig) => Promise<HttpResponse>;
type Hook<T> = { success?: (input: T) => T | Promise<T>; failure?: (error: unknown) => unknown };
class Hooks<T> {
  handlers: Array<Hook<T> | undefined> = [];
  use(success?: Hook<T>["success"], failure?: Hook<T>["failure"], _options?: { synchronous?: boolean }) { void _options; this.handlers.push({ success, failure }); return this.handlers.length - 1; }
  eject(index: number) { this.handlers[index] = undefined; }
}
const routeTable = operationKeys.filter(key => !/\{\*?(?:path|rest|tail|adapter_schema)\}/.test(key)).map(key => {
  const split = key.indexOf(" "), template = key.slice(split + 1), names = [...template.matchAll(/\{([^}]+)\}/g)].map(match => match[1]);
  const pattern = template.replace(/[.*+?^${}()|[\]\\]/g, "\\$&").replace(/\\\{[^}]+\\\}/g, "([^/]+)");
  return { key: key as Operation, method: key.slice(0, split), template, names, pattern: new RegExp(`^${pattern}$`) };
}).sort((a, b) => a.names.length - b.names.length);
function queryValues(params: URLSearchParams): Record<string, string | string[]> {
  const result: Record<string, string | string[]> = {};
  for (const [key, value] of params) { const previous=result[key]; result[key]=previous === undefined ? value : [...(Array.isArray(previous) ? previous : [previous]),value]; }
  return result;
}
export function resolveOperation(method: string, value: string) {
  const split = value.indexOf("?"), pathname = split < 0 ? value : value.slice(0, split), search = split < 0 ? "" : value.slice(split + 1);
  for (const route of routeTable) {
    if (route.method !== method.toUpperCase()) continue;
    const match = route.pattern.exec(pathname); if (!match) continue;
    return { operation: route.key, path: Object.fromEntries(route.names.map((name, index) => [name, decodeURIComponent(match[index + 1])])), query: queryValues(new URLSearchParams(search)) };
  }
  throw new Error("Console operation is not in the generated contract");
}
/** Raw SSE response through the same generated wire codec. The SSE reader
 * retains ownership of framing, cursors, keepalive budgets and read-only resume. */
export function sendGeneratedRequest(fetcher: typeof fetch, url: string, init: RequestInit): Promise<Response> {
  const route = resolveOperation(init.method ?? "GET", url);
  let body: unknown = init.body ?? undefined;
  if (typeof body === "string") body = JSON.parse(body);
  const sdk = createFetchClient((target, request) => fetcher(target, request));
  return sdk.send(route.operation, { path: route.path, query: route.query, body } as ApiOperations[Operation]["request"], { ...init, signal: init.signal ?? undefined, credentials: new Headers(init.headers).get("Authorization")?.startsWith("Bearer ") ? "omit" : "same-origin" });
}
export function sendStreamRequest(url: string, init: RequestInit): Promise<Response> {
  return sendGeneratedRequest(globalThis.fetch, url, init);
}
function hasBinary(data: unknown): boolean {
  return Boolean(data && typeof data === "object" && (data instanceof Blob || data instanceof FormData || data instanceof ArrayBuffer || ArrayBuffer.isView(data) || (Array.isArray(data) ? data : Object.values(data)).some(hasBinary)));
}
function dataSize(data: unknown): number | undefined {
  if (typeof FormData !== "undefined" && data instanceof FormData) return [...data.values()].reduce((sum, value) => sum + (typeof value === "string" ? value.length : value.size), 0);
  if (typeof Blob !== "undefined" && data instanceof Blob) return data.size;
  if (data && typeof data === "object" && hasBinary(data)) return (Array.isArray(data) ? data : Object.values(data)).reduce((sum: number, value: unknown) => sum + (dataSize(value) ?? 0), 0);
  return typeof data === "string" ? new TextEncoder().encode(data).length : undefined;
}
async function nativeAdapter(config: RequestConfig): Promise<HttpResponse> {
  const controller = new AbortController(); let timedOut = false;
  const abort = () => controller.abort(config.signal?.reason);
  config.signal?.addEventListener("abort", abort, { once: true });
  const timer = config.timeout ? setTimeout(() => { timedOut = true; controller.abort(); }, config.timeout) : undefined;
  try {
    config.signal?.throwIfAborted();
    const route = resolveOperation(config.method ?? "GET", config.url ?? "");
    const headers = config.headers.toJSON();
    let body = config.data;
    if (typeof body === "string" && /json/i.test(config.headers.get("Content-Type") ?? "")) body = JSON.parse(body);
    const query = config.params instanceof URLSearchParams ? queryValues(config.params) : config.params;
    const sdk = createFetchClient((url, init) => {
      const origin = typeof window !== "undefined" ? window.location.origin : undefined;
      const base = config.baseURL || "";
      const target = base ? `${base.replace(/\/$/, "")}/${url.replace(/^\//, "")}` : url;
      if (origin && new URL(target, origin).origin !== origin) throw new Error("Console BFF requests require the same origin");
      return globalThis.fetch(target, init);
    });
    const response = await sdk.send(route.operation, { path: route.path, query: { ...route.query, ...query }, body } as ApiOperations[Operation]["request"], { headers, signal: controller.signal, credentials: config.withCredentials === false ? "omit" : "same-origin" });
    const responseHeaders = Object.fromEntries(response.headers);
    const type = config.responseType;
    const data = type === "response" && response.ok ? response : type === "blob" && response.ok ? await response.blob() : type === "arraybuffer" && response.ok ? await response.arrayBuffer() : type === "text" && response.ok ? await response.text() : await response.text().then(text => { if (!text) return undefined; try { return JSON.parse(text); } catch { return text; } });
    const result = { data, status: response.status, statusText: response.statusText, headers: responseHeaders, config };
    if (!response.ok) throw new HttpError(`Request failed with status code ${response.status}`, response.status >= 500 ? "ERR_BAD_RESPONSE" : "ERR_BAD_REQUEST", config, undefined, result);
    const total = dataSize(config.data); if (total !== undefined) config.onUploadProgress?.({ loaded: total, total, progress: 1 });
    return result;
  } catch (error) {
    if (timedOut) throw new HttpError("Request timed out; remote outcome may be unknown", "ECONNABORTED", config);
    if (error instanceof TypeError && !("code" in error)) Object.assign(error, { code: "ERR_NETWORK" });
    throw error;
  } finally { if (timer) clearTimeout(timer); config.signal?.removeEventListener("abort", abort); }
}
export function getAdapter(adapter?: RequestConfig["adapter"]): RequestAdapter { return typeof adapter === "function" ? adapter : nativeAdapter; }
export class FetchClient {
  defaults: { adapter: RequestAdapter; transformRequest: RequestTransform[] } = { adapter: nativeAdapter, transformRequest: [(data, headers) => {
    if (data !== undefined && data !== null && typeof data === "object" && !(typeof FormData !== "undefined" && data instanceof FormData) && !(typeof Blob !== "undefined" && data instanceof Blob) && !(data instanceof ArrayBuffer) && !ArrayBuffer.isView(data) && !hasBinary(data)) { if (!headers.has("Content-Type")) headers.set("Content-Type", "application/json"); return JSON.stringify(data); }
    return data;
  }] };
  interceptors = { request: new Hooks<RequestConfig>(), response: new Hooks<HttpResponse>() };
  onCreate?: (config: RequestConfig) => void; beforeTransport?: (config: RequestConfig) => void;
  request<T = unknown>(options: RequestOptions): Promise<HttpResponse<T>> {
    const pending = (value: unknown): value is Promise<RequestConfig> => Boolean(value && typeof value === "object" && typeof (value as { then?: unknown }).then === "function");
    const config: RequestConfig = { ...options, method: (options.method ?? "get").toLowerCase(), headers: new HttpHeaders(options.headers), adapter: options.adapter ?? this.defaults.adapter };
    const finish = (pending: Promise<HttpResponse>) => { let result = pending; for (const hook of this.interceptors.response.handlers) if (hook) result = result.then(hook.success, hook.failure as ((error: unknown) => HttpResponse)); return result as Promise<HttpResponse<T>>; };
    const dispatch = (prepared: RequestConfig) => {
      const transforms = prepared.transformRequest ?? this.defaults.transformRequest;
      for (const transform of Array.isArray(transforms) ? transforms : [transforms]) prepared.data = transform(prepared.data, prepared.headers);
      const send = () => { prepared.signal?.throwIfAborted(); prepared.beforeSend?.(); this.beforeTransport?.(prepared); return getAdapter(prepared.adapter)(prepared); };
      if (prepared.preflight) return prepared.preflight().then(async cleanup => { try { const response = await send(); prepared.afterDispatch?.(); return response; } finally { cleanup(); } });
      return Promise.resolve(send());
    };
    try {
      this.onCreate?.(config);
      let current: RequestConfig | Promise<RequestConfig> = config;
      for (const hook of [...this.interceptors.request.handlers].reverse()) if (hook?.success) current = pending(current) ? current.then(hook.success) : hook.success(current);
      return finish(pending(current) ? current.then(dispatch) : dispatch(current));
    } catch (error) { return finish(Promise.reject(error)); }
  }
  get<T = unknown>(url: string, options: RequestOptions = {}) { return this.request<T>({ ...options, method: "get", url }); }
  delete<T = unknown>(url: string, options: RequestOptions = {}) { return this.request<T>({ ...options, method: "delete", url }); }
  post<T = unknown>(url: string, data?: unknown, options: RequestOptions = {}) { return this.request<T>({ ...options, method: "post", url, data }); }
  put<T = unknown>(url: string, data?: unknown, options: RequestOptions = {}) { return this.request<T>({ ...options, method: "put", url, data }); }
  patch<T = unknown>(url: string, data?: unknown, options: RequestOptions = {}) { return this.request<T>({ ...options, method: "patch", url, data }); }
}
export function createHttpClient() { return new FetchClient(); }
const fetchHttp = { create: createHttpClient, createHttpClient, getAdapter, isHttpError, HttpError, HttpHeaders, FetchClient };
export default fetchHttp;
