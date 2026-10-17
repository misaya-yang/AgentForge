import { browserTraceparent } from "@/observability/browserTelemetry";
import axios, { AxiosError } from "axios";
import { getApiBaseUrlConfig } from "@/config/runtime";
import { toast } from "@/hooks/use-toast";
import i18n from "@/i18n";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";

// Keep browser requests same-origin by default so local dev and E2E use Vite proxy
// instead of relying on backend CORS for ephemeral frontend ports.
const baseURL = getApiBaseUrlConfig();
const AUTH_STORAGE_KEY = "agent-gateway-auth";

// Keep the credential snapshot out of serialized Axios configuration/errors.
// A response from an old request must not log out a newer login or token refresh.
const requestAuth = new WeakMap<object, { token: string | null; authority: string; generation: number }>();

// Equality alone cannot distinguish an earlier A from a replacement A after A→B→A.
// Match the authenticated read scope's synchronous authority invalidation boundary.
let authGeneration = 0;
const unsubscribeAuth = useAuthStore.subscribe((state, previous) => {
  if (state.token !== previous.token || state.csrfToken !== previous.csrfToken || state.isAuthenticated !== previous.isAuthenticated
    || sdkAuthorityScope(state.user) !== sdkAuthorityScope(previous.user)) {
    authGeneration += 1;
  }
});

if (import.meta.hot) {
  import.meta.hot.dispose(() => {
    authGeneration += 1;
    unsubscribeAuth();
  });
}

// Rate limit state tracking to avoid toast spam
let lastRateLimitToast = 0;
const RATE_LIMIT_TOAST_COOLDOWN = 5000; // Only show toast once per 5 seconds

export const api = axios.create({
  baseURL,
});

/**
 * Get the API base URL for external references (e.g., code examples).
 * Uses configured VITE_API_BASE_URL or falls back to current origin.
 */
export function getApiBaseUrl(): string {
  return baseURL || window.location.origin;
}

/** Non-secret in-memory generation for existing UI/SSE ownership fences. */
export function getAuthToken(): string | null { return useAuthStore.getState().token; }
function clearAuthStorage(): void {
  for (const storage of [() => localStorage, () => sessionStorage]) {
    try { storage().removeItem(AUTH_STORAGE_KEY); } catch { /* Cookie auth requires no Web Storage. */ }
  }
}

// Keep the synchronous snapshot boundary: immediate A→B→A replacements must
// invalidate the original request before the transport adapter can dispatch.
api.interceptors.request.use(
  (config) => {
    const identity = useAuthStore.getState();
    const origin = window.location.origin;
    if (origin && new URL(config.url ?? "", new URL(config.baseURL || "/", origin)).origin !== origin) {
      throw new Error("Console BFF requests require the same origin");
    }
    const trace = browserTraceparent(config.url ?? "", config.baseURL);
    if (trace && config.headers && !config.headers.has("traceparent")) config.headers.set("traceparent", trace);

    // Never manufacture a bearer from browser state. Explicit API/SDK bearer
    // headers remain supported; the UI-only session marker is never sent.
    const authorization = String(config.headers.get("Authorization") ?? "");
    if (authorization.startsWith("Bearer console-session:")) {
      if (authorization !== `Bearer ${identity.token}`) throw new Error("Console session changed before request");
      config.headers.delete("Authorization");
    }
    const sdkBearer = String(config.headers.get("Authorization") ?? "").startsWith("Bearer ");
    config.withCredentials = !sdkBearer;
    if (sdkBearer) {
      requestAuth.delete(config);
      // Same-origin XHR always sends ambient cookies. Select Fetch for an
      // explicit SDK bearer so its credentials:"omit" can prevent mixed auth.
      if (Array.isArray(config.adapter)) config.adapter = "fetch";
      config.headers.delete("X-CSRF-Token");
    } else {
      requestAuth.set(config, { token: identity.token, authority: sdkAuthorityScope(identity.user), generation: authGeneration });
    }
    if (!["get", "head", "options"].includes((config.method ?? "get").toLowerCase()) && identity.isAuthenticated && !sdkBearer) {
      if (!identity.csrfToken) throw new Error("Console session CSRF bootstrap required");
      config.headers.set("X-CSRF-Token", identity.csrfToken);
    }
    return config;
  },
  (error) => { throw error; },
  { synchronous: true }
);

// Response interceptor to handle auth and rate limit errors
api.interceptors.response.use(
  (response) => response,
  (error: AxiosError) => {
    const requestIdentity = error.config && requestAuth.get(error.config);
    const currentIdentity = useAuthStore.getState();
    if (error.response?.status === 401 && requestIdentity
      && requestIdentity.generation === authGeneration
      && requestIdentity.token === getAuthToken()
      && requestIdentity.authority === sdkAuthorityScope(currentIdentity.user)) {
      // Only a 401 owned by the current auth generation clears BOTH storages. Old or
      // unowned responses still reject, but cannot clear a replacement login.
      clearAuthStorage();
      useAuthStore.getState().clearAuth();
      // Redirect to login if not already there
      if (!window.location.pathname.includes("/login")) {
        window.location.href = "/login";
      }
    }

    // Handle rate limit errors gracefully
    if (error.response?.status === 429) {
      const now = Date.now();
      // Only show toast if cooldown has passed (prevent toast spam)
      if (now - lastRateLimitToast > RATE_LIMIT_TOAST_COOLDOWN) {
        lastRateLimitToast = now;

        // Extract retry info from response
        const retryAfter = error.response.headers["retry-after"];
        const errorData = error.response.data as {
          error?: { retry_after?: number; dimension?: string };
        };
        const dimension = errorData?.error?.dimension || "unknown";
        const retrySeconds = retryAfter
          ? parseInt(retryAfter, 10)
          : errorData?.error?.retry_after || 60;

        toast.warning(
          i18n.t("errors.rateLimitTitle"),
          i18n.t("errors.rateLimitDesc", { seconds: retrySeconds, dimension })
        );
      }

      // Attach retry info to error for react-query retry logic
      (error as AxiosError & { retryAfter?: number }).retryAfter =
        parseInt(error.response.headers["retry-after"] || "5", 10) * 1000;
    }

    return Promise.reject(error);
  }
);
