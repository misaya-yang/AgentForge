// Included verbatim by the native SDK producer. Authentication, origin policy,
// retries and error recovery stay in the caller's injected fetch function.
export interface FetchWireInput { path?: Record<string, unknown>; query?: Record<string, unknown>; header?: Record<string, unknown>; body?: unknown; content_type?: string }
export interface FetchOperationShape { request: unknown; response: unknown }
export interface GeneratedFetchOptions extends Omit<RequestInit, "method" | "body" | "signal"> { signal?: AbortSignal }
export function encodeFetchOperation(operation: string, value: unknown, media: readonly string[] = []): { url: string; init: RequestInit } {
  const input = value as FetchWireInput;
  const split = operation.indexOf(" ");
  if (split < 1) throw new Error("Invalid generated operation");
  const method = operation.slice(0, split), template = operation.slice(split + 1);
  const url = template.replace(/\{([^}]+)\}/g, (_, name: string) => {
    const part = input.path?.[name];
    if ((typeof part !== "string" && typeof part !== "number") || String(part).length === 0) throw new Error("Missing generated path parameter");
    return encodeURIComponent(String(part));
  });
  const query = new URLSearchParams();
  for (const [name, raw] of Object.entries(input.query ?? {})) for (const item of Array.isArray(raw) ? raw : [raw]) if (item !== null && item !== undefined) query.append(name, typeof item === "object" ? JSON.stringify(item) : String(item));
  const headers = new Headers();
  for (const [name, item] of Object.entries(input.header ?? {})) if (item !== undefined) headers.set(name, String(item));
  if (media.length > 1 && !input.content_type) throw new Error("Generated request media selection is required");
  const selected = input.content_type ?? (media.length === 1 ? media[0] : "application/json");
  if (input.content_type && !media.includes(input.content_type) && !media.includes("*/*")) throw new Error("Unsupported generated request media type");
  let body: BodyInit | undefined;
  if (input.body !== undefined) {
    if (selected === "multipart/form-data") {
      headers.delete("Content-Type");
      if (input.body instanceof FormData) body = input.body;
      else {
        if (!input.body || typeof input.body !== "object" || Array.isArray(input.body)) throw new Error("Generated multipart body must be an object");
        const form = new FormData();
        for (const [key, raw] of Object.entries(input.body)) for (const item of Array.isArray(raw) ? raw : [raw]) {
          if (item === undefined || item === null) continue;
          if (item instanceof Blob) form.append(key, item);
          else form.append(key, typeof item === "object" ? JSON.stringify(item) : String(item));
        }
        body = form;
      }
    } else if (input.body instanceof Blob || input.body instanceof FormData || input.body instanceof ArrayBuffer || ArrayBuffer.isView(input.body)) {
      body = input.body as BodyInit;
      if (body instanceof FormData) headers.delete("Content-Type");
    } else if (selected === "application/x-www-form-urlencoded") {
      body = new URLSearchParams(input.body as Record<string, string>);
      headers.set("Content-Type", selected);
    } else {
      body = selected === "*/*" && typeof input.body === "string" ? input.body : JSON.stringify(input.body);
      if (selected !== "*/*" && !headers.has("Content-Type")) headers.set("Content-Type", selected);
    }
  }
  return { url: `${url}${query.size ? `?${query}` : ""}`, init: { method, headers, body } };
}
export function createFetchTransport<Ops extends { [K in keyof Ops]: FetchOperationShape }>(fetcher: (url: string, init: RequestInit) => Promise<Response>, media: Readonly<Record<string, readonly string[]>> = {}) {
  return {
    async send<K extends keyof Ops & string>(operation: K, input: Ops[K]["request"], options: GeneratedFetchOptions = {}): Promise<Response> {
      const wire = encodeFetchOperation(operation, input, media[operation]);
      const headers = new Headers(wire.init.headers);
      new Headers(options.headers).forEach((value, name) => headers.set(name, value));
      if (wire.init.body instanceof FormData) headers.delete("Content-Type");
      return fetcher(wire.url, { ...options, ...wire.init, headers, signal: options.signal, credentials: options.credentials ?? "omit" });
    },
    async request<K extends keyof Ops & string>(operation: K, input: Ops[K]["request"], options: GeneratedFetchOptions = {}): Promise<Ops[K]["response"]> {
      const response = await this.send(operation, input, options);
      if (!response.ok) throw Object.assign(new Error(`HTTP ${response.status}`), { response });
      if (response.status === 204 || response.status === 205) return undefined as Ops[K]["response"];
      const mime = response.headers.get("content-type") ?? "";
      if (mime.includes("json")) return await response.json() as Ops[K]["response"];
      const bytes = await response.blob();
      // Native accepted/no-content receipts can be HTTP 202 with no MIME.
      // A typed void response must not become a fabricated empty Blob value.
      return (bytes.size === 0 && !mime ? undefined : bytes) as Ops[K]["response"];
    },
  };
}
