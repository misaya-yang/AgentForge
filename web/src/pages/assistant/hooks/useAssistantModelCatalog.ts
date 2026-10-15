import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { getConfig, listModels, type AssistantConfig, type ModelInfo } from "@/api/assistant";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { generateUUID } from "@/lib/utils";
import { activeAssistantModels } from "../modelSelection";

/** Read-only catalog refresh never updates a conversation, draft, or selected model. */
export function useAssistantModelCatalog() {
  const token = useAuthStore(state => state.token);
  const authority = useAuthStore(state => sdkAuthorityScope(state.user));
  const hydrated = useAuthStore(state => state.hydrated);
  const authenticated = useAuthStore(state => state.isAuthenticated);
  const [transition, setTransition] = useState(0);
  const scope = useMemo(() => ({ id: generateUUID(), token, authority, transition, invalidated: false }), [token, authority, transition]);
  const [revision, setRevision] = useState(0);
  const [observation, setObservation] = useState<{ scope: string; models: ModelInfo[]; config: AssistantConfig | null; status: "loading" | "error" | "ready" }>({ scope: "", models: [], config: null, status: "loading" });
  const lifetime = useRef({ mounted: false, request: 0, pending: false });
  useLayoutEffect(() => {
    const owner = lifetime.current; owner.mounted = true;
    const unsubscribe = useAuthStore.subscribe((state, previous) => {
      if (state.token !== previous.token || state.isAuthenticated !== previous.isAuthenticated
        || sdkAuthorityScope(state.user) !== sdkAuthorityScope(previous.user)) {
        scope.invalidated = true; owner.request += 1; owner.pending = false;
        setTransition(value => value + 1);
      }
    });
    return () => { owner.mounted = false; owner.request += 1; owner.pending = false; unsubscribe(); };
  }, [scope]);
  const isCurrent = useCallback(() => {
    const state = useAuthStore.getState();
    return Boolean(lifetime.current.mounted && !scope.invalidated && state.hydrated && state.isAuthenticated && state.token && state.user
      && state.token === scope.token && sdkAuthorityScope(state.user) === scope.authority);
  }, [scope]);
  useEffect(() => {
    if (!hydrated || !authenticated || !isCurrent()) return;
    const owner = lifetime.current; const request = ++owner.request; owner.pending = true;
    const controller = new AbortController();
    const current = () => isCurrent() && owner.request === request;
    setObservation(previous => ({ scope: scope.id, models: previous.scope === scope.id ? previous.models : [], config: previous.scope === scope.id ? previous.config : null, status: "loading" }));
    void Promise.all([listModels({ signal: controller.signal }), getConfig({ signal: controller.signal })]).then(([models, config]) => {
      if (current()) setObservation({ scope: scope.id, models: activeAssistantModels(models, config.available_providers), config, status: "ready" });
    }).catch(() => {
      if (current()) setObservation(previous => ({ ...previous, status: "error" }));
    }).finally(() => { if (current()) owner.pending = false; });
    return () => { owner.request += 1; owner.pending = false; controller.abort(); };
  }, [scope.id, hydrated, authenticated, revision, isCurrent]);
  const refresh = useCallback(() => {
    if (!isCurrent() || lifetime.current.pending) return;
    lifetime.current.pending = true; lifetime.current.request += 1;
    setObservation(previous => ({ ...previous, status: "loading" }));
    setRevision(value => value + 1);
  }, [isCurrent]);
  const owned = observation.scope === scope.id && !scope.invalidated && authenticated && hydrated;
  const observedRequest = lifetime.current.request;
  const isReady = () => isCurrent() && owned && observation.status === "ready"
    && !lifetime.current.pending && lifetime.current.request === observedRequest;
  return { models: owned ? observation.models : [], config: owned ? observation.config : null,
    status: owned ? observation.status : "loading" as const, refresh, isCurrent, isReady };
}
