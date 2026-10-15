import { useCallback, useLayoutEffect, useMemo, useState } from "react";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { generateUUID } from "@/lib/utils";

/** Ephemeral identity for an authenticated read view, never a credential in a query key. */
export function useAuthenticatedReadScope() {
  const token = useAuthStore(state => state.token);
  const authority = useAuthStore(state => sdkAuthorityScope(state.user));
  const [epoch, setEpoch] = useState(0);
  const scope = useMemo(() => ({ id: generateUUID(), token, authority, epoch, invalidated: false, mounted: true }), [token, authority, epoch]);
  useLayoutEffect(() => {
    scope.mounted = true;
    const unsubscribe = useAuthStore.subscribe((state, previous) => {
      if (state.token !== previous.token || state.isAuthenticated !== previous.isAuthenticated || sdkAuthorityScope(state.user) !== sdkAuthorityScope(previous.user)) {
        scope.invalidated = true; setEpoch(value => value + 1);
      }
    });
    return () => { scope.mounted = false; unsubscribe(); };
  }, [scope]);
  const isCurrent = useCallback(() => {
    const state = useAuthStore.getState();
    return Boolean(scope.mounted && !scope.invalidated && state.isAuthenticated && state.token && state.user && state.token === scope.token && sdkAuthorityScope(state.user) === scope.authority);
  }, [scope]);
  return { id: scope.id, isCurrent };
}
