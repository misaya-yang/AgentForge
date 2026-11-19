import { useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { getRequestTrace, type RequestTrace } from "@/api/usage";
import { exactTraceId, taskTraceReturn } from "../taskTraceNavigation";

// Mounted under the trace panel's authority + router-visit key. One read per
// visit, no retries, recent-list fallback, or task mutation.
export function TaskTraceLookup({ isCurrent, renderTrace }: {
  isCurrent: () => boolean; renderTrace: (trace: RequestTrace) => ReactNode;
}) {
  const [params] = useSearchParams(); const { t } = useTranslation();
  const requestId = exactTraceId(params.get("request_id"));
  const back = taskTraceReturn(params);
  const mounted = useRef(true);
  const [state, setState] = useState<{ trace: RequestTrace | null; pending: boolean }>({ trace: null, pending: Boolean(requestId) });
  useLayoutEffect(() => {
    mounted.current = true; let live = true; const controller = new AbortController();
    if (requestId && isCurrent()) {
      void getRequestTrace(requestId, controller.signal).then(trace => {
        if (live && isCurrent()) setState({ trace: trace?.request_id === requestId ? trace : null, pending: false });
      }, () => { if (live && isCurrent()) setState({ trace: null, pending: false }); });
    }
    return () => { mounted.current = false; live = false; controller.abort(); };
  }, [requestId, isCurrent]);
  if (!isCurrent()) return null;
  return <section className="space-y-3">
    {back && <Link className="text-sm text-primary underline" to={back}
      onClick={event => { if (!mounted.current || !isCurrent()) event.preventDefault(); }}>{t("tasks.trace.back", "Return to task")}</Link>}
    <p className="break-all text-xs">{t("dashboard.requestTrace.requestId")}: {requestId ?? "—"}</p>
    <p className="text-sm text-muted-foreground">{t("tasks.trace.notice", "This lookup uses the task's exact request ID. A trace may be unavailable or outside your access; this does not change the task status.")}</p>
    {state.pending ? <p role="status">{t("dashboard.requestTrace.loading")}</p> : state.trace ? renderTrace(state.trace) : <p role="status">{t("tasks.trace.unavailable", "Request trace unavailable")}</p>}
  </section>;
}
