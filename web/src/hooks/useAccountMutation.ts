import { useCallback, useRef, useState } from "react";
import { getErrorMessage } from "@/lib/utils";

/** Single-attempt account writes. An error requires an authoritative refresh. */
export function useAccountMutation(fallback: string) {
  const running = useRef(false);
  const blocked = useRef(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = useCallback(async (action: () => Promise<unknown>): Promise<boolean> => {
    if (running.current || blocked.current) return false;
    running.current = true;
    setPending(true);
    setError(null);
    try {
      await action();
      return true;
    } catch (failure) {
      blocked.current = true;
      setError(getErrorMessage(failure) || fallback);
      return false;
    } finally {
      running.current = false;
      setPending(false);
    }
  }, [fallback]);
  const refreshed = useCallback(() => {
    if (running.current) return;
    blocked.current = false;
    setError(null);
  }, []);
  return { run, pending, error, refreshed, blocked: error !== null };
}
