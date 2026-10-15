import { useCallback, useRef, useState } from "react";
import { cancelTask } from "@/api/assistant";
import { useAppStore } from "@/store/useAppStore";

type RestoredRun = { sessionId: string; runId: string };
type Owner = { run: RestoredRun; version: string };

/** Stop only the restored run observed under the current view and authority. */
export function useRestoredRunStop(
  getVersion: () => string,
  setError: (error: string | null) => void,
) {
  const [observedRun, setRun] = useState<RestoredRun | null>(null);
  const [restoredStopPending, setPending] = useState(false);
  const ownerRef = useRef<Owner | null>(null);
  const requestRef = useRef<{ owner: Owner } | null>(null);
  const setRestoredStopPending = useCallback((pending: boolean) => {
    if (!pending) requestRef.current = null;
    setPending(pending);
  }, []);
  const setObservedRun = useCallback((run: RestoredRun | null) => {
    ownerRef.current = run ? { run, version: getVersion() } : null;
    requestRef.current = null;
    setPending(false);
    setRun(run);
  }, [getVersion]);
  const owner = ownerRef.current;
  const stopRestoredRun = useCallback(() => {
    const current = () => owner !== null && ownerRef.current === owner &&
      owner.version === getVersion() &&
      useAppStore.getState().assistantActiveSessionId === owner.run.sessionId;
    if (!owner || !current() || requestRef.current) return;
    const request = { owner };
    requestRef.current = request;
    setPending(true);
    const report = (message: string) => {
      if (!current() || requestRef.current !== request) return;
      requestRef.current = null;
      setError(message);
      setPending(false);
    };
    void cancelTask(owner.run.runId, "user_requested_stop")
      .then((result) => {
        if (!result.cancelled) report("Stop was not accepted. Rechecking the run before another action.");
      })
      .catch(() => report("Could not request Stop. Reopen this conversation and try again."));
  }, [owner, getVersion, setError]);
  return { observedRun, setObservedRun, restoredStopPending, setRestoredStopPending, stopRestoredRun };
}
