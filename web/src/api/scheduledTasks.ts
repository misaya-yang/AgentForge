import type { RequestBody, RequestQuery } from "@/api/contracts";
import type { ApiRequest } from "../../../sdk/typescript/generated";
import { api } from "@/lib/api";
export interface ScheduledTask {
  task_id: string;
  title: string;
  message: string;
  model_id: string;
  status: "active" | "paused" | "completed" | "cancelled";
  next_run_at: string;
  interval_seconds: number | null;
  max_retries: number;
  retry_count: number;
}
export interface ScheduledExecution {
  execution_id: string;
  session_id: string;
  run_id: string | null;
  status: string;
  attempt: number;
  error_code: string | null;
  cancel_requested: boolean;
  created_at: string;
  finished_at: string | null;
}
export type CreateScheduledTask = ApiRequest<"POST /api/v1/scheduled-tasks">["body"];
const base = "/api/v1/scheduled-tasks";
export async function listScheduledTasks(): Promise<ScheduledTask[]> {
  return (await api.get<{ tasks: ScheduledTask[] }>(base)).data.tasks;
}
export async function createScheduledTask(input: CreateScheduledTask): Promise<ScheduledTask> {
  return (await api.post<{ task: ScheduledTask }>(base, (input satisfies RequestBody<"POST /api/v1/scheduled-tasks">))).data.task;
}
export type ScheduleAction = "pause" | "resume" | "cancel";
export async function changeScheduledTask(id: string, action: ScheduleAction): Promise<void> {
  const receipt = (await api.post<{ task_id?: unknown; status?: unknown; cancellation_requested?: unknown }>(`${base}/${encodeURIComponent(id)}/${action}`, ({} satisfies RequestBody<"POST /api/v1/scheduled-tasks/{task_id}/pause" | "POST /api/v1/scheduled-tasks/{task_id}/resume" | "POST /api/v1/scheduled-tasks/{task_id}/cancel">))).data;
  const expected = action === "resume" ? "active" : action === "pause" ? "paused" : "cancelled";
  if (!receipt || typeof receipt.task_id !== "string" || receipt.task_id.toLowerCase() !== id.toLowerCase()
    || receipt.status !== expected || receipt.cancellation_requested !== (action === "cancel")) throw new Error("Unconfirmed schedule action");
}
export async function listScheduledExecutions(id: string): Promise<ScheduledExecution[]> {
  return (await api.get<{ executions: ScheduledExecution[] }>(`${base}/${encodeURIComponent(id)}/executions`)).data.executions;
}

export interface ScheduleCursor { before_at: string; before_id: string }
export interface SchedulePage<T> { rows: T[]; next: ScheduleCursor | null; paginationAvailable: boolean }
function page<T>(data: Record<string, unknown>, field: string): SchedulePage<T> {
  if (!Array.isArray(data[field])) throw new Error("Invalid schedule page");
  const cursor = data.next_cursor;
  if (cursor != null && (typeof cursor !== "object" || !("before_at" in cursor) || !("before_id" in cursor)
      || typeof cursor.before_at !== "string" || typeof cursor.before_id !== "string"
      || !Number.isFinite(Date.parse(cursor.before_at)) || !isScheduleId(cursor.before_id))) throw new Error("Invalid schedule cursor");
  return { rows: data[field] as T[], next: cursor == null ? null : cursor as ScheduleCursor,
    paginationAvailable: Object.hasOwn(data, "next_cursor") };
}
export function isScheduleId(value: string): boolean {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value);
}
export async function readSchedulePage(cursor: ScheduleCursor | null, signal?: AbortSignal): Promise<SchedulePage<ScheduledTask>> {
  return page((await api.get<Record<string, unknown>>(base, { params: ({ limit: 20, ...cursor } satisfies RequestQuery<"GET /api/v1/scheduled-tasks">), signal })).data, "tasks");
}
export async function readExecutionPage(id: string, cursor: ScheduleCursor | null, signal?: AbortSignal): Promise<SchedulePage<ScheduledExecution>> {
  return page((await api.get<Record<string, unknown>>(`${base}/${encodeURIComponent(id)}/executions`, { params: ({ limit: 20, ...cursor } satisfies RequestQuery<"GET /api/v1/scheduled-tasks/{task_id}/executions">), signal })).data, "executions");
}
export async function readSchedule(id: string, signal?: AbortSignal): Promise<ScheduledTask> {
  const task = (await api.get<{ task: ScheduledTask }>(`${base}/${encodeURIComponent(id)}`, { signal })).data.task as ScheduledTask;
  if (!task || typeof task.task_id !== "string" || task.task_id.toLowerCase() !== id.toLowerCase() || !["active", "paused", "completed", "cancelled"].includes(task.status)) throw new Error("Mismatched schedule");
  return task;
}
export async function readScheduledExecution(id: string, execution: string, signal?: AbortSignal): Promise<ScheduledExecution> {
  const row = (await api.get<{ execution: ScheduledExecution & { task_id: string } }>(`${base}/${encodeURIComponent(id)}/executions/${encodeURIComponent(execution)}`, { signal })).data.execution as ScheduledExecution & { task_id: string };
  if (!row || row.execution_id.toLowerCase() !== execution.toLowerCase() || row.task_id.toLowerCase() !== id.toLowerCase()) throw new Error("Mismatched execution");
  return row;
}
