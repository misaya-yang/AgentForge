import {
  AlertTriangle,
  CheckCircle2,
  Clock3,
  ExternalLink,
  Fingerprint,
  ShieldAlert,
  ShieldX,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader } from "@/components/ui/card";
import { cn } from "@/lib/utils";

import { formatTimestamp, riskLabels, shortDigest } from "./presentation";
import type {
  ApprovalScope,
  ExactApprovalRequest,
  TrustedConfirmationState,
} from "./types";

export interface ExactApprovalCardProps {
  approval: ExactApprovalRequest;
  trustedConfirmation?: TrustedConfirmationState;
  onRequestTrustedConfirmation?: (approvalId: string) => void;
  onReject?: (approvalId: string) => void;
  onChangeScope?: (approvalId: string, scope: ApprovalScope) => void;
}

const scopeLabels: Record<ApprovalScope, string> = {
  once: "This action only",
  session: "This session",
  workspace: "This workspace",
  narrow_rule: "Matching narrow rule",
};

const trustedStatusLabels: Record<TrustedConfirmationState["status"], string> = {
  not_requested: "Not sent to device",
  waiting_for_device: "Waiting for device",
  shown_on_device: "Shown on device",
  confirmed: "Confirmed on device",
  rejected: "Rejected on device",
  expired: "Confirmation expired",
  invalidated: "Target changed — approval invalidated",
};

function TrustedConfirmation({ state }: { state?: TrustedConfirmationState }) {
  if (!state) {
    return (
      <div className="flex items-start gap-2 rounded-md border border-amber-500/25 bg-amber-500/10 p-3 text-sm text-amber-800 dark:text-amber-200">
        <Clock3 aria-hidden="true" className="mt-0.5 size-4 shrink-0" />
        <div>
          <p className="font-medium">Trusted local confirmation required</p>
          <p className="mt-0.5 text-xs opacity-90">
            The Web UI can request or reject this action, but cannot authorize the host side effect.
          </p>
        </div>
      </div>
    );
  }

  const confirmed = state.status === "confirmed";
  const terminalFailure = ["rejected", "expired", "invalidated"].includes(state.status);

  return (
    <div
      className={cn(
        "flex items-start gap-2 rounded-md border p-3 text-sm",
        confirmed &&
          "border-emerald-500/25 bg-emerald-500/10 text-emerald-800 dark:text-emerald-200",
        terminalFailure && "border-destructive/25 bg-destructive/10 text-destructive",
        !confirmed &&
          !terminalFailure &&
          "border-amber-500/25 bg-amber-500/10 text-amber-800 dark:text-amber-200",
      )}
      aria-live="polite"
    >
      {confirmed ? (
        <CheckCircle2 aria-hidden="true" className="mt-0.5 size-4 shrink-0" />
      ) : terminalFailure ? (
        <ShieldX aria-hidden="true" className="mt-0.5 size-4 shrink-0" />
      ) : (
        <Clock3 aria-hidden="true" className="mt-0.5 size-4 shrink-0" />
      )}
      <div>
        <p className="font-medium">{trustedStatusLabels[state.status]}</p>
        <p className="mt-0.5 text-xs opacity-90">
          {state.detail ?? `Confirmation surface: ${state.deviceLabel}`}
        </p>
        {state.confirmedAt ? (
          <p className="mt-1 text-xs opacity-75">{formatTimestamp(state.confirmedAt)}</p>
        ) : null}
      </div>
    </div>
  );
}

export function ExactApprovalCard({
  approval,
  trustedConfirmation,
  onRequestTrustedConfirmation,
  onReject,
  onChangeScope,
}: ExactApprovalCardProps) {
  const selectableScopes =
    approval.risk === "high" || approval.risk === "critical"
      ? approval.allowedScopes.filter((scope) => scope === "once")
      : approval.allowedScopes;
  const selectedScope = selectableScopes.includes(approval.requestedScope)
    ? approval.requestedScope
    : (selectableScopes[0] ?? "");
  const requestDisabled =
    selectableScopes.length === 0 ||
    trustedConfirmation?.status === "waiting_for_device" ||
    trustedConfirmation?.status === "shown_on_device" ||
    trustedConfirmation?.status === "confirmed" ||
    trustedConfirmation?.status === "invalidated" ||
    trustedConfirmation?.status === "expired";

  return (
    <Card variant="blocking" aria-labelledby={`approval-${approval.id}-heading`}>
      <CardHeader className="flex-row items-start justify-between gap-3 space-y-0 p-4">
        <div className="flex min-w-0 items-start gap-2">
          <ShieldAlert aria-hidden="true" className="mt-0.5 size-5 shrink-0 text-destructive" />
          <div className="min-w-0">
            <h2 id={`approval-${approval.id}-heading`} className="text-sm font-semibold">
              Exact action confirmation
            </h2>
            <p className="mt-1 text-xs text-muted-foreground">
              Run {approval.runId} · {approval.deviceLabel}
            </p>
          </div>
        </div>
        <Badge
          variant="outline"
          className="border-destructive/25 bg-destructive/10 text-destructive"
        >
          {riskLabels[approval.risk]}
        </Badge>
      </CardHeader>

      <CardContent className="space-y-4 p-4 pt-0">
        <div className="rounded-md border border-border bg-background p-3">
          <p className="text-sm font-semibold">{approval.actionLabel}</p>
          <p className="mt-1 font-mono text-xs text-muted-foreground">
            {approval.targetLabel}
          </p>
          <pre className="mt-3 max-h-40 overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted p-3 text-xs text-foreground">
            {approval.normalizedArguments}
          </pre>
        </div>

        <div>
          <h3 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
            Why confirmation is required
          </h3>
          <ul className="mt-2 space-y-1.5 text-sm">
            {approval.riskReasons.map((reason) => (
              <li key={reason} className="flex items-start gap-2">
                <AlertTriangle aria-hidden="true" className="mt-0.5 size-4 shrink-0 text-amber-600" />
                <span>{reason}</span>
              </li>
            ))}
          </ul>
        </div>

        {approval.disclosures.length > 0 ? (
          <div className="rounded-md border border-amber-500/25 bg-amber-500/5 p-3">
            <h3 className="text-xs font-semibold uppercase tracking-wide text-amber-800 dark:text-amber-200">
              Data leaving this device
            </h3>
            <ul className="mt-2 space-y-2 text-sm">
              {approval.disclosures.map((disclosure) => (
                <li key={`${disclosure.destination}-${disclosure.summary}`}>
                  <p className="font-medium">{disclosure.summary}</p>
                  <p className="text-xs text-muted-foreground">
                    Destination: {disclosure.destination}
                    {typeof disclosure.bytes === "number" ? ` · ${disclosure.bytes} bytes` : ""}
                  </p>
                </li>
              ))}
            </ul>
          </div>
        ) : (
          <p className="text-xs text-muted-foreground">
            No external data disclosure was reported for this action.
          </p>
        )}

        <dl className="grid gap-3 rounded-md border border-border/70 bg-muted/25 p-3 text-xs sm:grid-cols-2">
          <div>
            <dt className="text-muted-foreground">Arguments digest</dt>
            <dd className="mt-0.5 font-mono" title={approval.argumentsDigest}>
              {shortDigest(approval.argumentsDigest)}
            </dd>
          </div>
          <div>
            <dt className="text-muted-foreground">Target snapshot</dt>
            <dd className="mt-0.5 font-mono" title={approval.targetSnapshot.digest}>
              {shortDigest(approval.targetSnapshot.digest)}
            </dd>
          </div>
          <div>
            <dt className="text-muted-foreground">Policy snapshot</dt>
            <dd className="mt-0.5 font-mono" title={approval.policySnapshotDigest}>
              {shortDigest(approval.policySnapshotDigest)}
            </dd>
          </div>
          <div>
            <dt className="text-muted-foreground">Expires</dt>
            <dd className="mt-0.5">{formatTimestamp(approval.expiresAt)}</dd>
          </div>
          <div className="sm:col-span-2">
            <dt className="text-muted-foreground">Reversibility</dt>
            <dd className="mt-0.5">
              {approval.reversible
                ? approval.rollbackDescription ?? "A verified rollback receipt is expected."
                : "This action is not represented as reversible."}
            </dd>
          </div>
        </dl>

        <div className="space-y-1.5">
          <label htmlFor={`approval-${approval.id}-scope`} className="text-xs font-medium">
            Requested authorization scope
          </label>
          <select
            id={`approval-${approval.id}-scope`}
            value={selectedScope}
            disabled={!onChangeScope || selectableScopes.length <= 1}
            onChange={(event) => {
              if (event.target.value) {
                onChangeScope?.(approval.id, event.target.value as ApprovalScope);
              }
            }}
            className="h-9 w-full rounded-md border border-input bg-background px-3 text-sm text-foreground outline-hidden focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-60"
          >
            {selectableScopes.length === 0 ? (
              <option value="">No safe authorization scope available</option>
            ) : (
              selectableScopes.map((scope) => (
                <option key={scope} value={scope}>
                  {scopeLabels[scope]}
                </option>
              ))
            )}
          </select>
        </div>

        <TrustedConfirmation state={trustedConfirmation} />

        <div className="flex flex-col-reverse gap-2 sm:flex-row sm:justify-end">
          {onReject ? (
            <Button variant="outline" onClick={() => onReject(approval.id)}>
              Reject action
            </Button>
          ) : null}
          {onRequestTrustedConfirmation ? (
            <Button
              variant="primary"
              disabled={requestDisabled}
              onClick={() => onRequestTrustedConfirmation(approval.id)}
            >
              <ExternalLink aria-hidden="true" className="mr-1.5 size-4" />
              Confirm on {approval.deviceLabel}
            </Button>
          ) : null}
        </div>

        <div className="flex items-start gap-2 text-xs text-muted-foreground">
          <Fingerprint aria-hidden="true" className="mt-0.5 size-3.5 shrink-0" />
          The Local Node must verify the exact arguments, target, and policy digests before dispatch.
        </div>
      </CardContent>
    </Card>
  );
}
