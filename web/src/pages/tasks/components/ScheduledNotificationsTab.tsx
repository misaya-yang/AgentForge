import { useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { Bell, RefreshCw } from "lucide-react";
import {
  getScheduledNotification, listScheduledNotifications,
  markScheduledNotification, markScheduledNotificationsRead,
  scheduledNotificationHref, SCHEDULED_NOTIFICATIONS_CHANGED,
  type NotificationCursor, type ScheduledNotification, type ScheduledNotificationPage,
} from "@/api/scheduledNotifications";
import { useAuthStore } from "@/store/useAuthStore";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { ScheduledExecutionResult } from "./ScheduledExecutionResult";

interface Loaded<T> {
  token: string | null;
  key: string;
  value: T | null;
  failed: boolean;
}
const validId = (id: string) => /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id);

export function ScheduledNotificationsTab() {
  const { t, i18n } = useTranslation();
  const token = useAuthStore((state) => state.token);
  const [searchParams, setSearchParams] = useSearchParams();
  const requested = searchParams.get("notification");
  const selectedId = requested && validId(requested) ? requested : null;
  const [filter, setFilter] = useState<"all" | "unread">("all");
  const [cursors, setCursors] = useState<Array<NotificationCursor | null>>([null]);
  const cursor = cursors[cursors.length - 1] ?? null;
  const pageKey = JSON.stringify([filter, cursor]);
  const [pageState, setPageState] = useState<Loaded<ScheduledNotificationPage> | null>(null);
  const [detailState, setDetailState] = useState<Loaded<ScheduledNotification> | null>(null);
  const [revision, setRevision] = useState(0);
  const [refreshing, setRefreshing] = useState(false);
  const [busy, setBusy] = useState(false);
  const mutationInFlight = useRef(false);
  const mounted = useRef(true);
  const [actionError, setActionError] = useState<{ token: string | null; message: string } | null>(null);
  const page = pageState?.token === token && pageState.key === pageKey ? pageState : null;
  const detail = detailState?.token === token && detailState.key === selectedId ? detailState : null;
  const rows = page?.value?.notifications ?? [];
  const unreadIds = rows.filter((row) => row.read_at === null).map((row) => row.notification_id);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  useEffect(() => { setCursors([null]); }, [token]);

  useEffect(() => {
    if (!token) return;
    let alive = true;
    let pending = false;
    const controller = new AbortController();
    const load = async () => {
      if (pending) return;
      pending = true;
      setRefreshing(true);
      try {
        const value = await listScheduledNotifications(filter, cursor, controller.signal);
        if (alive) setPageState({ token, key: pageKey, value, failed: false });
      } catch {
        // Hide stale data after a failed read. In particular, do not show a stale
        // cross-device unread count as if the latest refresh had succeeded.
        if (alive) setPageState({ token, key: pageKey, value: null, failed: true });
      } finally {
        pending = false;
        if (alive) setRefreshing(false);
      }
    };
    void load();
    const timer = window.setInterval(() => { void load(); }, 15000);
    return () => { alive = false; controller.abort(); window.clearInterval(timer); };
  }, [token, pageKey, cursor, filter, revision]);

  useEffect(() => {
    if (!selectedId || !token) return;
    let alive = true;
    const controller = new AbortController();
    setDetailState(null);
    void getScheduledNotification(selectedId, controller.signal).then((value) => {
      if (alive) setDetailState({ token, key: selectedId, value, failed: false });
    }).catch(() => {
      if (alive) setDetailState({ token, key: selectedId, value: null, failed: true });
    });
    return () => { alive = false; controller.abort(); };
  }, [selectedId, token, revision]);

  function select(id: string | null) {
    setSearchParams((current) => {
      const next = new URLSearchParams(current);
      next.set("tab", "notifications");
      next.delete("task");
      next.delete("execution");
      if (id) next.set("notification", id); else next.delete("notification");
      return next;
    });
  }
  async function acknowledge(action: () => Promise<void>) {
    if (mutationInFlight.current) return;
    mutationInFlight.current = true;
    setBusy(true);
    setActionError(null);
    try {
      await action();
      if (useAuthStore.getState().token === token) {
        window.dispatchEvent(new Event(SCHEDULED_NOTIFICATIONS_CHANGED));
        if (mounted.current) setRevision((value) => value + 1);
      }
    } catch {
      if (mounted.current && useAuthStore.getState().token === token) {
        setActionError({ token, message: t("tasks.notifications.actionError", "The read-state change was not confirmed. Refresh to check its state before retrying.") });
      }
    } finally {
      mutationInFlight.current = false;
      if (mounted.current) setBusy(false);
    }
  }
  function readButton(row: ScheduledNotification) {
    return <Button size="sm" variant="outline" disabled={busy} onClick={() => {
      void acknowledge(() => markScheduledNotification(row.notification_id, row.read_at === null));
    }}>{row.read_at === null
      ? t("tasks.notifications.markRead", "Mark read")
      : t("tasks.notifications.markUnread", "Mark unread")}</Button>;
  }

  return <section className="space-y-4" aria-label={t("tasks.notifications.title", "Scheduled task updates")}>
    <Card>
      <CardHeader className="pb-3"><CardTitle className="flex items-center gap-2 text-base"><Bell className="h-4 w-4" />{t("tasks.notifications.title", "Scheduled task updates")}</CardTitle></CardHeader>
      <CardContent className="space-y-3 text-sm text-muted-foreground">
        <p>{t("tasks.notifications.scope", "Completed, failed and cancelled attempts are saved here. Read state is saved to your account across devices. Opening an update does not mark it read.")}</p>
        <p>{t("tasks.notifications.delivery", "These are in-app execution receipts. No email, push or external message is sent. Unresolved launches remain in Scheduled until their outcome is known.")}</p>
      </CardContent>
    </Card>
    <div className="flex flex-wrap items-center justify-between gap-2">
      <div className="flex flex-wrap items-center gap-2" role="group" aria-label={t("tasks.notifications.filter", "Filter updates")}>
        <Button size="sm" variant={filter === "all" ? "default" : "outline"} aria-pressed={filter === "all"} onClick={() => { setFilter("all"); setCursors([null]); }}>{t("tasks.notifications.all", "All")}</Button>
        <Button size="sm" variant={filter === "unread" ? "default" : "outline"} aria-pressed={filter === "unread"} onClick={() => { setFilter("unread"); setCursors([null]); }}>{t("tasks.notifications.unread", "Unread")}</Button>
        {page?.value && <span className="text-sm text-muted-foreground">{t("tasks.notifications.unreadCount", { count: page.value.unread_count, defaultValue: "{{count}} unread" })}</span>}
      </div>
      <div className="flex flex-wrap gap-2">
        <Button size="sm" variant="outline" disabled={busy || unreadIds.length === 0} onClick={() => { void acknowledge(() => markScheduledNotificationsRead(unreadIds)); }}>{t("tasks.notifications.markPageRead", "Mark this page read")}</Button>
        <Button size="sm" variant="ghost" disabled={refreshing || busy} onClick={() => {
          setRevision((value) => value + 1);
          window.dispatchEvent(new Event(SCHEDULED_NOTIFICATIONS_CHANGED));
        }}><RefreshCw className={`mr-2 h-3.5 w-3.5 ${refreshing ? "animate-spin" : ""}`} />{t("tasks.notifications.refresh", "Refresh")}</Button>
      </div>
    </div>
    {actionError?.token === token && <p role="alert" className="text-sm text-destructive">{actionError.message}</p>}
    {requested && !selectedId && <p role="alert" className="text-sm text-destructive">{t("tasks.notifications.invalidLink", "This update link is invalid. Open an update from your inbox.")}</p>}
    {selectedId && <Card>
      <CardHeader className="flex-row items-center justify-between gap-3 space-y-0"><CardTitle className="break-words text-base">{detail?.value?.task.title ?? t("tasks.notifications.selected", "Selected update")}</CardTitle><Button size="sm" variant="ghost" onClick={() => select(null)}>{t("tasks.notifications.close", "Close")}</Button></CardHeader>
      <CardContent className="space-y-3">
        {!detail && <p role="status" className="text-sm">{t("tasks.notifications.loading", "Loading updates…")}</p>}
        {detail?.failed && <p role="alert" className="text-sm text-destructive">{t("tasks.notifications.detailUnavailable", "This update is unavailable to your account or could not be loaded. Refresh to try again.")}</p>}
        {detail?.value && <>
          <div className="flex flex-wrap items-center gap-3"><Badge variant={detail.value.read_at === null ? "default" : "outline"}>{detail.value.read_at === null ? t("tasks.notifications.unread", "Unread") : t("tasks.notifications.read", "Read")}</Badge>{readButton(detail.value)}</div>
          <ScheduledExecutionResult
            key={detail.value.notification_id}
            run={detail.value.execution} task={detail.value.task} initiallyOpen
            canonicalHref={scheduledNotificationHref(detail.value.notification_id)}
          />
        </>}
      </CardContent>
    </Card>}
    {!page && <p role="status" className="text-sm">{t("tasks.notifications.loading", "Loading updates…")}</p>}
    {page?.failed && <p role="alert" className="text-sm text-destructive">{t("tasks.notifications.loadError", "Updates could not be loaded. Check service availability and the scheduled-notification database upgrade, then refresh.")}</p>}
    {page?.value && rows.length === 0 && <p className="rounded-xl border border-dashed p-8 text-center text-sm text-muted-foreground">{filter === "unread" ? t("tasks.notifications.noUnread", "No unread updates on this page.") : t("tasks.notifications.empty", "No completed task outcomes on this page yet.")}</p>}
    <div className="space-y-3" aria-busy={refreshing}>
      {rows.map((row) => <article key={row.notification_id} className={`space-y-3 rounded-xl border bg-card p-4 ${row.read_at === null ? "border-primary/40" : "border-border/70"}`}>
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="min-w-0 space-y-1">
            <h3 className="break-words text-sm font-medium">{row.task.title}</h3>
            <p className="text-xs text-muted-foreground">{new Date(row.occurred_at).toLocaleString(i18n.language)}</p>
          </div>
          <div className="flex flex-wrap gap-2"><Badge variant="outline">{t(`tasks.notifications.status.${row.execution.status}`, row.execution.status)}</Badge><Badge variant={row.read_at === null ? "default" : "outline"}>{row.read_at === null ? t("tasks.notifications.unread", "Unread") : t("tasks.notifications.read", "Read")}</Badge></div>
        </div>
        {row.execution.error_code && <p className="break-words text-xs text-destructive">{row.execution.error_code}</p>}
        <p className="break-all font-mono text-xs text-muted-foreground">{row.notification_id}</p>
        <div className="flex flex-wrap gap-2"><Button size="sm" variant="outline" onClick={() => select(row.notification_id)}>{t("tasks.notifications.open", "Open update and output")}</Button>{readButton(row)}</div>
      </article>)}
    </div>
    <div className="flex flex-wrap items-center justify-between gap-3">
      <p className="text-xs text-muted-foreground">{t("tasks.notifications.pageNotice", "Up to 50 receipts per page. New outcomes appear on the latest page; read changes apply only to the selected receipts.")}</p>
      <div className="flex gap-2">
        {cursors.length > 1 && <Button size="sm" variant="outline" onClick={() => setCursors([null])}>{t("tasks.notifications.latest", "Latest")}</Button>}
        <Button size="sm" variant="outline" disabled={cursors.length <= 1 || busy} onClick={() => setCursors((value) => value.slice(0, -1))}>{t("tasks.notifications.newer", "Newer")}</Button>
        <Button size="sm" variant="outline" disabled={!page?.value?.next_cursor || busy} onClick={() => {
          const next = page?.value?.next_cursor;
          if (next) setCursors((value) => [...value, next]);
        }}>{t("tasks.notifications.older", "Older")}</Button>
      </div>
    </div>
  </section>;
}
