import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { cancelTask, getAssistantRunStatus } from "@/api/assistant";
import { sdkRunIsActive, type SdkRunReference } from "@/features/chat/stream/sdkRun";
import { createSdkRunController, emptySdkRunControlState, type SdkRunControlState } from "@/features/chat/stream/sdkRunController";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { useAuthStore } from "@/store/useAuthStore";

interface Props { reference: SdkRunReference; authorityScope?: string; interrupted: boolean }

export function SdkRunControls(props: Props) {
  const user = useAuthStore((state) => state.user);
  // A previous account's message cannot fetch or cancel with a new owner's credentials.
  if (!user || props.authorityScope !== sdkAuthorityScope(user)) return null;
  return <SdkRunDetails key={JSON.stringify([props.authorityScope, props.reference])} {...props} />;
}

function SdkRunDetails({ reference, authorityScope, interrupted }: Props) {
  const { t } = useTranslation();
  const token = useAuthStore((state) => state.token);
  const [expanded, setExpanded] = useState(false);
  const [observed, setObserved] = useState<{ token: typeof token; state: SdkRunControlState }>({ token, state: emptySdkRunControlState });
  const control = useRef<ReturnType<typeof createSdkRunController> | null>(null);
  const state = observed.token === token ? observed.state : emptySdkRunControlState;
  const { runId, engine, sessionId } = reference;
  useEffect(() => {
    const controller = createSdkRunController({ runId, engine, sessionId }, {
      isCurrent: () => !!token && useAuthStore.getState().token === token &&
        sdkAuthorityScope(useAuthStore.getState().user) === authorityScope,
      read: (id, signal) => getAssistantRunStatus(id, { signal, timeout: 10_000 }),
      cancel: (id, signal) => cancelTask(id, "client_interrupt", { signal, timeout: 10_000 }),
    });
    control.current = controller;
    controller.subscribe((next) => setObserved({ token, state: next }));
    return () => { controller.dispose(); if (control.current === controller) control.current = null; };
  }, [runId, engine, sessionId, authorityScope, token]);

  // Only opened, active cards poll. Terminal or unavailable reads retain manual refresh.
  const active = state.run !== null && sdkRunIsActive(state.run.status);
  useEffect(() => {
    if (!expanded || !active) return;
    const timer = window.setInterval(() => { void control.current?.refresh(); }, 2000);
    return () => window.clearInterval(timer);
  }, [expanded, active, token]);
  const usage = state.run?.usage;
  return <section className="w-full rounded-lg border bg-muted/20 p-3 text-xs space-y-2" data-message-supplemental="sdk-run">
    <div className="flex flex-wrap items-center gap-2">
      <span className="font-medium">{t("services.sdkAgent.run.title")}</span>
      <span className="break-all text-muted-foreground">{runId}</span>
      <button type="button" disabled={state.pending !== null || !token}
        aria-expanded={expanded} className="rounded border px-2 py-1 disabled:opacity-50"
        onClick={() => { setExpanded(true); void control.current?.refresh(); }}>
        {state.pending === "read" ? t("services.sdkAgent.run.loading") :
          t(expanded ? "services.sdkAgent.run.refresh" : "services.sdkAgent.run.inspect")}
      </button>
    </div>
    {interrupted && <p>{t("services.sdkAgent.run.interrupted")}</p>}
    {expanded && <>
      <p role="status">{t("services.sdkAgent.run.statusLabel")}: {t(`services.sdkAgent.run.status.${state.run?.status ?? "unknown"}`)}</p>
      {state.error && <p role="alert">{t("services.sdkAgent.run.unavailable")}</p>}
      {usage && <div className="space-y-1 text-muted-foreground">
        {usage.state === "unavailable" && <p>{t("services.sdkAgent.run.usageUnavailable")}</p>}
        {usage.inputTokens !== undefined && <p>{t("services.sdkAgent.run.inputTokens")}: {usage.inputTokens}</p>}
        {usage.outputTokens !== undefined && <p>{t("services.sdkAgent.run.outputTokens")}: {usage.outputTokens}</p>}
        {usage.totalTokens !== undefined && <p>{t("services.sdkAgent.run.totalTokens")}: {usage.totalTokens}</p>}
        <p>{usage.providerCostUsd !== undefined ? t("services.sdkAgent.run.providerCost", { amount: usage.providerCostUsd }) : t("services.sdkAgent.run.costUnavailable")}</p>
        <p>{t("services.sdkAgent.run.accountingNotice")}</p>
      </div>}
      {state.cancellation === "unknown" && <p role="alert">{t("services.sdkAgent.run.cancelUnknown")}</p>}
      {state.cancellation === "recorded" && <p role="status">{t("services.sdkAgent.run.cancelRecorded")}</p>}
      {(state.run?.status === "cancelled" || state.run?.status === "expired" || state.cancellation !== "none") &&
        <p className="text-muted-foreground">{t("services.sdkAgent.run.providerNotice")}</p>}
      {active && <button type="button" className="rounded border px-2 py-1 disabled:opacity-50"
        disabled={state.pending !== null || state.cancellation !== "none" || !token}
        onClick={() => { void control.current?.cancel(); }}>
        {t(state.pending === "cancel" ? "services.sdkAgent.run.cancelling" : "services.sdkAgent.run.cancel")}
      </button>}
    </>}
  </section>;
}
