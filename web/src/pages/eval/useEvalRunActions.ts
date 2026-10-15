import { App } from "antd";
import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { cancelEvalExperimentRun, retryFailedEvalExperimentCases } from "@/api/eval";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import { generateUUID } from "@/lib/utils";

type RunActionOperation = { owner: object; submitted: boolean; close?: () => void; release?: () => void };

/** Commands belong to the confirmed run and visit, never to a later selection. */
export function useEvalRunActions(options: {
  runId?: string;
  experimentId?: string;
  visit: object;
  allowed: boolean;
  onRetryQueued: (runId: string) => void;
}) {
  const auth = useAuthenticatedReadScope();
  const queryClient = useQueryClient();
  const { modal } = App.useApp();
  const owner = useMemo(() => ({
    active: true, runId: options.runId, experimentId: options.experimentId,
    visit: options.visit, allowed: options.allowed, accessId: auth.id,
  }),
    [options.runId, options.experimentId, options.visit, options.allowed, auth.id]);
  const operation = useRef<RunActionOperation | null>(null);
  const keys = useRef(new Map<string, string>());
  const [pendingOwner, setPendingOwner] = useState<object | null>(null);
  const [notice, setNotice] = useState<{ owner: object; text: string; warning: boolean } | null>(null);
  useLayoutEffect(() => {
    owner.active = true;
    return () => {
      owner.active = false;
      const request = operation.current;
      if (request?.owner === owner && !request.submitted) {
        request.close?.(); request.release?.();
      }
    };
  }, [owner]);
  const current = () => owner.active && options.allowed && auth.isCurrent();
  const refresh = async () => {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: ["eval", "run", options.runId] }, { throwOnError: true }),
      queryClient.invalidateQueries({ queryKey: ["eval", "run-results", options.runId] }, { throwOnError: true }),
      queryClient.invalidateQueries({ queryKey: ["eval", "experiment", options.experimentId] }, { throwOnError: true }),
    ]);
  };
  const confirm = (caseIds?: string[]) => {
    if (!options.runId || !current() || operation.current) return;
    const request: RunActionOperation = { owner, submitted: false };
    operation.current = request;
    setPendingOwner(owner);
    const release = () => {
      if (operation.current === request) { operation.current = null; setPendingOwner(null); }
    };
    request.release = release;
    // Copy the selection before opening a confirmation; callers cannot change its meaning.
    const selected = caseIds ? [...new Set(caseIds)].sort() : undefined;
    if (selected && !selected.length) { release(); return; }
    const runId = options.runId;
    const dialog = modal.confirm({
      title: selected ? `Retry ${selected.length} selected failed cases?` : "Cancel this evaluation run?",
      content: selected
        ? "This creates a separate one-attempt run using frozen inputs and can incur model cost. Only failures before runtime dispatch or completed model-only cases with no tool calls can be replayed. Cases with tool effects or uncertain dispatch are blocked for manual reconciliation."
        : "Cancellation stops new cases. Cases already started may still need runtime interruption.",
      okText: selected ? "Confirm new attempt" : "Cancel run",
      okButtonProps: { danger: !selected },
      onCancel: () => { if (!request.submitted) release(); },
      afterClose: () => { if (!request.submitted) release(); },
      onOk: async () => {
        if (request.submitted) return;
        if (operation.current !== request || !current()) { release(); return; }
        request.submitted = true;
        setNotice(null);
        let accepted = false;
        try {
          if (selected) {
            const selectionKey = `eval:retry:${JSON.stringify([runId, selected])}`;
            let stored: string | null = null;
            try { stored = sessionStorage.getItem(selectionKey); } catch { /* In-memory fallback. */ }
            const idempotencyKey = stored || keys.current.get(selectionKey) || generateUUID();
            keys.current.set(selectionKey, idempotencyKey);
            try { sessionStorage.setItem(selectionKey, idempotencyKey); } catch { /* In-memory fallback. */ }
            const batch = await retryFailedEvalExperimentCases(runId, selected, true, idempotencyKey);
            accepted = true;
            const retryRunId = batch.jobs[0]?.run_id;
            // Keep the same key if a malformed/lost response cannot identify the accepted run.
            if (!retryRunId) throw new Error("Missing retry run identity");
            if (!current()) return;
            await refresh();
            if (!current()) return;
            keys.current.delete(selectionKey);
            try { sessionStorage.removeItem(selectionKey); } catch { /* Storage may be unavailable. */ }
            options.onRetryQueued(retryRunId);
          } else {
            const result = await cancelEvalExperimentRun(runId);
            accepted = true;
            if (!current()) return;
            if (result.run_id !== runId || !["queued", "running", "succeeded", "failed", "cancelled"].includes(result.status)
              || !Number.isInteger(result.runtime_interrupt_pending) || result.runtime_interrupt_pending < 0) {
              throw new Error("Unconfirmed cancellation response");
            }
            setNotice({ owner, warning: result.runtime_interrupt_pending > 0 || result.status !== "cancelled",
              text: result.status === "cancelled"
                ? result.runtime_interrupt_pending > 0
                  ? "Run cancelled; some runtime interrupts remain unconfirmed. Already-started work may continue."
                  : "Run cancelled. This does not prove that external effects have stopped."
                : `Cancellation returned run status: ${result.status}. The run was not confirmed cancelled.` });
            await refresh();
          }
        } catch {
          if (current()) setNotice({ owner, warning: true, text: accepted
            ? "The server accepted the request, but its result could not be fully refreshed. Refresh the run before taking further action."
            : selected
              ? "The retry request outcome is unconfirmed. Refresh the run list; confirming the same case selection again reuses its request key. A different selection creates a separate attempt and may incur cost."
              : "Cancellation could not be confirmed. Refresh the run to check its current state; already-started work may continue." });
        } finally { release(); }
      },
    });
    request.close = () => dialog.destroy();
  };
  return {
    pending: pendingOwner !== null,
    notice: notice?.owner === owner && auth.isCurrent() ? notice : null,
    cancel: () => confirm(),
    retry: (caseIds: string[]) => confirm(caseIds),
  };
}
