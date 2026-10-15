/** One explicitly confirmed mutation, followed only by independent receipt reads. */
export interface CancellationState {
  phase: "idle" | "sending" | "accepted" | "unknown";
  reading: boolean;
  observed: string | null;
  readFailed: boolean;
}
export const initialCancellation: CancellationState = { phase: "idle", reading: false, observed: null, readFailed: false };
export const taskCanCancel = (status: string) => status === "pending" || status === "processing";

export function taskCancellation(options: {
  taskId: string;
  current: () => boolean;
  cancel: (id: string) => Promise<unknown>;
  read: (id: string) => Promise<{ task_id: string; status: string }>;
  changed: (state: CancellationState) => void;
  observed: () => void;
}) {
  let state = initialCancellation;
  let active = true;
  let attempted = false;
  let reading = false;
  const current = () => active && options.current();
  const publish = (patch: Partial<CancellationState>) => {
    if (!current()) return;
    state = { ...state, ...patch };
    options.changed(state);
  };
  async function refresh() {
    if (!current() || !attempted || reading || state.phase === "sending") return;
    reading = true;
    publish({ reading: true, observed: null, readFailed: false });
    try {
      const receipt = await options.read(options.taskId);
      if (!current()) return;
      if (receipt.task_id !== options.taskId || !["pending", "processing", "completed", "failed", "cancelled"].includes(receipt.status)) {
        throw new Error("Task receipt mismatch");
      }
      publish({ observed: receipt.status });
      options.observed();
    } catch {
      publish({ readFailed: true, observed: null });
    } finally {
      reading = false;
      publish({ reading: false });
    }
  }
  return {
    async confirm(status: string) {
      if (!current() || attempted || !taskCanCancel(status)) return;
      // Synchronous fence also blocks two clicks before React's next render.
      attempted = true;
      publish({ phase: "sending" });
      try {
        await options.cancel(options.taskId);
        publish({ phase: "accepted" });
      } catch {
        // Transport errors and 409/404 are not evidence of an execution outcome.
        publish({ phase: "unknown" });
      }
      if (current()) await refresh();
    },
    refresh,
    dispose() { active = false; },
  };
}
