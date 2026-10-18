import type { RequestBody } from "@/api/contracts";
import type { FetchClient } from "./fetchClient";
import { useAuthStore, consoleAuthGeneration } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { getPasskeyAssertion, type AssertionChallenge } from "./passkey";
import { authorizeConsoleMutation, sensitiveConsoleMutation, type StepUpClient, type StepUpStatus } from "./consoleStepUp";
const installed = new WeakMap<FetchClient, () => void>();

export function stepUpClient(api: FetchClient): StepUpClient {
  return {
    status: async signal => (await api.get<StepUpStatus>("/api/auth/session/step-up", { signal })).data,
    start: async signal => (await api.post<AssertionChallenge>("/api/auth/enterprise/passkeys/step-up/start", undefined, { signal })).data,
    assertion: getPasskeyAssertion,
    finish: async (flow_id, credential, signal) => (await api.post<{ verified: boolean }>("/api/auth/enterprise/passkeys/step-up/finish", ({ flow_id, credential } satisfies RequestBody<"POST /api/auth/enterprise/passkeys/step-up/finish">), { signal })).data,
  };
}
export function currentStepUpScope(): () => boolean {
  const generation = consoleAuthGeneration(), identity = useAuthStore.getState();
  const authority = sdkAuthorityScope(identity.user);
  return () => {
    const current = useAuthStore.getState();
    return identity.isAuthenticated && current.isAuthenticated && generation === consoleAuthGeneration()
      && identity.token === current.token && identity.csrfToken === current.csrfToken
      && authority === sdkAuthorityScope(current.user);
  };
}
/** All interceptors stay synchronous. Capture account ownership at call time;
 * delay only the adapter, after serialization and before network dispatch. */
export function installConsoleStepUp(api: FetchClient): () => void {
  const previous = installed.get(api);
  if (previous) return previous;
  const client = stepUpClient(api);
  const id = api.interceptors.request.use(config => {
    const path = new URL(config.url ?? "", new URL(config.baseURL || "/", window.location.origin)).pathname;
    if (!sensitiveConsoleMutation(config.method ?? "GET", path)
      || (/^Bearer /i.test(String(config.headers.get("Authorization") ?? ""))
        && !String(config.headers.get("Authorization")).startsWith("Bearer console-session:"))) return config;
    const scope = currentStepUpScope();
    const isCurrent = () => scope() && config.authorityCurrent?.() !== false;
    config.preflight = async () => {
      const controller = new AbortController();
      const abort = () => controller.abort();
      const unsubscribe = useAuthStore.subscribe(() => { if (!isCurrent()) abort(); });
      const cleanup = () => { unsubscribe(); config.signal?.removeEventListener?.("abort", abort); };
      config.signal?.addEventListener?.("abort", abort);
      try {
        if (config.signal?.aborted) abort();
        await authorizeConsoleMutation(client, isCurrent, controller.signal);
        if (!isCurrent() || controller.signal.aborted) throw new Error("Console session changed before sensitive request");
        return cleanup;
      } catch (error) { cleanup(); throw error; }
    };
    config.afterDispatch = () => { if (!isCurrent()) throw new Error("Console session changed after sensitive request"); };
    return config;
  }, error => { throw error; }, { synchronous: true });
  const uninstall = () => { api.interceptors.request.eject(id); installed.delete(api); };
  installed.set(api, uninstall);
  if (import.meta.hot) import.meta.hot.dispose(uninstall);
  return uninstall;
}
