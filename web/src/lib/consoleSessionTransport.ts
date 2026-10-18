import { sendGeneratedRequest } from "./fetchClient.ts";
/** Browser BFF transport. The marker fences UI work; it is never a credential. */
export const CONSOLE_SESSION_PREFIX = "console-session:";
export const CONSOLE_SESSION_PATH = "/api/auth/session";

export interface ConsoleSessionIdentity {
  token: string | null;
  csrfToken: string | null;
  isAuthenticated: boolean;
}

export function isConsoleSessionMarker(value: string | null | undefined): boolean {
  return Boolean(value?.startsWith(CONSOLE_SESSION_PREFIX));
}

export function stateChanging(method: string): boolean {
  return !["GET", "HEAD", "OPTIONS"].includes(method.toUpperCase());
}

export function applyConsoleHeaders(
  headers: Headers,
  method: string,
  identity: ConsoleSessionIdentity,
  sameOrigin: boolean,
): void {
  const authorization = headers.get("Authorization");
  if (authorization?.startsWith(`Bearer ${CONSOLE_SESSION_PREFIX}`)) {
    if (authorization !== `Bearer ${identity.token}`) {
      throw new Error("Console session changed before request");
    }
    headers.delete("Authorization");
  }
  if (!sameOrigin || headers.get("Authorization")?.startsWith("Bearer ")) {
    headers.delete("X-CSRF-Token");
    return;
  }
  if (!stateChanging(method) || !identity.isAuthenticated) return;
  if (!identity.csrfToken) {
    throw new Error("Console session CSRF bootstrap required");
  }
  headers.set("X-CSRF-Token", identity.csrfToken);
}

export function createConsoleFetch(
  nativeFetch: typeof fetch,
  origin: string,
  identity: () => ConsoleSessionIdentity & { generation: number },
  unauthorized: () => void,
): typeof fetch {
  return async function consoleFetch(input, init): Promise<Response> {
    let requestUrl: string;
    if (typeof input === "string") {
      requestUrl = input;
    } else if (input instanceof URL) {
      requestUrl = input.href;
    } else {
      requestUrl = input.url;
    }
    const url = new URL(requestUrl, origin);
    const isConsoleRequest = url.origin === origin && /^\/(api|v1)\//.test(url.pathname);
    const requestIdentity = identity();
    const method = init?.method ?? (input instanceof Request ? input.method : "GET");
    const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
    applyConsoleHeaders(headers, method, requestIdentity, isConsoleRequest);

    const hasSdkBearer = headers.get("Authorization")?.startsWith("Bearer ");
    const requestInit: RequestInit = { ...init, headers };
    if (hasSdkBearer) {
      requestInit.credentials = "omit";
    } else if (isConsoleRequest) {
      requestInit.credentials = "same-origin";
    }
    let response: Response;
    if (isConsoleRequest) {
      // Raw response consumers (Quiz/shares/telemetry) use the same generated
      // codec as the view-envelope API and SSE clients. Snapshot ownership is
      // captured above, before any asynchronous Request-body materialization.
      if (input instanceof Request && input.body !== null && init?.body === undefined && !["GET", "HEAD"].includes(method.toUpperCase())) {
        const copy = input.clone();
        requestInit.body = (headers.get("Content-Type") ?? "").startsWith("multipart/form-data") ? await copy.formData() : await copy.text();
        requestInit.signal ??= input.signal;
      }
      if (!hasSdkBearer && requestIdentity.generation !== identity().generation) throw new Error("Console session changed before request");
      response = await sendGeneratedRequest(nativeFetch, url.pathname + url.search, requestInit);
    } else response = await nativeFetch(input, requestInit);
    // No retries, including CSRF failures or unknown POST outcomes. The collector
    // cannot expire the application session merely because diagnostics failed.
    if (!isConsoleRequest || hasSdkBearer || response.status !== 401
      || url.pathname.startsWith("/api/v1/telemetry/")) {
      return response;
    }
    if (requestIdentity.generation === identity().generation) unauthorized();
    return response;
  };
}
