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
export function applyConsoleHeaders(headers: Headers, method: string, identity: ConsoleSessionIdentity, sameOrigin: boolean): void {
  const authorization = headers.get("Authorization");
  if (authorization?.startsWith(`Bearer ${CONSOLE_SESSION_PREFIX}`)) {
    if (authorization !== `Bearer ${identity.token}`) throw new Error("Console session changed before request");
    headers.delete("Authorization");
  }
  if (!sameOrigin || headers.get("Authorization")?.startsWith("Bearer ")) { headers.delete("X-CSRF-Token"); return; }
  if (stateChanging(method) && identity.isAuthenticated) {
    if (!identity.csrfToken) throw new Error("Console session CSRF bootstrap required");
    headers.set("X-CSRF-Token", identity.csrfToken);
  }
}
export function createConsoleFetch(
  nativeFetch: typeof fetch,
  origin: string,
  identity: () => ConsoleSessionIdentity & { generation: number },
  unauthorized: () => void,
): typeof fetch {
  return async (input, init) => {
    const url = new URL(typeof input === "string" ? input : input instanceof URL ? input.href : input.url, origin);
    const owned = url.origin === origin && /^\/(api|v1)\//.test(url.pathname);
    const before = identity();
    const method = init?.method ?? (input instanceof Request ? input.method : "GET");
    const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
    applyConsoleHeaders(headers, method, before, owned);
    const sdkBearer = headers.get("Authorization")?.startsWith("Bearer ");
    const response = await nativeFetch(input, { ...init, headers, ...(sdkBearer ? { credentials: "omit" as const } : owned ? { credentials: "same-origin" as const } : {}) });
    // No retries, including CSRF failures or unknown POST outcomes. The collector
    // cannot expire the application session merely because diagnostics failed.
    if (owned && !sdkBearer && response.status === 401 && !url.pathname.startsWith("/api/v1/telemetry/")
      && before.generation === identity().generation) unauthorized();
    return response;
  };
}
