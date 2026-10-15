// URL state belongs to the read-only generic-task inbox, not scheduled execution.
export const TASK_PAGE_SIZE = 50;
export const TASK_FILTERS = ["all", "pending", "processing", "completed", "failed", "cancelled"] as const;
export type TaskFilter = (typeof TASK_FILTERS)[number];

export function taskInboxLocation(params: URLSearchParams) {
  const requestedFilter = params.get("task_status");
  const filter = TASK_FILTERS.find((value) => value === requestedFilter) ?? "all";
  const rawOffset = params.get("task_offset") ?? "0";
  const parsedOffset = /^\d+$/.test(rawOffset) ? Number(rawOffset) : 0;
  const offset = Number.isSafeInteger(parsedOffset) && parsedOffset <= Number.MAX_SAFE_INTEGER - TASK_PAGE_SIZE
    ? Math.floor(parsedOffset / TASK_PAGE_SIZE) * TASK_PAGE_SIZE : 0;
  return { filter, offset };
}

export function taskInboxParams(params: URLSearchParams, filter?: TaskFilter, offset?: number) {
  const current = taskInboxLocation(params);
  const next = new URLSearchParams(params);
  next.set("tab", "inbox");
  next.delete("task_id");
  next.set("task_status", filter ?? current.filter);
  next.set("task_offset", String(offset ?? (filter === undefined ? current.offset : 0)));
  return next;
}

export function taskDetailParams(params: URLSearchParams, taskId: string) {
  const next = new URLSearchParams(params);
  next.set("tab", "query");
  next.set("task_id", taskId.trim());
  return next;
}

export function taskPageWindow<T>(rows: readonly T[], offset: number) {
  const visible = rows.slice(0, TASK_PAGE_SIZE);
  return {
    rows: visible,
    hasPrevious: offset > 0,
    hasNext: rows.length > TASK_PAGE_SIZE,
    start: visible.length ? offset + 1 : 0,
    end: visible.length ? offset + visible.length : 0,
  };
}
