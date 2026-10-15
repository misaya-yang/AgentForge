import { sdkCancellationRecorded, sdkRunIsActive, sdkRunView, type SdkRunReference, type SdkRunView } from "./sdkRun.ts";

export interface SdkRunControlState {
  run: SdkRunView | null;
  pending: "read" | "cancel" | null;
  error: "unavailable" | null;
  cancellation: "none" | "recorded" | "finished" | "unknown";
}
export const emptySdkRunControlState: SdkRunControlState = {
  run: null, pending: null, error: null, cancellation: "none",
};
interface Transport {
  read: (id: string, signal: AbortSignal) => Promise<unknown>;
  cancel: (id: string, signal: AbortSignal) => Promise<unknown>;
  isCurrent: () => boolean;
}

/** One owner/view-scoped controller. Reads reconcile; uncertain POSTs are never retried. */
export function createSdkRunController(reference: SdkRunReference, transport: Transport) {
  let state = { ...emptySdkRunControlState };
  let active = true;
  let request: AbortController | null = null;
  let listener: ((value: SdkRunControlState) => void) | undefined;
  const current = () => active && transport.isCurrent();
  const publish = (patch: Partial<SdkRunControlState>) => {
    if (!current()) return;
    state = { ...state, ...patch };
    listener?.(state);
  };
  const read = async (signal: AbortSignal) => {
    const response = await transport.read(reference.runId, signal);
    if (!current() || signal.aborted) return null;
    const run = sdkRunView(response, reference);
    publish({ run, error: null, ...(state.cancellation === "unknown" && !sdkRunIsActive(run.status) && run.status !== "unknown"
      ? { cancellation: run.status === "cancelled" ? "recorded" : "finished" } : {}) });
    return run;
  };
  return {
    subscribe(next: (value: SdkRunControlState) => void) { listener = next; next(state); },
    async refresh() {
      if (!current() || request) return;
      const own = new AbortController(); request = own;
      publish({ pending: "read" });
      try { await read(own.signal); }
      catch { publish({ run: null, error: "unavailable" }); }
      finally { if (request === own) request = null; publish({ pending: null }); }
    },
    async cancel() {
      if (!current() || request || state.cancellation !== "none" || !state.run || !sdkRunIsActive(state.run.status)) return;
      const own = new AbortController(); request = own;
      publish({ pending: "cancel" });
      let sent = false;
      try {
        // Refresh the exact run before mutation, including expiry and navigation/account fences.
        const latest = await read(own.signal);
        if (!latest || !current() || !sdkRunIsActive(latest.status)) return;
        sent = true;
        publish({ cancellation: "unknown" });
        const response = await transport.cancel(reference.runId, own.signal);
        if (!current() || own.signal.aborted) return;
        publish({ cancellation: sdkCancellationRecorded(response, reference) ? "recorded" : "finished" });
      } catch {
        publish({ run: null, error: "unavailable" });
      } finally {
        // Reconcile even a lost response using GET only. Do not replay the mutation.
        if (sent && current() && !own.signal.aborted) {
          try { await read(own.signal); }
          catch { publish({ run: null, error: "unavailable" }); }
        }
        if (request === own) request = null;
        publish({ pending: null });
      }
    },
    dispose() { active = false; request?.abort(); request = null; listener = undefined; },
  };
}
