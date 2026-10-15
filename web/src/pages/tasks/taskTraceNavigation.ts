import { taskInboxLocation } from "./taskNavigation.ts";

// IDs are opaque: never trim, infer a session, or substitute a task ID.
export function exactTraceId(value: unknown): string | null {
  if (typeof value !== "string" || value.length === 0 || value.length > 512
    || /\s/u.test(value) || Array.from(value).some(character => character.charCodeAt(0) < 32 || character.charCodeAt(0) === 127)) return null;
  // URLSearchParams replaces invalid UTF-16; reject instead of changing identity.
  try { encodeURIComponent(value); return value; } catch { return null; }
}
export function taskTraceHref(requestId: unknown, taskId: unknown, params: URLSearchParams): string | null {
  const request = exactTraceId(requestId); const task = exactTraceId(taskId);
  if (!request || !task) return null;
  const { filter, offset } = taskInboxLocation(params);
  return `/dashboard?${new URLSearchParams({ tab: "tracing", request_id: request, task_id: task, task_status: filter, task_offset: String(offset) })}`;
}
export function taskTraceReturn(params: URLSearchParams): string | null {
  const task = exactTraceId(params.get("task_id")); if (!task) return null;
  const { filter, offset } = taskInboxLocation(params);
  return `/tasks?${new URLSearchParams({ tab: "query", task_id: task, task_status: filter, task_offset: String(offset) })}`;
}
