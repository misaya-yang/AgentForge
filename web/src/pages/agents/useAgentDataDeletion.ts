import { App } from "antd";
import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { requestAgentDataDeletion } from "@/api/agents";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import { generateUUID } from "@/lib/utils";

type Scope = "retention" | "user" | "tenant";
type Target = { agentId: string; scope: Scope; subject: string | null; key: string };
type Receipt = { deletionId: string; status: "pending" | "completed" | "failed" | "blocked";
  counts: Array<[string, number]>; errorCode: string | null; attemptCount: number | null;
  completedAt: string | null; retryable: boolean };
type Observation = { target: Target; receipt: Receipt | null; unknown: boolean; refreshFailed: boolean };
type Operation = { owner: object; submitted: boolean; close?: () => void };

function receipt(value: unknown, target: Target, previous: Receipt | null): Receipt {
  const raw = value as Record<string, unknown> | null;
  if (!raw || typeof raw !== "object" || Array.isArray(raw)
    || raw.agent_id !== target.agentId || raw.scope !== target.scope
    || (raw.subject_user_id ?? null) !== target.subject
    || typeof raw.deletion_id !== "string" || !raw.deletion_id || raw.deletion_id.length > 160
    || (previous && raw.deletion_id !== previous.deletionId)
    || typeof raw.status !== "string" || !["pending", "completed", "failed", "blocked"].includes(raw.status)
    || (raw.retryable !== undefined && typeof raw.retryable !== "boolean")
    || (raw.completed_at !== null && typeof raw.completed_at !== "string")) {
    throw new Error("Unconfirmed deletion receipt");
  }
  const status = raw.status as Receipt["status"];
  const counts = raw.deleted_counts;
  return { deletionId: raw.deletion_id, status,
    counts: counts && typeof counts === "object" && !Array.isArray(counts)
      ? Object.entries(counts).filter((entry): entry is [string, number] => Number.isSafeInteger(entry[1]) && entry[1] >= 0) : [],
    errorCode: typeof raw.error_code === "string" ? raw.error_code : null,
    attemptCount: Number.isSafeInteger(raw.attempt_count) && (raw.attempt_count as number) >= 0 ? raw.attempt_count as number : null,
    completedAt: raw.completed_at as string | null,
    retryable: status === "pending" || (status === "failed" && raw.completed_at === null && raw.retryable !== false),
  };
}

/** Current-view receipt only. Every continuation is a separately confirmed destructive POST. */
export function useAgentDataDeletion(options: {
  agentId: string; allowed: boolean; legalHold: boolean; onReceipt: () => Promise<unknown>;
}) {
  const auth = useAuthenticatedReadScope();
  const { modal } = App.useApp();
  const { t } = useTranslation();
  const owner = useMemo(() => ({ active: true, agentId: options.agentId, authId: auth.id }), [options.agentId, auth.id]);
  const permissions = useRef(options); permissions.current = options;
  const operation = useRef<Operation | null>(null);
  const retained = useRef<{ owner: object; value: Observation } | null>(null);
  const [observation, setObservation] = useState<{ owner: object; value: Observation } | null>(null);
  const [pending, setPending] = useState<object | null>(null);
  useLayoutEffect(() => {
    owner.active = true;
    return () => {
      owner.active = false;
      const active = operation.current;
      if (active?.owner === owner) { active.close?.(); operation.current = null; }
    };
  }, [owner]);
  const current = () => owner.active && auth.isCurrent() && permissions.current.allowed;
  const writable = () => current() && !permissions.current.legalHold;
  const publish = (value: Observation) => {
    retained.current = { owner, value };
    if (current()) setObservation({ owner, value });
  };
  const confirm = (scope?: Scope, subject = "") => {
    if (!writable() || operation.current) return;
    const previous = retained.current?.owner === owner ? retained.current.value : null;
    if (scope && previous) return;
    if (!scope && (!previous || (!previous.unknown && !previous.receipt?.retryable))) return;
    if (scope && !["retention", "user", "tenant"].includes(scope)) return;
    const normalized = subject.trim();
    if (scope === "user" && (!normalized || normalized.length > 255)) return;
    const target: Target = previous?.target || { agentId: options.agentId, scope: scope!,
      subject: scope === "user" ? normalized : null, key: `agent-delete-${generateUUID()}` };
    const request: Operation = { owner, submitted: false };
    operation.current = request; setPending(owner);
    const release = () => {
      if (operation.current === request) { operation.current = null; if (owner.active) setPending(null); }
    };
    const dialog = modal.confirm({
      title: t(previous ? "agents.analytics.deletionRecovery.retryTitle" : "agents.analytics.confirm.deletionTitle"),
      content: t("agents.analytics.deletionRecovery.confirmTarget", {
        agent: target.agentId, scope: t(`agents.analytics.scopes.${target.scope}`), subject: target.subject || "—",
      }) + " " + t(previous ? "agents.analytics.deletionRecovery.retryDescription" : "agents.analytics.confirm.deletionDescription"),
      okText: t(previous ? "agents.analytics.deletionRecovery.retryAction" : "agents.analytics.confirm.deletionAction"),
      cancelText: t("agents.common.cancel"), okButtonProps: { danger: true },
      onCancel: () => { if (!request.submitted) release(); },
      afterClose: () => { if (!request.submitted) release(); },
      onOk: async () => {
        if (request.submitted || operation.current !== request || !writable()) { if (!request.submitted) release(); return; }
        request.submitted = true;
        const before: Observation = previous || { target, receipt: null, unknown: true, refreshFailed: false };
        publish({ ...before, unknown: true, refreshFailed: false });
        try {
          const result = await requestAgentDataDeletion(target.agentId, {
            scope: target.scope, ...(target.subject !== null ? { subject_user_id: target.subject } : {}),
            idempotency_key: target.key,
          });
          if (!current()) return;
          const confirmed: Observation = { target, receipt: receipt(result, target, before.receipt), unknown: false, refreshFailed: false };
          publish(confirmed);
          try { await permissions.current.onReceipt(); }
          catch { if (current()) publish({ ...confirmed, refreshFailed: true }); }
        } catch {
          if (current()) publish({ ...before, unknown: true, refreshFailed: false });
        } finally { release(); }
      },
    });
    request.close = () => dialog.destroy();
  };
  const visible = observation?.owner === owner && current() ? observation.value : null;
  return {
    observation: visible, pending: pending === owner, allowed: writable(),
    start: (scope: Scope, subject: string) => confirm(scope, subject), retry: () => confirm(),
    newOperation: () => {
      const previous = retained.current?.owner === owner ? retained.current.value : null;
      if (!writable() || operation.current || !previous || previous.unknown || !previous.receipt || previous.receipt.retryable) return;
      retained.current = null; setObservation(null);
    },
  };
}
