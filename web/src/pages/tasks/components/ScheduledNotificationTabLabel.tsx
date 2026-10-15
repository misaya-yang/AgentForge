import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { getScheduledNotificationSummary, SCHEDULED_NOTIFICATIONS_CHANGED } from "@/api/scheduledNotifications";
import { useAuthStore } from "@/store/useAuthStore";

/** A count refresh is read-only. Navigation or polling never marks an outcome read. */
export function ScheduledNotificationTabLabel() {
  const { t } = useTranslation();
  const token = useAuthStore((state) => state.token);
  const [state, setState] = useState<{ token: string | null; unread: number | null; failed: boolean } | null>(null);
  useEffect(() => {
    if (!token) return;
    let alive = true;
    let pending = false;
    const controller = new AbortController();
    const refresh = async () => {
      if (pending) return;
      pending = true;
      try {
        const summary = await getScheduledNotificationSummary(controller.signal);
        if (alive) setState({ token, unread: summary.unread_count, failed: false });
      } catch {
        if (alive) setState({ token, unread: null, failed: true });
      } finally { pending = false; }
    };
    void refresh();
    const listener = () => { void refresh(); };
    const timer = window.setInterval(listener, 30000);
    window.addEventListener(SCHEDULED_NOTIFICATIONS_CHANGED, listener);
    return () => {
      alive = false;
      controller.abort();
      window.clearInterval(timer);
      window.removeEventListener(SCHEDULED_NOTIFICATIONS_CHANGED, listener);
    };
  }, [token]);
  const current = state?.token === token ? state : null;
  return <span className="inline-flex items-center gap-1.5">
    {t("tasks.notifications.tab", "Updates")}
    {current?.unread != null && current.unread > 0 && <span
      className="rounded-full bg-primary px-1.5 text-xs text-primary-foreground"
      aria-label={t("tasks.notifications.unreadCount", { count: current.unread, defaultValue: "{{count}} unread" })}
    >{current.unread > 99 ? "99+" : current.unread}</span>}
    {current?.failed && <span aria-label={t("tasks.notifications.countUnavailable", "Unread count unavailable")}>?</span>}
  </span>;
}
