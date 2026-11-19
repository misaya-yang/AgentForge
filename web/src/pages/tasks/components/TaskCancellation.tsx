import { useEffect, useId, useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { cancelTask, getTask } from "@/api/gateway";
import { Button } from "@/components/ui/button";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { generateUUID } from "@/lib/utils";
import { useAuthStore } from "@/store/useAuthStore";
import { initialCancellation, taskCancellation, taskCanCancel } from "../taskCancellation";

type Props = { taskId: string; status: string | undefined };

export function TaskCancellation(props: Props) {
  const token = useAuthStore(state => state.token);
  const scope = useAuthStore(state => sdkAuthorityScope(state.user));
  const identity = useMemo(() => ({ id: generateUUID(), token, scope }), [token, scope]);
  if (!token) return null;
  return <CancellationControl key={`${identity.id}:${props.taskId}`} {...props} token={token} scope={scope} />;
}

function CancellationControl({ taskId, status, token, scope }: Props & { token: string; scope: string }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [confirming, setConfirming] = useState(false);
  const [state, setState] = useState(initialCancellation);
  const confirmationId = useId();
  const requestButton = useRef<HTMLButtonElement>(null);
  const section = useRef<HTMLElement>(null);
  const controller = useRef<ReturnType<typeof taskCancellation> | null>(null);
  useEffect(() => {
    const userId = useAuthStore.getState().user?.user_id;
    const operation = taskCancellation({
      taskId,
      current: () => {
        const auth = useAuthStore.getState();
        return auth.token === token && sdkAuthorityScope(auth.user) === scope;
      },
      cancel: cancelTask,
      read: getTask,
      changed: setState,
      observed: () => {
        // Re-read cached views; never synthesize an execution terminal state.
        void queryClient.invalidateQueries({ queryKey: ["task", userId, taskId] });
        void queryClient.invalidateQueries({ queryKey: ["tasks", userId] });
      },
    });
    controller.current = operation;
    return () => { operation.dispose(); controller.current = null; };
  }, [taskId, token, scope, queryClient]);
  const eligible = status !== undefined && taskCanCancel(status);
  if (!eligible && state.phase === "idle") return null;
  return <section ref={section} tabIndex={-1} className="space-y-3 rounded-xl border border-border bg-card p-4" aria-label={t("tasks.cancellation.title")}>
    <h3 className="text-sm font-semibold">{t("tasks.cancellation.title")}</h3>
    <p className="text-xs text-muted-foreground">{t("tasks.cancellation.scope")}</p>
    {state.phase === "idle" && eligible && <div className="space-y-3">
      <Button ref={requestButton} variant="outline" size="sm" aria-expanded={confirming}
        aria-controls={confirmationId} aria-describedby={confirming ? `${confirmationId}-warning` : undefined}
        onClick={() => setConfirming(true)}>{t("tasks.cancellation.request")}</Button>
      {confirming && <div id={confirmationId} role="group" aria-describedby={`${confirmationId}-warning`} className="space-y-3">
        <p id={`${confirmationId}-warning`} role="alert">{t("tasks.cancellation.warning", { id: taskId })}</p>
        <div className="flex gap-2">
          <Button variant="destructive" size="sm" onClick={() => {
            if (status !== undefined) void controller.current?.confirm(status);
            section.current?.focus();
          }}>{t("tasks.cancellation.confirm")}</Button>
          <Button variant="outline" size="sm" onClick={() => { setConfirming(false); requestButton.current?.focus(); }}>{t("tasks.cancellation.keep")}</Button>
        </div>
      </div>}
    </div>}
    {state.phase !== "idle" && <p role="status">{t(`tasks.cancellation.${state.phase}`)}</p>}
    {state.reading && <p role="status">{t("tasks.cancellation.reading")}</p>}
    {state.readFailed && <p role="alert">{t("tasks.cancellation.readFailed")}</p>}
    {state.observed && <p role="status">{t("tasks.cancellation.observed", { status: t(`tasks.status.${state.observed}`) })}</p>}
    {["accepted", "unknown"].includes(state.phase) && <Button variant="outline" size="sm" disabled={state.reading} onClick={() => { void controller.current?.refresh(); }}>{t("tasks.cancellation.refresh")}</Button>}
  </section>;
}
