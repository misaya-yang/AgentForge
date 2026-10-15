import { useState } from "react";
import { Alert, Button, Input, Select, Typography } from "antd";
import { Ban, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useAgentDataDeletion } from "./useAgentDataDeletion";

export function AgentDataDeletionPanel({ agentId, allowed, legalHold, onReceipt }: {
  agentId: string; allowed: boolean; legalHold: boolean; onReceipt: () => Promise<unknown>;
}) {
  const { t } = useTranslation();
  const [scope, setScope] = useState<"retention" | "user" | "tenant">("retention");
  const [subject, setSubject] = useState("");
  const actions = useAgentDataDeletion({ agentId, allowed, legalHold, onReceipt });
  const observed = actions.observation;
  return <section aria-label={t("agents.analytics.deletionRecovery.title")}>
    {observed ? <>
      <Alert showIcon type={observed.unknown || observed.receipt?.status !== "completed" ? "warning" : "success"}
        title={observed.unknown ? t("agents.analytics.deletionRecovery.unknown") : t(`agents.analytics.deletion.${observed.receipt!.status}`, { code: observed.receipt!.errorCode || "" })}
        description={t("agents.analytics.deletionRecovery.scopeNotice")} />
      <p>{t("agents.analytics.deletionRecovery.confirmTarget", { agent: observed.target.agentId,
        scope: t(`agents.analytics.scopes.${observed.target.scope}`), subject: observed.target.subject || "—" })}</p>
      <p>{t("agents.analytics.deletionRecovery.requestKey")}: <Typography.Text copyable>{observed.target.key}</Typography.Text></p>
      {observed.receipt && <div>
        <p>{t("agents.analytics.deletionRecovery.lastReceipt")}: <Typography.Text copyable>{observed.receipt.deletionId}</Typography.Text> · {observed.receipt.status}</p>
        {observed.receipt.attemptCount !== null && <p>{t("agents.analytics.deletionRecovery.attemptCount", { count: observed.receipt.attemptCount })}</p>}
        {observed.receipt.counts.length ? <ul>{observed.receipt.counts.map(([name, count]) => <li key={name}>{name}: {count}</li>)}</ul> : <p>{t("agents.analytics.deletionRecovery.countsUnavailable")}</p>}
      </div>}
      {observed.refreshFailed && <Alert type="warning" showIcon title={t("agents.analytics.deletionRecovery.refreshFailed")} />}
      {observed.unknown || observed.receipt?.retryable
        ? <Button danger loading={actions.pending} disabled={!actions.allowed} onClick={actions.retry}>{t("agents.analytics.deletionRecovery.retryAction")}</Button>
        : <Button disabled={!actions.allowed || actions.pending} onClick={actions.newOperation}>{t("agents.analytics.deletionRecovery.newOperation")}</Button>}
    </> : <div className="agent-operation-list">
      <Select value={scope} onChange={setScope} disabled={actions.pending || !actions.allowed}
        aria-label={t("agents.analytics.deletionScope")} options={["retention", "user", "tenant"].map(value => ({ value, label: t(`agents.analytics.scopes.${value}`) }))} />
      {scope === "user" && <Input value={subject} maxLength={255} disabled={actions.pending || !actions.allowed}
        onChange={event => setSubject(event.target.value)} placeholder={t("agents.analytics.subjectUser")} aria-label={t("agents.analytics.subjectUser")} />}
      <Button danger type="primary" icon={legalHold ? <Ban size={15} /> : <Trash2 size={15} />}
        disabled={!actions.allowed || (scope === "user" && !subject.trim())} loading={actions.pending}
        onClick={() => actions.start(scope, subject)}>{t(legalHold ? "agents.analytics.legalHoldActive" : "agents.analytics.deleteData")}</Button>
    </div>}
  </section>;
}
