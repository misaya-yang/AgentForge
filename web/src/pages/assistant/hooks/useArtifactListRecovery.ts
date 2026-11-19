import { useCallback, useLayoutEffect, useMemo, useRef, useState } from "react";
import { getSessionArtifacts, type ArtifactInfo } from "@/api/assistant";
import { useAuthStore } from "@/store/useAuthStore";
import { useAppStore } from "@/store/useAppStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";

export function useArtifactListRecovery(options: {
  sessionId: string | null | undefined;
  open: boolean;
  viewEpoch: number;
  isViewCurrent: (epoch: number) => boolean;
  eligible: boolean;
  getVersion: () => string;
  apply: (items: ArtifactInfo[]) => void;
  setLoadError: (failed: boolean) => void;
}) {
  const { open, isViewCurrent } = options;
  const token = useAuthStore(state => state.token);
  const authority = useAuthStore(state => sdkAuthorityScope(state.user));
  const [transition, setTransition] = useState(0);
  const scope = useMemo(() => ({ token, authority, transition, sessionId: options.sessionId, viewEpoch: options.viewEpoch, invalidated: false, mounted: true }), [token, authority, transition, options.sessionId, options.viewEpoch]);
  const pending = useRef<{ controller: AbortController; scope: typeof scope } | null>(null);
  const [observation, setObservation] = useState<{ scope: typeof scope; status: "loading" | "error" | "unavailable" | "superseded" | "ready" } | null>(null);
  useLayoutEffect(() => {
    scope.mounted = true;
    const invalidate = () => { scope.invalidated = true; pending.current?.controller.abort(); pending.current = null; setTransition(value => value + 1); };
    const auth = useAuthStore.subscribe((state, previous) => {
      if (state.token !== previous.token || state.isAuthenticated !== previous.isAuthenticated || sdkAuthorityScope(state.user) !== sdkAuthorityScope(previous.user)) invalidate();
    });
    const app = useAppStore.subscribe((state, previous) => {
      if (state.assistantActiveSessionId !== previous.assistantActiveSessionId) invalidate();
    });
    return () => { scope.mounted = false; if (pending.current?.scope === scope) { pending.current.controller.abort(); pending.current = null; } auth(); app(); };
  }, [scope]);
  const isCurrent = useCallback(() => {
    const state = useAuthStore.getState();
    return Boolean(scope.mounted && !scope.invalidated && scope.sessionId && open && isViewCurrent(scope.viewEpoch)
      && useAppStore.getState().assistantActiveSessionId === scope.sessionId && state.isAuthenticated && state.token && state.user
      && state.token === scope.token && sdkAuthorityScope(state.user) === scope.authority);
  }, [scope, open, isViewCurrent]);
  const renderVersion = options.getVersion();
  const refresh = useCallback(async () => {
    if (!isCurrent() || !options.eligible || pending.current || options.getVersion() !== renderVersion) return;
    const request = { controller: new AbortController(), scope }; pending.current = request;
    setObservation({ scope, status: "loading" });
    try {
      const items = await getSessionArtifacts(scope.sessionId!, request.controller.signal);
      if (!isCurrent() || pending.current !== request) return;
      if (options.getVersion() !== renderVersion) { setObservation({ scope, status: "superseded" }); return; }
      options.apply(items); options.setLoadError(false); setObservation({ scope, status: "ready" });
    } catch (error) {
      if (!isCurrent() || pending.current !== request) return;
      if (options.getVersion() !== renderVersion) { setObservation({ scope, status: "superseded" }); return; }
      const status = (error as { response?: { status?: number } })?.response?.status;
      const unavailable = status === 403 || status === 404;
      if (unavailable) options.apply([]);
      options.setLoadError(true); setObservation({ scope, status: unavailable ? "unavailable" : "error" });
    } finally { if (pending.current === request) pending.current = null; }
  }, [isCurrent, options, renderVersion, scope]);
  return { refreshArtifacts: refresh, artifactRefreshStatus: observation?.scope === scope ? observation.status : "idle",
    canRefreshArtifacts: options.eligible && isCurrent() };
}
