import { api } from "@/lib/api";
import type { ScheduledExecution, ScheduledTask } from "./scheduledTasks";

export type ScheduledOutcomeStatus = "succeeded" | "failed" | "cancelled";
export interface ScheduledNotification {
  notification_id: string;
  occurred_at: string;
  read_at: string | null;
  task: Pick<ScheduledTask, "task_id" | "title" | "status">;
  execution: ScheduledExecution & { status: ScheduledOutcomeStatus };
}
export interface NotificationCursor {
  before_at: string;
  before_id: string;
}
export interface ScheduledNotificationPage {
  notifications: ScheduledNotification[];
  unread_count: number;
  next_cursor: NotificationCursor | null;
}
export interface ScheduledNotificationSummary {
  unread_count: number;
  total_count: number;
}

const base = "/api/v1/scheduled-task-notifications";
export const SCHEDULED_NOTIFICATIONS_CHANGED = "scheduled-notifications-changed";

export async function listScheduledNotifications(
  filter: "all" | "unread", cursor: NotificationCursor | null, signal?: AbortSignal,
): Promise<ScheduledNotificationPage> {
  return (await api.get<ScheduledNotificationPage>(base, {
    params: { filter, limit: 50, ...cursor }, signal,
  })).data;
}
export async function getScheduledNotification(id: string, signal?: AbortSignal): Promise<ScheduledNotification> {
  return (await api.get<{ notification: ScheduledNotification }>(`${base}/${encodeURIComponent(id)}`, { signal })).data.notification;
}
export async function getScheduledNotificationSummary(signal?: AbortSignal): Promise<ScheduledNotificationSummary> {
  return (await api.get<ScheduledNotificationSummary>(`${base}/summary`, { signal })).data;
}
export async function markScheduledNotification(id: string, read: boolean): Promise<void> {
  await api.post(`${base}/${encodeURIComponent(id)}/${read ? "read" : "unread"}`, {});
}
/** Explicitly acknowledges only the supplied visible receipts, never unseen arrivals. */
export async function markScheduledNotificationsRead(ids: string[]): Promise<void> {
  await api.post(`${base}/read`, { notification_ids: ids });
}
export function scheduledNotificationHref(id: string): string {
  return `/tasks?${new URLSearchParams({ tab: "notifications", notification: id })}`;
}
