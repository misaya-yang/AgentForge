import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import { changeScheduledTask, readSchedule, type ScheduledTask, type ScheduleAction } from "@/api/scheduledTasks";

type Observation = { action: ScheduleAction; accepted: boolean; status: ScheduledTask["status"] | null; uncertain: boolean; changed: boolean };
export function ScheduledTaskActions({ taskId, task, available, onObserved }: {
  taskId: string; task: ScheduledTask | null; available: boolean; onObserved: () => void;
}) {
  const { t } = useTranslation(); const auth = useAuthenticatedReadScope();
  const owner = useMemo(() => ({ active: true, taskId, authority: auth.id }), [taskId, auth.id]);
  const latest = useRef({ task, available, onObserved }); latest.current = { task, available, onObserved };
  const flight = useRef<object | null>(null);
  const eventEpoch = useRef(0); const epoch = eventEpoch.current;
  const [busy, setBusy] = useState(false);
  const [confirmation, setConfirmation] = useState<ScheduledTask | null>(null);
  const [observation, setObservation] = useState<Observation | null>(null);
  useLayoutEffect(() => { owner.active = true; return () => { owner.active = false; flight.current = null; }; }, [owner]);
  const current = () => owner.active && auth.isCurrent();
  const usable = () => current() && latest.current.available;
  const initial = usable() && task?.task_id.toLowerCase() === taskId.toLowerCase() ? task : null;
  const label = (key: string) => t(`tasks.scheduleActions.${key}`);
  async function perform(action: ScheduleAction, expected: ScheduledTask) {
    if (!usable() || flight.current || observation || eventEpoch.current !== epoch) return;
    eventEpoch.current++;
    const attempt = {}; flight.current = attempt; setBusy(true); setConfirmation(null);
    let accepted = false; let submitted = false;
    try {
      const before = await readSchedule(taskId);
      if (!usable() || flight.current !== attempt) return;
      if (before.status !== expected.status || !["active", "paused"].includes(before.status)) {
        setObservation({ action, accepted: false, status: before.status, uncertain: false, changed: true }); return;
      }
      submitted = true;
      try { await changeScheduledTask(taskId, action); accepted = true; }
      catch { /* A lost POST response may follow a committed state change. Never replay it. */ }
      if (!current() || flight.current !== attempt) return;
      setObservation({ action, accepted, status: null, uncertain: !accepted, changed: false });
      try {
        const observed = await readSchedule(taskId);
        if (!current() || flight.current !== attempt) return;
        setObservation({ action, accepted, status: observed.status, uncertain: !accepted, changed: false });
        latest.current.onObserved();
      } catch { /* Keep the action receipt even when the exact GET is unavailable. */ }
    } catch {
      if (current()) setObservation({ action, accepted, status: null, uncertain: submitted, changed: !submitted });
    } finally { if (flight.current === attempt) { flight.current = null; if (current()) setBusy(false); } }
  }
  async function recheck() {
    if (!current() || flight.current || !observation || eventEpoch.current !== epoch) return;
    eventEpoch.current++;
    const attempt = {}; flight.current = attempt; setBusy(true);
    try {
      const observed = await readSchedule(taskId);
      if (current() && flight.current === attempt) { setObservation({ ...observation, status: observed.status }); latest.current.onObserved(); }
    } catch { if (current()) setObservation({ ...observation, status: null }); }
    finally { if (flight.current === attempt) { flight.current = null; if (current()) setBusy(false); } }
  }
  if (!current()) return null;
  return <section aria-label={label("title")} className="space-y-2">
    {observation ? <>
      <p role="status">{label(observation.accepted ? "accepted" : observation.uncertain ? "unknown" : "notSent")}</p>
      <p>{label("target")}: {taskId} · {observation.action}</p>
      <p>{observation.status ? `${label("observed")}: ${observation.status}` : label("unavailable")}</p>
      <p>{label("concurrency")}</p>
      {observation.action === "cancel" && <p>{label("cancelLimit")}</p>}
      <Button variant="outline" disabled={busy} onClick={() => void recheck()}>{label("recheck")}</Button>
      {observation.status && <Button variant="outline" disabled={busy || !usable()} onClick={() => { if (usable() && !flight.current && eventEpoch.current === epoch) { eventEpoch.current++; setObservation(null); } }}>{label("newAction")}</Button>}
    </> : confirmation ? <>
      <p role="alert">{label("cancelConfirm")}</p><p>{confirmation.title} · {taskId}</p>
      <Button variant="destructive" disabled={busy || !usable()} onClick={() => void perform("cancel", confirmation)}>{label("confirmCancel")}</Button>
      <Button variant="outline" disabled={busy} onClick={() => { eventEpoch.current++; setConfirmation(null); }}>{label("keep")}</Button>
    </> : initial && <div className="flex flex-wrap gap-2">
      {initial.status === "active" && <Button disabled={busy} variant="outline" onClick={() => void perform("pause", initial)}>{t("tasks.scheduled.pause", "Pause")}</Button>}
      {initial.status === "paused" && <Button disabled={busy} variant="outline" onClick={() => void perform("resume", initial)}>{t("tasks.scheduled.resume", "Resume")}</Button>}
      {["active", "paused"].includes(initial.status) && <Button disabled={busy} variant="destructive" onClick={() => { if (usable() && !flight.current && eventEpoch.current === epoch) { eventEpoch.current++; setConfirmation(initial); } }}>{t("tasks.scheduled.cancel", "Cancel schedule and active run")}</Button>}
    </div>}
  </section>;
}
