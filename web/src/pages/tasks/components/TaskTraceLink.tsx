import { useLayoutEffect, useRef } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { useAuthStore } from "@/store/useAuthStore";
import { taskTraceHref } from "../taskTraceNavigation";

export function TaskTraceLink({ requestId, taskId, isCurrent }: {
  requestId: unknown; taskId: string; isCurrent: () => boolean;
}) {
  const [params] = useSearchParams(); const { t } = useTranslation();
  const hasPermission = useAuthStore(state => state.hasPermission);
  const live = useRef(true);
  useLayoutEffect(() => { live.current = true; return () => { live.current = false; }; }, []);
  const href = taskTraceHref(requestId, taskId, params);
  return href && isCurrent() && hasPermission("console:dashboard:view") ? <Link className="text-sm text-primary underline" to={href}
    onClick={event => { if (!live.current || !isCurrent() || !hasPermission("console:dashboard:view")) event.preventDefault(); }}>
    {t("tasks.trace.open", "View request trace")}</Link> : null;
}
