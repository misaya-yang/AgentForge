import { useMemo, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Alert,
  App as AntApp,
  Button,
  Checkbox,
  Drawer,
  Empty,
  Input,
  Modal,
  Select,
  Skeleton,
  Tag,
  Timeline,
  Typography,
} from "antd";
import {
  CheckCircle2,
  ExternalLink,
  GitCompare,
  History,
  Play,
  RefreshCw,
  Rocket,
  RotateCcw,
  ShieldCheck,
  XCircle,
} from "lucide-react";
import { useTranslation } from "react-i18next";

import {
  agentErrorDetail,
  cancelAgentReleaseEvaluation,
  executeAgentReleaseEvaluation,
  getAgentReleaseDiff,
  listAgentPublications,
  listAgentPublishEvents,
  listAgentReleaseEvaluations,
  publishAgent,
  rollbackAgentPublication,
  runAgentReleaseEvaluation,
} from "@/api/agents";
import { getEvalExperimentRun, type EvalExperimentRun } from "@/api/eval";
import type {
  AgentAuthMode,
  AgentChannel,
  AgentPublication,
  AgentReleaseEvaluation,
  AgentReleaseFinding,
  AgentReleaseStatus,
  AgentRole,
  AgentVersion,
} from "@/types/agents";
import { futurePublicExpiry } from "./agentReleaseExpiry";
import { ApiTokenPanel } from "./ApiTokenPanel";
import { AgentVersionComparison } from "./AgentVersionComparison";

const { Paragraph, Text, Title } = Typography;

interface DatasetOption {
  dataset_id: string;
  name: string;
  version?: string;
}

interface AgentReleasePanelProps {
  mode: "eval" | "versions";
  agentId: string;
  draftRevision: number;
  dirty: boolean;
  role: AgentRole;
  versions: AgentVersion[];
  datasets: DatasetOption[];
}

function shortHash(value: string | null | undefined): string {
  const raw = String(value || "").replace(/^sha256:/, "");
  return raw ? `${raw.slice(0, 8)}…${raw.slice(-6)}` : "—";
}

function releaseStatusColor(status: AgentReleaseStatus): string {
  if (status === "passed") return "success";
  if (status === "queued" || status === "running") return "processing";
  if (status === "stale" || status === "cancelled") return "warning";
  return "error";
}

function releaseRunMismatch(
  run: EvalExperimentRun,
  evaluation: AgentReleaseEvaluation,
  versions: AgentVersion[],
): string | null {
  const target = run.target_snapshot ?? {};
  if (run.status !== "succeeded" || run.run_mode !== "live_candidate" || target.candidate_type !== "agent_version") {
    return "Choose a successful live Agent Version run.";
  }
  if (target.agent_id !== evaluation.agent_id || target.agent_spec_hash !== evaluation.spec_hash) {
    return "The run was executed against a different Agent or draft spec.";
  }
  if (run.dataset_id !== evaluation.dataset_id || (evaluation.dataset_manifest_hash && run.dataset_manifest_hash !== evaluation.dataset_manifest_hash)) {
    return "The run's test set does not match this release evaluation.";
  }
  if (!versions.some((version) => version.agent_version_id === target.agent_version_id
    && version.agent_id === evaluation.agent_id
    && version.source_draft_id === evaluation.draft_id
    && version.source_draft_revision === evaluation.draft_revision
    && version.spec_hash === evaluation.spec_hash)) {
    return "The run's immutable Version does not match this evaluated draft revision.";
  }
  const gate = run.metrics?.gate;
  if (!gate || typeof gate !== "object" || Array.isArray(gate)) {
    return "The run's quality gate is unavailable.";
  }
  const { profile, status, reason } = gate as Record<string, unknown>;
  if (profile !== "agent_version_task_suite"
    || (status !== "pass" && !(status === "warning" && reason === "fixed_sample_only"))) {
    return "The run's quality gate does not permit linked publication.";
  }
  return null;
}

function FindingList({
  title,
  findings,
  blocking,
}: {
  title: string;
  findings: AgentReleaseFinding[];
  blocking: boolean;
}) {
  if (findings.length === 0) return null;
  return (
    <section className={`agent-release-findings${blocking ? " is-blocking" : ""}`}>
      <strong>{title}</strong>
      <ul>
        {findings.map((finding) => (
          <li key={`${finding.code}:${finding.field}`}>
            {blocking ? <XCircle size={14} /> : <CheckCircle2 size={14} />}
            <span><b>{finding.code}</b>{finding.message}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

function EvalHistoryCard({
  evaluation,
  canRelease,
  dirty,
  onReview,
  onRetry,
  onCancel,
}: {
  evaluation: AgentReleaseEvaluation;
  canRelease: boolean;
  dirty: boolean;
  onReview: () => void;
  onRetry: () => void;
  onCancel: () => void;
}) {
  const { t, i18n } = useTranslation();
  const locale = i18n.resolvedLanguage === "zh-CN" ? "zh-CN" : "en-US";
  const gate = evaluation.gate_snapshot;
  const blocking = gate.blocking_findings ?? [];
  const nonBlocking = gate.non_blocking_findings ?? [];
  const canReview = evaluation.status === "passed" && !evaluation.stale;
  const canCancel = evaluation.status === "queued" || evaluation.status === "running";
  return (
    <article className={`agent-eval-card is-${evaluation.status}`} data-testid={`agent-eval-${evaluation.status}`}>
      <header>
        <div>
          <Tag className="agent-release-status-tag" color={releaseStatusColor(evaluation.status)}>
            {t(`agents.studio.release.status.${evaluation.status}`)}
          </Tag>
          <strong>{t("agents.studio.release.evalRevision", { revision: evaluation.draft_revision })}</strong>
          <Text type="secondary">{evaluation.channel.toUpperCase()}</Text>
        </div>
        <Text type="secondary">
          {new Date(evaluation.completed_at || evaluation.created_at).toLocaleString(locale)}
        </Text>
      </header>
      <div className="agent-eval-fingerprint-grid">
        <span><small>{t("agents.studio.release.specHash")}</small>{shortHash(evaluation.spec_hash)}</span>
        <span><small>{t("agents.studio.release.runtimeHash")}</small>{shortHash(evaluation.runtime_fingerprint_hash)}</span>
        <span><small>{t("agents.studio.release.profile")}</small>{evaluation.profile_id} · {evaluation.profile_version}</span>
        <span><small>{t("agents.studio.release.dataset")}</small>{evaluation.dataset_id || t("agents.studio.release.noDatasetShort")}</span>
      </div>
      {evaluation.events.length > 0 && (
        <div className="agent-eval-lifecycle" aria-label={t("agents.studio.release.lifecycle")}>
          {evaluation.events.map((event) => (
            <span key={event.event_id} className={`is-${event.status}`}>
              {event.sequence}. {t(`agents.studio.release.status.${event.status}`)}
            </span>
          ))}
        </div>
      )}
      <FindingList title={t("agents.studio.release.blockingFindings")} findings={blocking} blocking />
      <FindingList title={t("agents.studio.release.nonBlockingFindings")} findings={nonBlocking} blocking={false} />
      {(evaluation.stale_reasons ?? []).length > 0 && (
        <Alert
          type="warning"
          showIcon
          title={t("agents.studio.release.staleReasons")}
          description={(evaluation.stale_reasons ?? []).join(", ")}
        />
      )}
      <footer>
        <Text type="secondary">
          {gate.model_quality_evaluated
            ? t("agents.studio.release.modelQualityEvaluated")
            : t("agents.studio.release.providerFreeScope")}
        </Text>
        {canCancel ? (
          <Button
            className="agent-eval-cancel-button"
            danger
            icon={<XCircle size={15} />}
            onClick={onCancel}
          >
            {t("agents.studio.release.cancelEval")}
          </Button>
        ) : canReview ? (
          <Button
            icon={<GitCompare size={15} />}
            disabled={!canRelease || dirty}
            onClick={onReview}
          >
            {t("agents.studio.release.reviewPublish")}
          </Button>
        ) : (
          <Button
            icon={<RefreshCw size={15} />}
            disabled={!canRelease || dirty}
            onClick={onRetry}
          >
            {t("agents.studio.release.retryEval")}
          </Button>
        )}
      </footer>
    </article>
  );
}

export function AgentReleasePanel({
  mode,
  agentId,
  draftRevision,
  dirty,
  role,
  versions,
  datasets,
}: AgentReleasePanelProps) {
  const { t, i18n } = useTranslation();
  const locale = i18n.resolvedLanguage === "zh-CN" ? "zh-CN" : "en-US";
  const { message: messageApi } = AntApp.useApp();
  const queryClient = useQueryClient();
  const canRelease = role === "owner";
  const [datasetId, setDatasetId] = useState<string | null>(null);
  const [channel, setChannel] = useState<AgentChannel>("hosted");
  const [authMode, setAuthMode] = useState<AgentAuthMode>("private");
  const [attachments, setAttachments] = useState(false);
  const [highRiskTools, setHighRiskTools] = useState(false);
  const [allowedOrigins, setAllowedOrigins] = useState("");
  const [publicExpiresAt, setPublicExpiresAt] = useState("");
  const [selectedEvaluation, setSelectedEvaluation] = useState<AgentReleaseEvaluation | null>(null);
  const [publishReason, setPublishReason] = useState("");
  const [runIdDraft, setRunIdDraft] = useState("");
  const [verifiedRunId, setVerifiedRunId] = useState<string | null>(null);
  const [rollbackTarget, setRollbackTarget] = useState<{
    publication: AgentPublication;
    version: AgentVersion;
  } | null>(null);
  const [rollbackReason, setRollbackReason] = useState("");
  const [evidenceVersionId, setEvidenceVersionId] = useState<string | null>(null);
  const idempotencyKeys = useRef(new Map<string, string>());

  const evaluationsQuery = useQuery({
    queryKey: ["agent", agentId, "release-evaluations"],
    queryFn: () => listAgentReleaseEvaluations(agentId),
    retry: false,
    refetchInterval: (query) => (
      (query.state.data ?? []).some((evaluation) => (
        evaluation.status === "queued" || evaluation.status === "running"
      )) ? 1000 : false
    ),
  });
  const publicationsQuery = useQuery({
    queryKey: ["agent", agentId, "publications"],
    queryFn: () => listAgentPublications(agentId),
    retry: false,
  });
  const eventsQuery = useQuery({
    queryKey: ["agent", agentId, "publish-events"],
    queryFn: () => listAgentPublishEvents(agentId),
    retry: false,
  });
  const diffQuery = useQuery({
    queryKey: ["agent", agentId, "release-diff", selectedEvaluation?.evaluation_id],
    queryFn: () => getAgentReleaseDiff(agentId, selectedEvaluation!.evaluation_id),
    enabled: Boolean(selectedEvaluation),
    retry: false,
  });
  const releaseRunQuery = useQuery({
    queryKey: ["agent", agentId, "release-run", selectedEvaluation?.evaluation_id, verifiedRunId],
    queryFn: () => getEvalExperimentRun(verifiedRunId!),
    enabled: Boolean(selectedEvaluation && verifiedRunId),
    retry: false,
  });

  const refreshReleaseState = async () => {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: ["agent", agentId, "release-evaluations"] }),
      queryClient.invalidateQueries({ queryKey: ["agent", agentId, "publications"] }),
      queryClient.invalidateQueries({ queryKey: ["agent", agentId, "publish-events"] }),
      queryClient.invalidateQueries({ queryKey: ["agent", agentId, "versions"] }),
    ]);
  };

  const originValues = () => allowedOrigins
    .split(/[\n,]/)
    .map((value) => value.trim())
    .filter(Boolean);

  const runEvalMutation = useMutation({
    retry: false,
    mutationFn: async () => {
      const queued = await runAgentReleaseEvaluation(agentId, {
        draft_revision: draftRevision,
        dataset_id: datasetId,
        channel,
        auth_mode: authMode,
        channel_policy: {
          attachments,
          high_risk_tools: authMode === "public" ? false : highRiskTools,
          allowed_origins: originValues(),
          ...(authMode === "public" ? { expires_at: futurePublicExpiry(publicExpiresAt) } : {}),
        },
      });
      queryClient.setQueryData<AgentReleaseEvaluation[]>(
        ["agent", agentId, "release-evaluations"],
        (current = []) => [
          queued,
          ...current.filter((item) => item.evaluation_id !== queued.evaluation_id),
        ],
      );
      return executeAgentReleaseEvaluation(agentId, queued.evaluation_id);
    },
    onSuccess: async (evaluation) => {
      await refreshReleaseState();
      setSelectedEvaluation(evaluation.status === "passed" ? evaluation : null);
      setRunIdDraft("");
      setVerifiedRunId(null);
      messageApi.success(t(
        evaluation.status === "cancelled"
          ? "agents.studio.release.evalCancelled"
          : "agents.studio.release.evalComplete",
      ));
    },
    onError: async () => refreshReleaseState(),
  });

  const cancelEvalMutation = useMutation({
    retry: false,
    mutationFn: (evaluationId: string) => cancelAgentReleaseEvaluation(
      agentId,
      evaluationId,
    ),
    onSuccess: async () => {
      await refreshReleaseState();
      messageApi.success(t("agents.studio.release.evalCancelled"));
    },
  });

  const keyFor = (scope: string) => {
    const existing = idempotencyKeys.current.get(scope);
    if (existing) return existing;
    const key = `${scope}:${crypto.randomUUID()}`;
    idempotencyKeys.current.set(scope, key);
    return key;
  };

  const publishScope = (evaluationId: string, runId?: string) =>
    `publish:${evaluationId}:${runId || "provider-free"}`;
  const publishMutation = useMutation({
    retry: false,
    mutationFn: ({ evaluation, runId }: { evaluation: AgentReleaseEvaluation; runId?: string }) => publishAgent(
      agentId,
      evaluation.evaluation_id,
      keyFor(publishScope(evaluation.evaluation_id, runId)),
      publishReason.trim(),
      runId,
    ),
    onSuccess: async (_result, { evaluation, runId }) => {
      idempotencyKeys.current.delete(publishScope(evaluation.evaluation_id, runId));
      await refreshReleaseState();
      messageApi.success(t("agents.studio.release.publishSuccess"));
    },
  });

  const rollbackMutation = useMutation({
    retry: false,
    mutationFn: ({ publication, version }: NonNullable<typeof rollbackTarget>) =>
      rollbackAgentPublication(
        publication.publication_id,
        version.agent_version_id,
        keyFor(`rollback:${publication.publication_id}:${version.agent_version_id}`),
        rollbackReason.trim(),
      ),
    onSuccess: async (_result, { publication, version }) => {
      idempotencyKeys.current.delete(
        `rollback:${publication.publication_id}:${version.agent_version_id}`,
      );
      await refreshReleaseState();
      setRollbackTarget(null);
      setRollbackReason("");
      messageApi.success(t("agents.studio.release.rollbackSuccess"));
    },
  });

  const evaluations = evaluationsQuery.data ?? [];
  const publications = publicationsQuery.data ?? [];
  const events = eventsQuery.data ?? [];
  const latestError = runEvalMutation.error
    || cancelEvalMutation.error
    || publishMutation.error
    || rollbackMutation.error;
  const errorDetail = latestError ? agentErrorDetail(latestError) : null;
  const selectedDiff = diffQuery.data?.diff;
  const selectedGate = selectedEvaluation?.gate_snapshot;
  const releaseRun = verifiedRunId && releaseRunQuery.data?.run_id === verifiedRunId
    ? releaseRunQuery.data : null;
  const runMismatch = releaseRun && selectedEvaluation
    ? releaseRunMismatch(releaseRun, selectedEvaluation, versions) : null;
  const selectedRun = releaseRun && !runMismatch ? releaseRun : null;
  const wantsRun = Boolean(runIdDraft.trim());
  const runAudienceSupported = selectedEvaluation?.auth_mode === "private" || selectedEvaluation?.auth_mode === "tenant";
  const publicExpiry = selectedEvaluation?.auth_mode === "public"
    ? selectedEvaluation.channel_policy.expires_at : null;
  const publicExpiryValid = selectedEvaluation?.auth_mode !== "public"
    || (Boolean(publicExpiry) && new Date(publicExpiry!).getTime() > Date.now());
  const publishDisabled = Boolean(
    !selectedEvaluation
    || selectedEvaluation.status !== "passed"
    || selectedEvaluation.stale
    || !canRelease
    || dirty
    || !publicExpiryValid
    || (selectedGate?.blocking_findings?.length ?? 0) > 0
    || (wantsRun && (!runAudienceSupported || !selectedRun))
  );
  const evidenceEvent = evidenceVersionId
    ? events.find((event) => event.to_version_id === evidenceVersionId)
      ?? events.find((event) => event.from_version_id === evidenceVersionId)
    : undefined;

  const channelOptions = useMemo(() => ([
    { value: "hosted", label: t("agents.studio.release.channels.hosted") },
    { value: "embed", label: t("agents.studio.release.channels.embed") },
    { value: "api", label: t("agents.studio.release.channels.api") },
  ]), [t]);

  if (evaluationsQuery.isLoading || publicationsQuery.isLoading || eventsQuery.isLoading) {
    return <Skeleton active paragraph={{ rows: 10 }} />;
  }

  if (mode === "eval") {
    return (
      <div className="agent-release-panel" data-testid="agent-release-eval-panel">
        {!canRelease && (
          <Alert type="info" showIcon title={t("agents.studio.release.ownerOnlyTitle")} description={t("agents.studio.release.ownerOnlyDescription")} />
        )}
        {dirty && (
          <Alert type="warning" showIcon title={t("agents.studio.release.saveFirstTitle")} description={t("agents.studio.release.saveFirstDescription")} />
        )}
        {datasets.length === 0 && (
          <Alert type="info" showIcon title={t("agents.studio.release.noDatasetTitle")} description={t("agents.studio.release.noDatasetDescription")} />
        )}
        <Alert type="warning" showIcon title={t("agents.studio.release.providerFreeTitle")} description={t("agents.studio.release.providerFreeDescription")} />
        {errorDetail && (
          <Alert
            type="error"
            showIcon
            closable
            title={t("agents.studio.release.operationFailed")}
            description={`${errorDetail.code ? `${errorDetail.code}: ` : ""}${errorDetail.message}`}
          />
        )}
        <section className="agent-release-runner" aria-label={t("agents.studio.release.runnerLabel")}>
          <header>
            <div><Title level={4}>{t("agents.studio.release.runTitle")}</Title><Paragraph type="secondary">{t("agents.studio.release.runDescription", { revision: draftRevision })}</Paragraph></div>
            <Tag>{t("agents.studio.release.serverOwned")}</Tag>
          </header>
          <div className="agent-release-controls">
            <label><span>{t("agents.studio.release.dataset")}</span><Select allowClear value={datasetId} placeholder={t("agents.studio.release.noDatasetShort")} options={datasets.map((dataset) => ({ value: dataset.dataset_id, label: dataset.version ? `${dataset.name} · ${dataset.version}` : dataset.name }))} onChange={(value) => setDatasetId(value ?? null)} /></label>
            <label><span>{t("agents.studio.release.channel")}</span><Select value={channel} options={channelOptions} onChange={setChannel} /></label>
            <label><span>{t("agents.studio.release.authMode")}</span><Select value={authMode} options={["private", "tenant", "token", "public"].map((value) => ({ value, label: t(`agents.studio.release.auth.${value}`) }))} onChange={(value) => { setAuthMode(value); if (value === "public") setHighRiskTools(false); else setPublicExpiresAt(""); }} /></label>
            <label className="agent-release-origins"><span>{t("agents.studio.release.allowedOrigins")}</span><Input value={allowedOrigins} onChange={(event) => setAllowedOrigins(event.target.value)} placeholder="https://app.example.com" /></label>
            {authMode === "public" && <label><span>{t("agents.studio.release.publicExpiry", { defaultValue: "Public access ends" })}</span><Input type="datetime-local" value={publicExpiresAt} onChange={(event) => setPublicExpiresAt(event.target.value)} aria-required="true" aria-invalid={!futurePublicExpiry(publicExpiresAt)} /></label>}
          </div>
          {authMode === "public" && <Alert type={futurePublicExpiry(publicExpiresAt) ? "info" : "warning"} showIcon title={futurePublicExpiry(publicExpiresAt)
            ? t("agents.studio.release.publicExpiryNotice", { defaultValue: "This public audience and expiry are frozen in the release evaluation." })
            : t("agents.studio.release.publicExpiryRequired", { defaultValue: "Choose a future expiry before evaluating public access." })} />}
          <div className="agent-release-policy-controls">
            <Checkbox checked={attachments} onChange={(event) => setAttachments(event.target.checked)}>{t("agents.studio.release.attachments")}</Checkbox>
            <Checkbox disabled={authMode === "public"} checked={highRiskTools} onChange={(event) => setHighRiskTools(event.target.checked)}>{t("agents.studio.release.highRiskTools")}</Checkbox>
          </div>
          <footer>
            <Text type="secondary">{t("agents.studio.release.exactRevision", { revision: draftRevision })}</Text>
            <Button type="primary" icon={<Play size={15} />} loading={runEvalMutation.isPending} disabled={!canRelease || dirty || (authMode === "public" && !futurePublicExpiry(publicExpiresAt))} onClick={() => runEvalMutation.mutate()}>{t("agents.studio.release.runEval")}</Button>
          </footer>
        </section>

        <section className="agent-eval-history" aria-live="polite">
          <header><div><Title level={4}>{t("agents.studio.release.historyTitle")}</Title><Paragraph type="secondary">{t("agents.studio.release.historyDescription")}</Paragraph></div><Button aria-label={t("agents.common.retry")} icon={<RefreshCw size={15} />} onClick={() => void evaluationsQuery.refetch()} /></header>
          {evaluations.length === 0 ? <Empty description={t("agents.studio.release.noEvaluations")} /> : evaluations.map((evaluation) => (
            <EvalHistoryCard
              key={evaluation.evaluation_id}
              evaluation={evaluation}
              canRelease={canRelease}
              dirty={dirty}
              onReview={() => { setSelectedEvaluation(evaluation); setPublishReason(""); setRunIdDraft(""); setVerifiedRunId(null); }}
              onRetry={() => runEvalMutation.mutate()}
              onCancel={() => cancelEvalMutation.mutate(evaluation.evaluation_id)}
            />
          ))}
        </section>

        <Drawer
          className="agent-publish-drawer"
          size={640}
          title={t("agents.studio.release.publishTitle")}
          open={Boolean(selectedEvaluation)}
          onClose={() => { setSelectedEvaluation(null); setRunIdDraft(""); setVerifiedRunId(null); publishMutation.reset(); }}
          footer={selectedEvaluation && (
            <div className="agent-publish-footer">
              <Button onClick={() => setSelectedEvaluation(null)}>{t("agents.common.cancel")}</Button>
              <Button type="primary" icon={<Rocket size={15} />} disabled={publishDisabled} loading={publishMutation.isPending} onClick={() => publishMutation.mutate({ evaluation: selectedEvaluation, runId: selectedRun?.run_id })}>{t("agents.studio.release.publish")}</Button>
            </div>
          )}
        >
          {selectedEvaluation && (
            <div className="agent-publish-sheet" data-testid="agent-publish-sheet">
              <Alert type="info" showIcon title={t("agents.studio.release.publishSource", { revision: selectedEvaluation.draft_revision })} description={`${t("agents.studio.release.runtimeHash")}: ${shortHash(selectedEvaluation.runtime_fingerprint_hash)}`} />
              <div className="agent-publish-facts">
                <span><small>{t("agents.studio.release.channel")}</small>{selectedEvaluation.channel.toUpperCase()}</span>
                <span><small>{t("agents.studio.release.authMode")}</small>{t(`agents.studio.release.auth.${selectedEvaluation.auth_mode}`)}</span>
                {selectedEvaluation.auth_mode === "public" && <span><small>{t("agents.studio.release.publicExpiry", { defaultValue: "Public access ends" })}</small>{publicExpiry ? new Date(publicExpiry).toLocaleString(locale) : "—"}</span>}
                <span><small>{t("agents.studio.release.profile")}</small>{selectedEvaluation.profile_id}</span>
                <span><small>{t("agents.studio.release.specHash")}</small>{shortHash(selectedEvaluation.spec_hash)}</span>
              </div>
              <section className="agent-publish-checks">
                <Title level={5}>{t("agents.studio.release.resourceChecks")}</Title>
                <p><ShieldCheck size={15} />{t("agents.studio.release.serverValidationPassed")}</p>
                <p><ShieldCheck size={15} />{t("agents.studio.release.authorizationRechecked")}</p>
                <p><ShieldCheck size={15} />{t("agents.studio.release.noSecrets")}</p>
              </section>
              <FindingList title={t("agents.studio.release.blockingFindings")} findings={selectedGate?.blocking_findings ?? []} blocking />
              {!publicExpiryValid && <Alert type="error" showIcon title={t("agents.studio.release.publicExpiryElapsed", { defaultValue: "This evaluation's public expiry has passed. Run a new evaluation with a future expiry." })} />}
              <FindingList title={t("agents.studio.release.nonBlockingFindings")} findings={selectedGate?.non_blocking_findings ?? []} blocking={false} />
              <section className="agent-release-diff">
                <Title level={5}>{t("agents.studio.release.diffTitle")}</Title>
                {diffQuery.isLoading && <Skeleton active paragraph={{ rows: 4 }} />}
                {diffQuery.isError && <Alert type="error" showIcon title={t("agents.studio.release.diffFailed")} action={<Button onClick={() => void diffQuery.refetch()}>{t("agents.common.retry")}</Button>} />}
                {selectedDiff && Object.entries(selectedDiff.sections).map(([name, section]) => (
                  <article key={name} className={section.changed ? "is-changed" : ""}>
                    <span><GitCompare size={14} /><strong>{t(`agents.studio.release.diffSections.${name}`)}</strong></span>
                    <Tag color={section.changed ? "blue" : "default"}>{section.changed ? t("agents.studio.release.changed") : t("agents.studio.release.unchanged")}</Tag>
                    {name === "prompt" && <small>{t("agents.studio.release.promptLengthDiff", { before: section.before_length ?? 0, after: section.after_length ?? 0 })}</small>}
                    {section.changed_paths.length > 0 && <small>{section.changed_paths.join(", ")}</small>}
                  </article>
                ))}
              </section>
              <section className="agent-publish-run-evidence" data-testid="agent-publish-run-evidence">
                <Title level={5}>{t("agents.studio.release.selectedRunTitle", { defaultValue: "Selected Agent Version run" })}</Title>
                {runAudienceSupported ? <>
                  <Paragraph type="secondary">{t("agents.studio.release.selectedRunHint", { defaultValue: "Optional: enter a successful live run ID from Eval. Verify its immutable Version and test set before linking it to this publication." })}</Paragraph>
                  <Input.Search
                    aria-label={t("agents.studio.release.runId", { defaultValue: "Experiment run ID" })}
                    value={runIdDraft}
                    placeholder={t("agents.studio.release.runId", { defaultValue: "Experiment run ID" })}
                    enterButton={t("agents.studio.release.verifyRun", { defaultValue: "Verify run" })}
                    loading={releaseRunQuery.isFetching}
                    onChange={(event) => { setRunIdDraft(event.target.value); setVerifiedRunId(null); }}
                    onSearch={() => setVerifiedRunId(runIdDraft.trim() || null)}
                  />
                  {releaseRunQuery.isError && verifiedRunId && <Alert type="error" showIcon title={t("agents.studio.release.runUnavailable", { defaultValue: "This run could not be loaded. Check its ID and access." })} />}
                  {runMismatch && <Alert type="error" showIcon title={runMismatch} />}
                  {selectedRun && <div className="agent-publish-facts" data-testid="agent-publish-selected-run">
                    <span><small>{t("agents.studio.release.runId", { defaultValue: "Experiment run ID" })}</small>{selectedRun.run_id}</span>
                    <span><small>{t("agents.studio.release.runVersion", { defaultValue: "Evaluated Version" })}</small>{String(selectedRun.target_snapshot?.agent_version_id)}</span>
                    <span><small>{t("agents.studio.release.runDataset", { defaultValue: "Evaluated test set" })}</small>{selectedRun.dataset_id || "—"}</span>
                    <span><small>{t("agents.studio.release.runModel", { defaultValue: "Resolved model" })}</small>{String(selectedRun.target_snapshot?.model_id || "—")}</span>
                    <span><small>{t("agents.studio.release.runKnowledge", { defaultValue: "Resolved knowledge IDs" })}</small>{Array.isArray(selectedRun.target_snapshot?.knowledge_dataset_ids) ? selectedRun.target_snapshot.knowledge_dataset_ids.join(", ") : "—"}</span>
                  </div>}
                </> : <Alert type="info" showIcon title={t("agents.studio.release.runAudienceHint", { defaultValue: "Linked model-quality runs are available for private and tenant releases." })} />}
              </section>
              <label className="agent-release-reason"><span>{t("agents.studio.release.reason")}</span><Input.TextArea rows={3} maxLength={1000} value={publishReason} onChange={(event) => setPublishReason(event.target.value)} placeholder={t("agents.studio.release.reasonPlaceholder")} /></label>
              <Alert type="warning" showIcon title={t("agents.studio.release.sessionPinningTitle")} description={t("agents.studio.release.sessionPinningDescription")} />
              {publishMutation.isSuccess && <Alert type="success" showIcon title={t("agents.studio.release.publishSuccess")} description={t("agents.studio.release.publishSuccessDescription", { version: publishMutation.data.version.version_number })} />}
            </div>
          )}
        </Drawer>
      </div>
    );
  }

  return (
    <div className="agent-release-panel" data-testid="agent-release-versions-panel">
      {!canRelease && <Alert type="info" showIcon title={t("agents.studio.release.ownerOnlyTitle")} description={t("agents.studio.release.rollbackOwnerOnly")} />}
      <Alert type="info" showIcon title={t("agents.studio.release.sessionPinningTitle")} description={t("agents.studio.release.sessionPinningDescription")} />
      {errorDetail && <Alert type="error" showIcon closable title={t("agents.studio.release.operationFailed")} description={`${errorDetail.code ? `${errorDetail.code}: ` : ""}${errorDetail.message}`} />}
      <section className="agent-publication-grid">
        {publications.length === 0 ? <Empty description={t("agents.studio.release.noPublications")} /> : publications.map((publication) => (
          <article key={publication.publication_id}>
            <header><Tag className={publication.status === "active" ? "agent-release-success-tag" : undefined} color={publication.status === "active" ? "success" : "warning"}>{publication.status}</Tag><strong>{publication.channel.toUpperCase()}</strong></header>
            <p>{t("agents.studio.release.currentVersion", { version: publication.version_number ?? "—" })}</p>
            <small>{t("agents.studio.release.pointerStable")}</small>
            {publication.channel === "hosted" && (
              <a className="agent-channel-link" href={`/a/${publication.public_id}`} target="_blank" rel="noreferrer">
                <ExternalLink size={14} />{t("agents.studio.channels.openHosted")}
              </a>
            )}
            {publication.channel === "embed" && (
              <code className="agent-channel-code">{`<script src="${window.location.origin}/agent-widget.js" data-agent-id="${publication.public_id}"></script>`}</code>
            )}
            {publication.channel === "api" && (
              <code className="agent-channel-code">{`${window.location.origin}/api/v1/agent-runtime/${publication.publication_id}`}</code>
            )}
          </article>
        ))}
      </section>
      {publications.filter((publication) => publication.channel === "api").map((publication) => (
        <ApiTokenPanel key={publication.publication_id} publication={publication} canManage={canRelease} />
      ))}
      <AgentVersionComparison agentId={agentId} />
      <section className="agent-version-history">
        <header><div><Title level={4}>{t("agents.studio.release.versionsTitle")}</Title><Paragraph type="secondary">{t("agents.studio.release.versionsDescription")}</Paragraph></div><History size={18} /></header>
        {versions.length === 0 ? <Empty description={t("agents.studio.release.noVersions")} /> : versions.map((version) => {
          const currentChannels = publications.filter((publication) => publication.version_id === version.agent_version_id);
          const historicalChannels = publications.filter((publication) => events.some((event) => (
            event.publication_id === publication.publication_id
            && (event.from_version_id === version.agent_version_id || event.to_version_id === version.agent_version_id)
          )));
          return (
            <article key={version.agent_version_id} className={currentChannels.length ? "is-current" : ""}>
              <div className="agent-version-number"><strong>v{version.version_number}</strong><span>{t("agents.common.draftLabel", { revision: version.source_draft_revision })}</span></div>
              <div className="agent-version-meta"><span>{shortHash(version.spec_hash)}</span><span>{new Date(version.created_at).toLocaleString(locale)}</span>{currentChannels.map((publication) => <Tag className="agent-release-success-tag" key={publication.channel} color="success">{publication.channel}</Tag>)}</div>
              <div className="agent-version-actions">
                <Button size="small" icon={<GitCompare size={14} />} onClick={() => setEvidenceVersionId(evidenceVersionId === version.agent_version_id ? null : version.agent_version_id)}>{t("agents.studio.release.viewEvidence")}</Button>
                {publications.filter((publication) => publication.version_id !== version.agent_version_id).map((publication) => (
                  <Button key={publication.publication_id} size="small" icon={<RotateCcw size={14} />} disabled={!canRelease || !historicalChannels.some((item) => item.publication_id === publication.publication_id)} onClick={() => { setRollbackTarget({ publication, version }); setRollbackReason(""); }}>{t("agents.studio.release.rollbackChannel", { channel: publication.channel.toUpperCase() })}</Button>
                ))}
              </div>
              {evidenceVersionId === version.agent_version_id && (
                <div className="agent-version-evidence">
                  {evidenceEvent ? <><strong>{t("agents.studio.release.releaseEvidence")}</strong><span>{evidenceEvent.operation} · {shortHash(evidenceEvent.request_hash)}</span><small>{t("agents.studio.release.evidenceImmutable")}</small></> : <Text type="secondary">{t("agents.studio.release.noReleaseEvidence")}</Text>}
                </div>
              )}
            </article>
          );
        })}
      </section>
      <section className="agent-publish-timeline">
        <Title level={4}>{t("agents.studio.release.auditTitle")}</Title>
        {events.length === 0 ? <Empty description={t("agents.studio.release.noAuditEvents")} /> : <Timeline items={events.map((event) => ({
          color: event.operation === "rollback" ? "orange" : "green",
          content: <div><strong>{t(`agents.studio.release.operation.${event.operation}`)}</strong><p>{shortHash(event.from_version_id)} → {shortHash(event.to_version_id)}</p><small>{new Date(event.created_at).toLocaleString(locale)} · {event.actor_id}</small></div>,
        }))} />}
      </section>
      <Modal
        title={t("agents.studio.release.rollbackTitle")}
        open={Boolean(rollbackTarget)}
        okText={t("agents.studio.release.confirmRollback")}
        okButtonProps={{ disabled: !rollbackReason.trim(), danger: true }}
        confirmLoading={rollbackMutation.isPending}
        onCancel={() => setRollbackTarget(null)}
        onOk={() => rollbackTarget && rollbackMutation.mutate(rollbackTarget)}
        afterClose={() => setRollbackReason("")}
      >
        {rollbackTarget && <div className="agent-rollback-confirm"><Alert type="warning" showIcon title={t("agents.studio.release.rollbackWarning", { channel: rollbackTarget.publication.channel.toUpperCase(), version: rollbackTarget.version.version_number })} description={t("agents.studio.release.rollbackRecheck")} /><label><span>{t("agents.studio.release.rollbackReason")}</span><Input.TextArea autoFocus rows={3} maxLength={1000} value={rollbackReason} onChange={(event) => setRollbackReason(event.target.value)} /></label></div>}
      </Modal>
    </div>
  );
}
