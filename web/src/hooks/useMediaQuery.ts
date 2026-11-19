import { useCallback, useMemo, useSyncExternalStore } from "react";

/** Read the actual viewport on the first render, including lazy route mounts. */
export function useMediaQuery(query: string) {
  const media = useMemo(() => typeof window === "undefined" ? undefined : window.matchMedia(query), [query]);
  const subscribe = useCallback((notify: () => void) => {
    media?.addEventListener("change", notify);
    return () => media?.removeEventListener("change", notify);
  }, [media]);
  return useSyncExternalStore(subscribe, () => media?.matches ?? false, () => false);
}
