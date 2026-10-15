import { useTranslation } from "react-i18next";
import { approveToolCall } from "@/api/assistant";
import type { SdkToolActivity as Activity } from "@/features/chat/stream/sdkLifecycle";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { useAuthStore } from "@/store/useAuthStore";
import { capabilityDisplayName } from "@/pages/agents/agentCatalogPresentation";
import { ToolApprovalCard } from "@/pages/assistant/components/ToolApprovalCard";

export function SdkToolActivity({ items, active, authorityScope }: {
  items: Activity[]; active: boolean; authorityScope?: string;
}) {
  const { t } = useTranslation();
  const user = useAuthStore((state) => state.user);
  // Old-account cards must not fetch details or act using a new account's token.
  if (!user || authorityScope !== sdkAuthorityScope(user)) return null;
  return <div className="w-full space-y-3" data-message-supplemental="sdk-tools">
    {items.map((item) => <section key={`${item.runId}:${item.toolCallId}`} className="space-y-2">
      {item.approvalId && item.approvalStatus === "pending" && !item.executionStatus && active
        ? <ToolApprovalCard tool={{ id: item.toolCallId, name: item.tool, status: "approval_required", approvalId: item.approvalId }}
            approvalSource="sdk" sdkRunId={item.runId}
            onSdkDecision={(approved, binding) => approveToolCall(item.approvalId!, { approved, ...binding }).then(() => undefined)} />
        : <div role="status" className="rounded-lg border bg-muted/30 p-3 text-xs">
            <p className="font-medium">{capabilityDisplayName(item.tool)}</p>
            {item.executionStatus && item.approvalStatus && item.approvalStatus !== "pending" &&
              <p>{t("services.sdkAgent.lifecycle.decision", { status: t(`services.sdkAgent.lifecycle.status.${item.approvalStatus}`) })}</p>}
            <p>{item.executionStatus
              ? t("services.sdkAgent.lifecycle.execution", { status: t(`services.sdkAgent.lifecycle.status.${item.executionStatus}`) })
              : item.approvalStatus === "pending"
                ? t("services.sdkAgent.lifecycle.interrupted")
                : t("services.sdkAgent.lifecycle.resolved", { status: t(`services.sdkAgent.lifecycle.status.${item.approvalStatus}`) })}</p>
            {item.executionId && <p className="mt-1 break-all text-muted-foreground">{t("services.sdkAgent.lifecycle.executionId")}: {item.executionId}</p>}
          </div>}
    </section>)}
  </div>;
}
