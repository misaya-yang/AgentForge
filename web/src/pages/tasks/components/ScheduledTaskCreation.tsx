import { useEffect, useRef, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { createScheduledTask, readSchedule } from "@/api/scheduledTasks";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { useAuthStore } from "@/store/useAuthStore";
import { emptyScheduleDraft, initialScheduleCreation, scheduleCreation, scheduleInput, type ScheduleDraft } from "../scheduledCreation";

export function ScheduledTaskCreation({ token, scope, onReview, onNew }: {
  token: string; scope: string; onReview: () => void; onNew: () => void;
}) {
  const { t } = useTranslation();
  const [params] = useSearchParams();
  const [draft, setDraft] = useState(emptyScheduleDraft);
  const [state, setState] = useState(initialScheduleCreation);
  const [invalid, setInvalid] = useState(false);
  const controller = useRef<ReturnType<typeof scheduleCreation> | null>(null);
  const current = () => {
    const auth = useAuthStore.getState();
    return auth.token === token && sdkAuthorityScope(auth.user) === scope;
  };
  useEffect(() => {
    const operation = scheduleCreation({
      current: () => { const auth = useAuthStore.getState(); return auth.token === token && sdkAuthorityScope(auth.user) === scope; },
      create: createScheduledTask, read: readSchedule, changed: setState,
    });
    controller.current = operation;
    return () => { operation.dispose(); controller.current = null; };
  }, [token, scope]);
  const update = (key: keyof ScheduleDraft, value: string) => {
    if (controller.current?.isSubmitted() || !current()) return;
    setDraft(previous => ({ ...previous, [key]: value })); setInvalid(false);
  };
  const locked = state.phase !== "idle";
  const detail = new URLSearchParams(params);
  if (state.taskId) detail.set("task", state.taskId);
  detail.set("tab", "scheduled");
  for (const field of ["execution", "execution_before_at", "execution_before_id"]) detail.delete(field);
  return <Card>
    <CardHeader><CardTitle>{t("tasks.scheduled.create")}</CardTitle></CardHeader>
    <CardContent className="space-y-4">
      <p className="text-sm text-muted-foreground">{t("tasks.scheduleCreation.scope")}</p>
      <form onSubmit={event => {
        event.preventDefault();
        if (!current() || controller.current?.isSubmitted()) return;
        const input = scheduleInput(draft, Date.now());
        if (!input) { setInvalid(true); return; }
        setInvalid(false); void controller.current?.submit(input);
      }}>
        <fieldset disabled={locked} className="grid gap-3">
          <legend className="sr-only">{t("tasks.scheduled.create")}</legend>
          <label className="grid gap-1 text-sm">{t("tasks.scheduled.title")}<Input name="title" required maxLength={120} value={draft.title} onChange={e => update("title", e.target.value)} /></label>
          <label className="grid gap-1 text-sm">{t("tasks.scheduled.prompt")}<Textarea name="message" required maxLength={200000} value={draft.message} onChange={e => update("message", e.target.value)} /></label>
          <label className="grid gap-1 text-sm">{t("tasks.scheduled.model")}<Input name="model" required maxLength={255} value={draft.model} onChange={e => update("model", e.target.value)} /></label>
          <div className="grid gap-3 sm:grid-cols-3">
            <label className="grid gap-1 text-sm">{t("tasks.scheduled.start")}<Input name="runAt" required type="datetime-local" value={draft.runAt} onChange={e => update("runAt", e.target.value)} /></label>
            <label className="grid gap-1 text-sm">{t("tasks.scheduled.interval")}<Input name="interval" type="number" min={1} max={525600} step={1} value={draft.interval} onChange={e => update("interval", e.target.value)} /></label>
            <label className="grid gap-1 text-sm">{t("tasks.scheduled.retries")}<Input name="retries" type="number" required min={0} max={3} step={1} value={draft.retries} onChange={e => update("retries", e.target.value)} /></label>
          </div>
          <p className="text-sm text-muted-foreground">{t("tasks.scheduled.policy")}</p>
          <Button disabled={locked} type="submit" className="justify-self-start">{t("tasks.scheduled.save")}</Button>
        </fieldset>
      </form>
      {invalid && <p role="alert">{t("tasks.scheduleCreation.invalid")}</p>}
      {locked && <div className="space-y-3" aria-live="polite">
        <p role={state.phase === "uncertain" ? "alert" : "status"}>{t(`tasks.scheduleCreation.${state.phase}`)}</p>
        <p className="text-sm">{t("tasks.scheduleCreation.retained", { at: state.submitted?.run_at })}</p>
        {state.taskId && <>
          <p className="break-all font-mono text-sm">{state.taskId}</p>
          <Button asChild variant="link" size="sm"><Link to={`?${detail}`}>{t("tasks.scheduleCreation.open")}</Link></Button>
          {state.reading && <p role="status">{t("tasks.scheduleCreation.reading")}</p>}
          {state.readFailed && <p role="alert">{t("tasks.scheduleCreation.readFailed")}</p>}
          {state.receipt && <p role="status">{t("tasks.scheduleCreation.observed", { status: state.receipt.status })}</p>}
          <Button variant="outline" size="sm" disabled={state.reading} onClick={() => { void controller.current?.refresh(); }}>{t("tasks.scheduleCreation.refresh")}</Button>
        </>}
        {state.phase !== "submitting" && <Button variant="outline" size="sm" onClick={() => { if (current()) onReview(); }}>{t("tasks.scheduleCreation.review")}</Button>}
        {state.receipt && !state.reading && !state.readFailed && <div className="space-y-2">
          <p className="text-sm text-muted-foreground">{t("tasks.scheduleCreation.anotherWarning")}</p>
          <Button variant="outline" size="sm" onClick={() => { if (current()) onNew(); }}>{t("tasks.scheduleCreation.another")}</Button>
        </div>}
      </div>}
    </CardContent>
  </Card>;
}
