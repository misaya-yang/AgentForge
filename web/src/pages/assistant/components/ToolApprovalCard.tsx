import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { getAgentRuntimeApproval, type AgentApprovalPreview } from "@/api/agentThreads";
import { getAssistantToolApproval } from "@/api/assistant";
import { sdkApprovalDisplayStatus, sdkApprovalPreviewIsActionable } from "@/features/chat/stream/sdkLifecycle";
import { sdkApprovalBinding, sameSdkApprovalBinding, type SdkApprovalBinding } from "@/features/chat/stream/sdkApprovalBinding";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { useAuthStore } from "@/store/useAuthStore";
import { capabilityDisplayName } from "@/pages/agents/agentCatalogPresentation";
import type { ToolTimelineItem } from "../types";

interface ToolApprovalCardBaseProps {
  tool: ToolTimelineItem;
  runtimeThreadId?: string;
  sourceAccessRevoked?: boolean;
}

type ToolApprovalCardProps = ToolApprovalCardBaseProps & ({
  approvalSource: "sdk";
  sdkRunId: string;
  onSdkDecision: (approved: boolean, binding: SdkApprovalBinding) => void | Promise<void>;
  onApprove?: never;
  onReject?: never;
} | {
  approvalSource?: "runtime";
  sdkRunId?: never;
  onSdkDecision?: never;
  onApprove: () => void | Promise<void>;
  onReject: () => void | Promise<void>;
});

export function ToolApprovalCard(props: ToolApprovalCardProps) {
  const user = useAuthStore((state) => state.user);
  const scope = sdkAuthorityScope(user);
  return <ToolApprovalCardDetails key={JSON.stringify([scope, props.approvalSource, props.runtimeThreadId, props.tool.approvalId, props.sdkRunId, props.tool.id, props.tool.name])} {...props} />;
}

function ToolApprovalCardDetails({
  tool, runtimeThreadId, sdkRunId, approvalSource = "runtime", sourceAccessRevoked = false, onApprove, onReject, onSdkDecision,
}: ToolApprovalCardProps) {
  const { t } = useTranslation();
  const [preview, setPreview] = useState<AgentApprovalPreview | null>(null);
  const [reviewedBinding, setReviewedBinding] = useState<SdkApprovalBinding | null>(null);
  const [approvalStatus, setApprovalStatus] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [checking, setChecking] = useState(false);
  const checkingRef = useRef<number | null>(null);
  const [expiresAtMs, setExpiresAtMs] = useState<number | null>(null);
  const [nowMs, setNowMs] = useState(() => Date.now());
  const pendingRef = useRef(false);
  const mountedRef = useRef(true);
  const generation = useRef(0);
  const token = useRef(useAuthStore.getState().token);
  const authorityScope = useRef(sdkAuthorityScope(useAuthStore.getState().user));
  const blocked = useRef(sourceAccessRevoked);
  blocked.current = sourceAccessRevoked;
  const [decisionRecorded, setDecisionRecorded] = useState(false);
  const isCurrent = useCallback(() => mountedRef.current && !blocked.current &&
    useAuthStore.getState().token === token.current &&
    sdkAuthorityScope(useAuthStore.getState().user) === authorityScope.current, []);
  useEffect(() => {
    mountedRef.current = true;
    return () => { mountedRef.current = false; generation.current += 1; checkingRef.current = null; };
  }, []);
  const displayName = capabilityDisplayName(tool.name);

  const refresh = useCallback(async () => {
    if (!isCurrent() || checkingRef.current !== null) return null;
    const request = ++generation.current;
    if ((approvalSource === "runtime" && !runtimeThreadId) || !tool.approvalId) {
      setError(t("assistant.activity.approvalDetailsUnavailable", "Action details cannot be verified."));
      return null;
    }
    checkingRef.current = request;
    setChecking(true);
    try {
      const value = approvalSource === "sdk"
        ? await getAssistantToolApproval(tool.approvalId)
        : await getAgentRuntimeApproval(runtimeThreadId!, tool.approvalId);
      if (!isCurrent() || request !== generation.current) return null;
      if (approvalSource === "sdk" && (!("approval_id" in value.approval) ||
          value.approval.approval_id !== tool.approvalId || value.approval.tool_name !== tool.name ||
          !("run_id" in value.approval) || !sdkRunId || value.approval.run_id !== sdkRunId ||
          !("tool_call_id" in value.approval) || value.approval.tool_call_id !== tool.id)) {
        throw new Error("approval_identity_mismatch");
      }
      const binding = approvalSource === "sdk" ? sdkApprovalBinding(value, {
        approvalId: tool.approvalId, runId: sdkRunId!, toolCallId: tool.id, tool: tool.name,
      }) : null;
      setReviewedBinding(binding);
      // Runtime summaries can remain pending after the deadline until durable
      // cleanup catches up. Expiry is authoritative even without an action preview.
      value.approval.status = sdkApprovalDisplayStatus(value.approval.status,
        "expires_in_seconds" in value.approval ? value.approval.expires_in_seconds : value.preview?.expires_in_seconds,
        approvalSource === "sdk" && "active" in value.approval ? value.approval.active : undefined);
      if (approvalSource === "sdk" && !sdkApprovalPreviewIsActionable(value.preview, tool.name)) {
        // Unsupported/malformed exact previews cannot offer approval or show raw arguments.
        value.preview = { can_approve: false, authorization_scope: "",
          reason: typeof value.preview?.reason === "string" ? value.preview.reason :
            t("assistant.activity.approvalDetailsUnavailable", "Action details cannot be verified.") };
      }
      setApprovalStatus(value.approval.status);
      setPreview(value.preview);
      const checkedAt = Date.now();
      setNowMs(checkedAt);
      setExpiresAtMs(typeof value.preview.expires_in_seconds === "number"
        ? checkedAt + value.preview.expires_in_seconds * 1000 : null);
      setError(approvalSource === "sdk" && !binding
        ? t("assistant.activity.approvalDetailsUnavailable", "Action details cannot be verified.") : "");
      return { ...value, binding };
    } catch {
      if (!isCurrent() || request !== generation.current) return null;
      setPreview(null);
      setReviewedBinding(null);
      setApprovalStatus("");
      setError(t("assistant.activity.approvalDetailsUnavailable", "Action details cannot be verified."));
      return null;
    } finally {
      if (checkingRef.current === request) {
        checkingRef.current = null;
        if (mountedRef.current) setChecking(false);
      }
    }
  }, [runtimeThreadId, tool.approvalId, tool.id, tool.name, sdkRunId, approvalSource, isCurrent, t]);

  useEffect(() => {
    if (sourceAccessRevoked) { setPreview(null); setReviewedBinding(null); generation.current += 1; return; }
    void refresh();
  }, [refresh, sourceAccessRevoked]);

  useEffect(() => {
    if (approvalStatus !== "pending" || expiresAtMs === null) return;
    const timer = window.setInterval(() => {
      const now = Date.now();
      setNowMs(now);
      if (now >= expiresAtMs) {
        window.clearInterval(timer);
        void refresh();
      }
    }, 1000);
    return () => window.clearInterval(timer);
  }, [approvalStatus, expiresAtMs, refresh]);
  const expiresInSeconds = expiresAtMs === null ? null : Math.max(0, Math.ceil((expiresAtMs - nowMs) / 1000));

  const decide = async (approved: boolean) => {
    if (!isCurrent() || pendingRef.current || checkingRef.current !== null || decisionRecorded || approvalStatus !== "pending") return;
    if (approvalSource === "sdk" && !reviewedBinding) return;
    if (approved && (sourceAccessRevoked || !preview?.can_approve)) return;
    pendingRef.current = true;
    setBusy(true);
    try {
      const latest = await refresh();
      if (!isCurrent() || !latest || latest.approval.status !== "pending") return;
      if (approvalSource === "sdk" && !sameSdkApprovalBinding(reviewedBinding, latest.binding)) {
        setError(t("assistant.activity.approvalChanged", "Action details changed or expired. Review the current request before approving."));
        return;
      }
      if (approved) {
        if (!latest.preview.can_approve || latest.preview.arguments_hash !== preview?.arguments_hash) {
          setError(t("assistant.activity.approvalChanged", "Action details changed or expired. Review the current request before approving."));
          return;
        }
      }
      if (approvalSource === "sdk") {
        // Both decisions bind the exact reviewed action. Never retry the mutation.
        await onSdkDecision!(approved, latest.binding!);
      } else if (approved) {
        await onApprove!();
      } else {
        await onReject!();
      }
      if (!isCurrent()) return;
      setDecisionRecorded(true);
      await refresh();
    } catch {
      const latest = await refresh();
      if (!isCurrent()) return;
      if (!latest || latest.approval.status === "pending") {
        setError(t("assistant.activity.approvalDecisionFailed", "Decision result could not be confirmed. Check the current request before making another decision."));
      }
    } finally {
      pendingRef.current = false;
      if (mountedRef.current) setBusy(false);
    }
  };

  const checkCurrentRequest = () => {
    if (!isCurrent() || pendingRef.current || checkingRef.current !== null) return;
    void refresh();
  };

  return (
    <div role="alertdialog" aria-labelledby={`approval-${tool.id}-title`} className="rounded-xl border border-amber-500/40 bg-amber-500/10 p-4">
      <p id={`approval-${tool.id}-title`} className="text-sm font-semibold">
        {t("assistant.activity.approvalRequired", "Approval required")}: {displayName}
      </p>
      <p className="mt-1 text-xs text-muted-foreground">
        {t("assistant.activity.approvalScope", "Approval applies to this action only. A changed or expired request needs a new decision.")}
      </p>
      {preview?.can_approve && !sourceAccessRevoked ? (
        <div className="mt-2 space-y-1 text-xs">
          <p><strong>{t("assistant.activity.approvalEffect", "Effect")}:</strong> {preview.effect}</p>
          <p><strong>{t("assistant.activity.approvalTarget", "Target")}:</strong> {preview.target}</p>
          <p><strong>{t("services.sdkAgent.lifecycle.scope")}:</strong> {preview.authorization_scope}</p>
          <p><strong>{t("assistant.activity.approvalExpiry", "Expires in")}:</strong> {expiresInSeconds}s</p>
          <div className="max-h-52 space-y-1 overflow-y-auto rounded border border-amber-500/20 bg-background/50 p-2">
            {preview.parameters?.map((parameter) => (
              <p key={parameter.name} className="break-all whitespace-pre-wrap"><strong>{parameter.name}:</strong> {parameter.value}</p>
            ))}
          </div>
        </div>
      ) : (
        <p className="mt-2 text-xs text-amber-800 dark:text-amber-200">
          {sourceAccessRevoked ? t("assistant.sourceNeedsNewConversation") : preview?.reason || error || t("assistant.activity.approvalLoading", "Verifying this action…")}
        </p>
      )}
      {error && preview?.can_approve && <p role="alert" className="mt-2 text-xs text-red-600">{error}</p>}
      {decisionRecorded &&
        <p role="status" className="mt-2 text-xs">{t("services.sdkAgent.lifecycle.decisionRecorded")}</p>}
      {!decisionRecorded && approvalStatus && approvalStatus !== "pending" &&
        <p role="status" className="mt-2 text-xs">{t("services.sdkAgent.lifecycle.resolved", {
          status: t(`services.sdkAgent.lifecycle.status.${sdkApprovalDisplayStatus(approvalStatus)}`),
        })}</p>}
      <div className="mt-3 flex gap-2">
        <button type="button" disabled={(approvalSource === "sdk" && !reviewedBinding) || busy || checking || decisionRecorded || approvalStatus !== "pending" || sourceAccessRevoked || !preview?.can_approve || expiresInSeconds === 0} onClick={() => void decide(true)} className="rounded-md bg-[hsl(var(--assistant-accent))] px-3 py-1.5 text-xs font-medium text-white disabled:opacity-50">
          {t("common.approve", "Approve")}
        </button>
        <button type="button" disabled={(approvalSource === "sdk" && !reviewedBinding) || busy || checking || decisionRecorded || approvalStatus !== "pending" || sourceAccessRevoked} onClick={() => void decide(false)} className="rounded-md border border-[hsl(var(--assistant-border))] px-3 py-1.5 text-xs disabled:opacity-50">
          {t("common.reject", "Reject")}
        </button>
        <button type="button" disabled={busy || checking || sourceAccessRevoked || !tool.approvalId || (approvalSource === "runtime" && !runtimeThreadId)} onClick={checkCurrentRequest} className="rounded-md border border-[hsl(var(--assistant-border))] px-3 py-1.5 text-xs disabled:opacity-50">
          {checking ? t("assistant.activity.approvalChecking", "Checking current request…") : t("assistant.activity.approvalRecheck", "Check current request")}
        </button>
      </div>
    </div>
  );
}

export default ToolApprovalCard;
