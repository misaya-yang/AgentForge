import { isScheduleId, type CreateScheduledTask, type ScheduledTask } from "@/api/scheduledTasks";

export interface ScheduleDraft {
  title: string; message: string; model: string; runAt: string; interval: string; retries: string;
}
export const emptyScheduleDraft: ScheduleDraft = { title: "", message: "", model: "", runAt: "", interval: "", retries: "0" };
export function scheduleInput(draft: ScheduleDraft, now: number): CreateScheduledTask | null {
  const date = new Date(draft.runAt);
  const interval = draft.interval === "" ? undefined : Number(draft.interval);
  const retries = Number(draft.retries);
  if (!draft.title.trim() || [...draft.title].length > 120 || !draft.message.trim() || [...draft.message].length > 200000
    || !draft.model.trim() || new TextEncoder().encode(draft.model).length > 255
    || !Number.isFinite(date.getTime()) || date.getTime() <= now
    || (interval !== undefined && (!Number.isSafeInteger(interval) || interval < 1 || interval > 525600))
    || draft.retries.trim() === "" || !Number.isSafeInteger(retries) || retries < 0 || retries > 3) return null;
  return { title: draft.title, message: draft.message, model_id: draft.model, run_at: date.toISOString(),
    ...(interval === undefined ? {} : { interval_seconds: interval * 60 }), max_retries: retries };
}
function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
/** Validate only public fields; do not retain unknown metadata from the server. */
export function scheduleReceipt(value: unknown): ScheduledTask | null {
  if (!object(value) || typeof value.task_id !== "string" || !isScheduleId(value.task_id)
    || typeof value.title !== "string" || typeof value.message !== "string" || typeof value.model_id !== "string"
    || typeof value.status !== "string" || !["active", "paused", "completed", "cancelled"].includes(value.status)
    || typeof value.next_run_at !== "string" || !Number.isFinite(Date.parse(value.next_run_at))
    || !(value.interval_seconds === null || (Number.isSafeInteger(value.interval_seconds) && Number(value.interval_seconds) >= 60 && Number(value.interval_seconds) <= 31536000))
    || !Number.isSafeInteger(value.max_retries) || Number(value.max_retries) < 0 || Number(value.max_retries) > 3
    || !Number.isSafeInteger(value.retry_count) || Number(value.retry_count) < 0) return null;
  return { task_id: value.task_id.toLowerCase(), title: value.title, message: value.message, model_id: value.model_id,
    status: value.status as ScheduledTask["status"], next_run_at: value.next_run_at,
    interval_seconds: value.interval_seconds as number | null, max_retries: Number(value.max_retries), retry_count: Number(value.retry_count) };
}
export interface ScheduleCreationState {
  phase: "idle" | "submitting" | "accepted" | "uncertain";
  submitted: Readonly<CreateScheduledTask> | null;
  taskId: string | null;
  reading: boolean;
  receipt: ScheduledTask | null;
  readFailed: boolean;
}
export const initialScheduleCreation: ScheduleCreationState = {
  phase: "idle", submitted: null, taskId: null, reading: false, receipt: null, readFailed: false,
};

export function scheduleCreation(options: {
  current: () => boolean;
  create: (input: CreateScheduledTask) => Promise<unknown>;
  read: (id: string) => Promise<unknown>;
  changed: (state: ScheduleCreationState) => void;
}) {
  let state = initialScheduleCreation;
  let active = true, attempted = false, reading = false;
  const current = () => active && options.current();
  const publish = (patch: Partial<ScheduleCreationState>) => {
    if (!current()) return;
    state = { ...state, ...patch }; options.changed(state);
  };
  async function refresh() {
    if (!current() || !state.taskId || reading) return;
    reading = true;
    const id = state.taskId;
    publish({ reading: true, readFailed: false, receipt: null });
    try {
      const row = scheduleReceipt(await options.read(id));
      if (!current()) return;
      if (!row || row.task_id !== id) throw new Error("Unconfirmed schedule readback");
      publish({ receipt: row });
    } catch { publish({ readFailed: true, receipt: null }); }
    finally { reading = false; publish({ reading: false }); }
  }
  return {
    isSubmitted: () => attempted,
    async submit(input: CreateScheduledTask) {
      if (!current() || attempted) return;
      attempted = true;
      const submitted = Object.freeze({ ...input });
      publish({ phase: "submitting", submitted });
      try {
        const row = scheduleReceipt(await options.create(submitted));
        if (!current()) return;
        // The POST response is the only creation identity source. Similar list
        // titles/times cannot prove which request created a schedule.
        if (!row || row.status !== "active" || row.title !== submitted.title.trim()
          || row.message !== submitted.message || row.model_id !== submitted.model_id
          || Date.parse(row.next_run_at) !== Date.parse(submitted.run_at)
          || row.interval_seconds !== (submitted.interval_seconds ?? null)
          || row.max_retries !== submitted.max_retries || row.retry_count !== 0) throw new Error("Unconfirmed schedule acknowledgement");
        publish({ phase: "accepted", taskId: row.task_id });
        await refresh();
      } catch { publish({ phase: "uncertain" }); }
    },
    refresh,
    dispose() { active = false; },
  };
}
