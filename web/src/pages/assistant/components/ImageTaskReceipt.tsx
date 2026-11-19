import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { readImageTask, type ImageTaskObservation } from "@/api/assistant";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";

export default function ImageTaskReceipt({ taskId, sessionId, turnId }: {
  taskId: string; sessionId: string; turnId?: string;
}) {
  const { t } = useTranslation();
  const auth = useAuthenticatedReadScope();
  const owner = useMemo(() => ({ active: true, taskId, sessionId, turnId, authId: auth.id }), [taskId, sessionId, turnId, auth.id]);
  const flight = useRef<{ owner: typeof owner; controller: AbortController } | null>(null);
  const [state, setState] = useState<{ owner: typeof owner; busy: boolean; failed: boolean; value: ImageTaskObservation | null } | null>(null);
  useLayoutEffect(() => {
    owner.active = true;
    return () => {
      owner.active = false;
      if (flight.current?.owner === owner) { flight.current.controller.abort(); flight.current = null; }
    };
  }, [owner]);
  const current = () => owner.active && auth.isCurrent();
  const valid = /^imt_[0-9a-f]{20}$/i.test(taskId) && Boolean(sessionId.trim());
  const observed = current() && state?.owner === owner ? state : null;
  const label = (key: string) => t(`assistant.imageTaskReceipt.${key}`);
  async function check() {
    if (!current() || !valid || flight.current) return;
    const request = { owner, controller: new AbortController() }; flight.current = request;
    setState({ owner, busy: true, failed: false, value: null });
    try {
      const value = await readImageTask(taskId, sessionId, turnId, request.controller.signal);
      if (current() && flight.current === request) setState({ owner, busy: false, failed: false, value });
    } catch {
      if (current() && flight.current === request) setState({ owner, busy: false, failed: true, value: null });
    } finally {
      if (flight.current === request) flight.current = null;
    }
  }
  const value = observed?.value;
  const known = value && ["pending", "claimed", "running", "completed", "failed", "unknown", "dead_letter"].includes(value.status);
  return <div className="mt-2 space-y-1 text-xs">
    <button type="button" disabled={!current() || !valid || observed?.busy === true} onClick={() => void check()}
      className="rounded border border-current/30 px-2 py-1 disabled:opacity-50">{label(observed?.busy ? "checking" : "check")}</button>
    <p>{label("readOnly")}</p>
    {observed?.failed && <p role="alert">{label("unavailable")}</p>}
    {value && <div role="status" aria-live="polite">
      <p>{label("observed")}</p>
      <p>{label("status")}: {known ? label(value.status) : value.status}</p>
      {value.progress !== null && <p>{label("progress")}: {value.progress}%</p>}
      {value.effectiveModel && <p>{label("model")}: {value.effectiveModel}</p>}
    </div>}
  </div>;
}
