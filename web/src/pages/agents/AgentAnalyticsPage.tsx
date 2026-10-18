import { DeferredTable as Table } from "@/components/DeferredTable";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Alert,
  App as AntApp,
  Button,
  Empty,
  Form,
  Input,
  InputNumber,
  Pagination,
  Select,
  Skeleton,
  Switch,
  Tabs,
  Tag,
  Typography,
  type TableColumnsType,
} from "antd";
import {
  ArrowLeft,
  BarChart3,
  DatabaseZap,
  ExternalLink,
  KeyRound,
  RefreshCw,
  Save,
  ShieldCheck,
} from "lucide-react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { useTranslation } from "react-i18next";

import {
  agentErrorDetail,
  getAgent,
  getAgentAnalytics,
  getAgentGovernance,
  invalidateAgentCache,
  listAgentAuditEvents,
  listAgentPublications,
  listAgentVersions,
  revokeAllAgentCredentials,
  updateAgentGovernance,
  type AgentAnalyticsFilters,
} from "@/api/agents";
import type {
  AgentAuditEvent,
  AgentGovernancePolicy,
  AgentGovernancePolicyUpdate,
  AgentOperationsTrace,
} from "@/types/agents";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import { AgentDataDeletionPanel } from "./AgentDataDeletionPanel";
import "./agent-studio.css";

const { Text, Title } = Typography;
const PAGE_SIZE = 20;

function formatMetric(value: number | null | undefined, suffix = ""): string {
  if (value === null || value === undefined) return "—";
  return `${new Intl.NumberFormat().format(value)}${suffix}`;
}

function preview(value: string): string {
  return value || "—";
}

export function AgentAnalyticsPage() {
  const { agentId = "" } = useParams();
  const access = useAuthenticatedReadScope();
  return <AgentAnalyticsView key={`${access.id}:${agentId}`} agentId={agentId} access={access} />;
}

function AgentAnalyticsView({ agentId, access }: {
  agentId: string; access: ReturnType<typeof useAuthenticatedReadScope>;
}) {
  const navigate = useNavigate();
  const { t, i18n } = useTranslation();
  const { message, modal } = AntApp.useApp();
  const queryClient = useQueryClient();
  const locale = i18n.resolvedLanguage === "zh-CN" ? "zh-CN" : "en-US";
  const [versionId, setVersionId] = useState<string>();
  const [publicationId, setPublicationId] = useState<string>();
  const [channel, setChannel] = useState<string>();
  const [status, setStatus] = useState<string>();
  const [startedAfter, setStartedAfter] = useState("");
  const [startedBefore, setStartedBefore] = useState("");
  const [offset, setOffset] = useState(0);
  const [governanceDraft, setGovernanceDraft] = useState<AgentGovernancePolicy | null>(null);

  const view = useMemo(() => ({ active: true }), []);
  const permission = useRef(false);
  useLayoutEffect(() => { view.active = true; return () => { view.active = false; }; }, [view]);
  const current = () => view.active && access.isCurrent();
  const govern = () => current() && permission.current;
  const read = async <T,>(operation: () => Promise<T>): Promise<T> => {
    if (!current()) throw new Error("Retired analytics view");
    const result = await operation();
    if (!current()) throw new Error("Retired analytics view");
    return result;
  };

  const filters = useMemo<AgentAnalyticsFilters>(() => ({
    agent_version_id: versionId,
    publication_id: publicationId,
    channel,
    status,
    started_after: startedAfter ? new Date(`${startedAfter}T00:00:00Z`).toISOString() : undefined,
    started_before: startedBefore ? new Date(`${startedBefore}T23:59:59Z`).toISOString() : undefined,
    limit: PAGE_SIZE,
    offset,
  }), [channel, offset, publicationId, startedAfter, startedBefore, status, versionId]);

  const agentQuery = useQuery({
    queryKey: ["agent", agentId, access.id],
    queryFn: () => read(() => getAgent(agentId)),
    enabled: Boolean(agentId && current()),
    retry: false,
  });
  const analyticsQuery = useQuery({
    queryKey: ["agent", agentId, "analytics", filters, access.id],
    queryFn: () => read(() => getAgentAnalytics(agentId, filters)),
    enabled: Boolean(agentId && current()),
    retry: false,
  });
  const versionsQuery = useQuery({
    queryKey: ["agent", agentId, "versions", access.id],
    queryFn: () => read(() => listAgentVersions(agentId)),
    enabled: Boolean(agentId && current()),
    retry: false,
  });
  const publicationsQuery = useQuery({
    queryKey: ["agent", agentId, "publications", access.id],
    queryFn: () => read(() => listAgentPublications(agentId)),
    enabled: Boolean(agentId && current()),
    retry: false,
  });
  const accessDenied = [agentQuery.error, analyticsQuery.error].some(error => error && [401, 403].includes(agentErrorDetail(error).status || 0));
  const canGovern = current() && !accessDenied && agentQuery.data?.caller_role === "owner" && analyticsQuery.data?.caller_role === "owner";
  permission.current = canGovern;
  const auditQuery = useQuery({
    queryKey: ["agent", agentId, "audits", filters, access.id],
    queryFn: () => read(() => listAgentAuditEvents(agentId, filters)),
    enabled: Boolean(agentId && canGovern),
    retry: false,
  });
  const governanceQuery = useQuery({
    queryKey: ["agent", agentId, "governance", access.id],
    queryFn: () => read(() => getAgentGovernance(agentId)),
    enabled: Boolean(agentId && canGovern),
    retry: false,
  });

  useEffect(() => {
    if (governanceQuery.data) setGovernanceDraft(governanceQuery.data);
  }, [governanceQuery.data]);

  const governanceMutation = useMutation({
    mutationFn: (input: AgentGovernancePolicyUpdate) => {
      if (!govern()) throw new Error("Retired governance action");
      return updateAgentGovernance(agentId, input);
    },
    onSuccess: (policy) => {
      if (!govern()) return;
      setGovernanceDraft(policy);
      queryClient.setQueryData(["agent", agentId, "governance", access.id], policy);
      void analyticsQuery.refetch();
      message.success(t("agents.analytics.policySaved"));
    },
    onError: (error) => { if (govern()) message.error(agentErrorDetail(error).message); },
  });
  const cacheMutation = useMutation({
    mutationFn: () => {
      if (!govern()) throw new Error("Retired governance action");
      return invalidateAgentCache(agentId);
    },
    onSuccess: (result) => {
      if (!govern()) return;
      void governanceQuery.refetch();
      void auditQuery.refetch();
      message.success(t("agents.analytics.cacheInvalidated", { count: result.deleted_cache_rows }));
    },
    onError: (error) => { if (govern()) message.error(agentErrorDetail(error).message); },
  });
  const credentialsMutation = useMutation({
    mutationFn: () => {
      if (!govern()) throw new Error("Retired governance action");
      return revokeAllAgentCredentials(agentId);
    },
    onSuccess: () => {
      if (!govern()) return;
      void auditQuery.refetch();
      message.success(t("agents.analytics.credentialsRevoked"));
    },
    onError: (error) => { if (govern()) message.error(agentErrorDetail(error).message); },
  });
  const resetFilters = () => {
    setVersionId(undefined);
    setPublicationId(undefined);
    setChannel(undefined);
    setStatus(undefined);
    setStartedAfter("");
    setStartedBefore("");
    setOffset(0);
  };
  const changeFilter = <T,>(setter: (value: T) => void) => (value: T) => {
    setter(value);
    setOffset(0);
  };
  const savePolicy = () => {
    if (!govern() || !governanceDraft) return;
    const {
      trace_retention_days,
      runtime_retention_days,
      attachment_retention_days,
      legal_hold,
      principal_requests_per_minute,
      principal_requests_per_day,
      ip_requests_per_minute,
      ip_requests_per_day,
      publication_requests_per_minute,
      publication_requests_per_day,
      max_agents_per_tenant,
      max_active_publications,
      max_concurrent_runs,
      max_daily_tokens,
      max_daily_mcp_calls,
      max_storage_bytes,
      alert_threshold_percent,
    } = governanceDraft;
    governanceMutation.mutate({
      trace_retention_days,
      runtime_retention_days,
      attachment_retention_days,
      legal_hold,
      principal_requests_per_minute,
      principal_requests_per_day,
      ip_requests_per_minute,
      ip_requests_per_day,
      publication_requests_per_minute,
      publication_requests_per_day,
      max_agents_per_tenant,
      max_active_publications,
      max_concurrent_runs,
      max_daily_tokens,
      max_daily_mcp_calls,
      max_storage_bytes,
      alert_threshold_percent,
    });
  };
  const confirmOperation = () => {
    if (!govern()) return;
    modal.confirm({
      title: t("agents.analytics.confirm.credentialsTitle"),
      content: t("agents.analytics.confirm.credentialsDescription"),
      okText: t("agents.analytics.confirm.credentialsAction"),
      okButtonProps: { danger: true },
      cancelText: t("agents.common.cancel"),
      onOk: () => { if (govern()) return credentialsMutation.mutateAsync(); },
    });
  };

  const traceColumns: TableColumnsType<AgentOperationsTrace> = [
    {
      title: t("agents.analytics.trace"),
      dataIndex: "trace_id",
      width: 170,
      render: (traceId: string) => (
        <Link to={`/eval?tab=traces&family=assistant&trace_id=${encodeURIComponent(traceId)}`}>
          {traceId.slice(0, 8)} <ExternalLink size={12} />
        </Link>
      ),
    },
    { title: t("agents.analytics.channel"), dataIndex: "channel", width: 100, render: (value: string) => <Tag>{value}</Tag> },
    { title: t("agents.analytics.status"), dataIndex: "status", width: 105, render: (value: string) => <Tag color={value === "succeeded" ? "green" : "red"}>{value}</Tag> },
    { title: t("agents.analytics.latency"), dataIndex: "total_latency_ms", width: 105, render: (value: number) => formatMetric(value, " ms") },
    { title: t("agents.analytics.tokens"), dataIndex: "total_tokens", width: 90, render: (value: number) => formatMetric(value) },
    { title: t("agents.analytics.inputPreview"), dataIndex: "input_preview", ellipsis: true, render: preview },
    { title: t("agents.analytics.outputPreview"), dataIndex: "output_preview", ellipsis: true, render: preview },
    { title: t("agents.analytics.started"), dataIndex: "started_at", width: 170, render: (value: string | null) => value ? new Date(value).toLocaleString(locale) : "—" },
  ];
  const auditColumns: TableColumnsType<AgentAuditEvent> = [
    { title: t("agents.analytics.auditAction"), dataIndex: "action" },
    { title: t("agents.analytics.actor"), dataIndex: "user_id", render: (value: string | null) => value || "—" },
    { title: t("agents.analytics.channel"), dataIndex: "channel", render: (value: string | null) => value ? <Tag>{value}</Tag> : "—" },
    { title: t("agents.analytics.status"), dataIndex: "status", render: (value: string) => <Tag>{value}</Tag> },
    { title: t("agents.analytics.created"), dataIndex: "created_at", render: (value: string) => new Date(value).toLocaleString(locale) },
  ];

  const loadError = agentQuery.error || analyticsQuery.error;
  if (loadError && (!agentQuery.data || !analyticsQuery.data)) {
    const detail = agentErrorDetail(loadError);
    return (
      <main className="agent-analytics agent-analytics-state">
        <Alert type={detail.status === 403 ? "warning" : "error"} showIcon title={detail.status === 403 ? t("agents.analytics.forbidden") : t("agents.analytics.loadError")} description={detail.message} action={<Button onClick={() => { void agentQuery.refetch(); void analyticsQuery.refetch(); }}>{t("agents.common.retry")}</Button>} />
      </main>
    );
  }
  if (agentQuery.isLoading || analyticsQuery.isLoading) {
    return <main className="agent-analytics agent-analytics-state" role="status"><Skeleton active paragraph={{ rows: 10 }} /></main>;
  }

  const metrics = analyticsQuery.data?.metrics || {};
  const traces = analyticsQuery.data?.traces || [];
  const policyField = <K extends keyof AgentGovernancePolicy>(key: K, value: AgentGovernancePolicy[K]) => {
    setGovernanceDraft((current) => current ? { ...current, [key]: value } : current);
  };
  return (
    <main className="agent-analytics" data-testid="agent-analytics-page">
      {loadError && <Alert type="error" showIcon title={t("agents.analytics.loadError")} description={agentErrorDetail(loadError).message} />}
      <header className="agent-analytics-header">
        <div>
          <Button type="text" icon={<ArrowLeft size={16} />} onClick={() => navigate(`/agents/${agentId}`)} aria-label={t("agents.analytics.back")} />
          <span><Title level={2}>{agentQuery.data?.name}</Title><Text type="secondary">{t("agents.analytics.subtitle")}</Text></span>
        </div>
        <Button icon={<RefreshCw size={15} />} loading={analyticsQuery.isFetching} onClick={() => { void analyticsQuery.refetch(); if (canGovern) { void auditQuery.refetch(); void governanceQuery.refetch(); } }}>{t("agents.analytics.refresh")}</Button>
      </header>

      <section className="agent-analytics-filters" aria-label={t("agents.analytics.filtersLabel")}>
        <Select allowClear value={versionId} placeholder={t("agents.analytics.allVersions")} aria-label={t("agents.analytics.version")} onChange={changeFilter(setVersionId)} options={(versionsQuery.data || []).map((version) => ({ value: version.agent_version_id, label: `v${version.version_number}` }))} />
        <Select allowClear value={publicationId} placeholder={t("agents.analytics.allPublications")} aria-label={t("agents.analytics.publication")} onChange={changeFilter(setPublicationId)} options={(publicationsQuery.data || []).map((publication) => ({ value: publication.publication_id, label: `${publication.channel} · ${publication.public_id}` }))} />
        <Select allowClear value={channel} placeholder={t("agents.analytics.allChannels")} aria-label={t("agents.analytics.channel")} onChange={changeFilter(setChannel)} options={["preview", "hosted", "embed", "api", "builtin"].map((value) => ({ value, label: value }))} />
        <Select allowClear value={status} placeholder={t("agents.analytics.allStatuses")} aria-label={t("agents.analytics.status")} onChange={changeFilter(setStatus)} options={["succeeded", "failed", "timeout"].map((value) => ({ value, label: value }))} />
        <Input type="date" value={startedAfter} aria-label={t("agents.analytics.startedAfter")} onChange={(event) => changeFilter(setStartedAfter)(event.target.value)} />
        <Input type="date" value={startedBefore} aria-label={t("agents.analytics.startedBefore")} onChange={(event) => changeFilter(setStartedBefore)(event.target.value)} />
        <Button onClick={resetFilters}>{t("agents.analytics.reset")}</Button>
      </section>

      {metrics.retention_limited && <Alert className="agent-retention-alert" type="warning" showIcon title={t("agents.analytics.retentionLimited")} description={t("agents.analytics.retentionLimitedDescription", { days: metrics.retention?.trace_retention_days || 90 })} />}
      <section className="agent-metric-grid" aria-label={t("agents.analytics.metricsLabel")}>
        <article><BarChart3 size={18} /><span>{t("agents.analytics.totalRuns")}</span><strong>{formatMetric(metrics.total_runs)}</strong></article>
        <article><ShieldCheck size={18} /><span>{t("agents.analytics.successRate")}</span><strong>{metrics.success_rate == null ? "—" : `${Math.round(metrics.success_rate * 100)}%`}</strong></article>
        <article><BarChart3 size={18} /><span>{t("agents.analytics.sessions")}</span><strong>{formatMetric(metrics.sessions)}</strong></article>
        <article><DatabaseZap size={18} /><span>{t("agents.analytics.p95Ttft")}</span><strong>{formatMetric(metrics.p95_ttft_ms, " ms")}</strong></article>
        <article><DatabaseZap size={18} /><span>{t("agents.analytics.p95Latency")}</span><strong>{formatMetric(metrics.p95_latency_ms, " ms")}</strong></article>
        <article><BarChart3 size={18} /><span>{t("agents.analytics.totalTokens")}</span><strong>{formatMetric(metrics.total_tokens)}</strong></article>
        <article><ShieldCheck size={18} /><span>{t("agents.analytics.toolSuccess")}</span><strong>{metrics.tool_success_rate == null ? "—" : `${Math.round(metrics.tool_success_rate * 100)}%`}</strong></article>
        <article><DatabaseZap size={18} /><span>{t("agents.analytics.knowledgeHit")}</span><strong>{metrics.knowledge_hit_rate == null ? "—" : `${Math.round(metrics.knowledge_hit_rate * 100)}%`}</strong></article>
        <article><ShieldCheck size={18} /><span>{t("agents.analytics.positiveFeedback")}</span><strong>{metrics.feedback_positive_rate == null ? "—" : `${Math.round(metrics.feedback_positive_rate * 100)}%`}</strong></article>
        <article><BarChart3 size={18} /><span>{t("agents.analytics.totalCost")}</span><strong>{formatMetric(metrics.total_cost_cents, " ¢")}</strong></article>
      </section>
      {(metrics.breakdown?.length || 0) > 0 && <section className="agent-channel-breakdown" aria-label={t("agents.analytics.channelBreakdown")}>
        <Text type="secondary">{t("agents.analytics.channelBreakdown")}</Text>
        {metrics.breakdown?.map((entry, index) => <Tag key={`${String(entry.channel)}-${index}`}>{String(entry.channel || "unknown")} · {formatMetric(Number(entry.run_count || 0))}</Tag>)}
      </section>}

      <Tabs
        className="agent-analytics-tabs"
        items={[
          {
            key: "traces",
            label: t("agents.analytics.traces"),
            children: (
              <section aria-label={t("agents.analytics.traces")}>
                {traces.length === 0 ? <Empty description={t("agents.analytics.noTraces")} /> : <>
                  <div className="agent-trace-table"><Table rowKey="trace_id" size="small" columns={traceColumns} dataSource={traces} pagination={false} scroll={{ x: 1180 }} /></div>
                  <div className="agent-trace-cards">{traces.map((trace) => <article key={trace.trace_id}><header><Link to={`/eval?tab=traces&family=assistant&trace_id=${encodeURIComponent(trace.trace_id)}`}>{trace.trace_id.slice(0, 8)} <ExternalLink size={12} /></Link><Tag color={trace.status === "succeeded" ? "green" : "red"}>{trace.status}</Tag></header><dl><div><dt>{t("agents.analytics.channel")}</dt><dd>{trace.channel}</dd></div><div><dt>{t("agents.analytics.latency")}</dt><dd>{formatMetric(trace.total_latency_ms, " ms")}</dd></div></dl><p>{preview(trace.input_preview)}</p><p>{preview(trace.output_preview)}</p></article>)}</div>
                  <Pagination current={Math.floor(offset / PAGE_SIZE) + 1} pageSize={PAGE_SIZE} total={analyticsQuery.data?.total || 0} showSizeChanger={false} onChange={(page) => setOffset((page - 1) * PAGE_SIZE)} />
                </>}
              </section>
            ),
          },
          {
            key: "audit",
            label: t("agents.analytics.audit"),
            children: canGovern ? (
              auditQuery.error ? <Alert type="error" showIcon title={t("agents.analytics.auditError")} description={agentErrorDetail(auditQuery.error).message} />
                : auditQuery.isLoading ? <Skeleton active />
                  : (auditQuery.data?.events.length || 0) === 0 ? <Empty description={t("agents.analytics.noAudit")} />
                    : <Table rowKey="id" size="small" columns={auditColumns} dataSource={auditQuery.data?.events || []} pagination={false} scroll={{ x: 720 }} />
            ) : <Alert type="info" showIcon title={t("agents.analytics.ownerOnly")} description={t("agents.analytics.ownerOnlyDescription")} />,
          },
          {
            key: "governance",
            label: t("agents.analytics.governance"),
            children: !canGovern ? <Alert type="info" showIcon title={t("agents.analytics.ownerOnly")} description={t("agents.analytics.ownerOnlyDescription")} />
              : governanceQuery.error && !governanceDraft ? <Alert type="error" showIcon title={t("agents.analytics.policyError")} description={agentErrorDetail(governanceQuery.error).message} />
                : !governanceDraft ? <Skeleton active /> : (
                  <div className="agent-governance-grid">
                    {governanceQuery.error && <Alert type="error" showIcon title={t("agents.analytics.policyError")} description={agentErrorDetail(governanceQuery.error).message} />}
                    <section className="agent-policy-card">
                      <header><div><Title level={3}>{t("agents.analytics.retentionTitle")}</Title><Text type="secondary">{t("agents.analytics.retentionDescription")}</Text></div><Button type="primary" icon={<Save size={15} />} loading={governanceMutation.isPending} onClick={savePolicy}>{t("agents.analytics.savePolicy")}</Button></header>
                      <Form layout="vertical" className="agent-policy-form">
                        <Form.Item label={t("agents.analytics.traceRetention")}><InputNumber aria-label={t("agents.analytics.traceRetention")} min={1} max={3650} value={governanceDraft.trace_retention_days} onChange={(value) => policyField("trace_retention_days", value || 1)} /></Form.Item>
                        <Form.Item label={t("agents.analytics.runtimeRetention")}><InputNumber aria-label={t("agents.analytics.runtimeRetention")} min={1} max={3650} value={governanceDraft.runtime_retention_days} onChange={(value) => policyField("runtime_retention_days", value || 1)} /></Form.Item>
                        <Form.Item label={t("agents.analytics.attachmentRetention")}><InputNumber aria-label={t("agents.analytics.attachmentRetention")} min={1} max={3650} value={governanceDraft.attachment_retention_days} onChange={(value) => policyField("attachment_retention_days", value || 1)} /></Form.Item>
                        <Form.Item label={t("agents.analytics.legalHold")}><Switch aria-label={t("agents.analytics.legalHold")} checked={governanceDraft.legal_hold} onChange={(value) => policyField("legal_hold", value)} /></Form.Item>
                      </Form>
                    </section>
                    <section className="agent-policy-card">
                      <header><div><Title level={3}>{t("agents.analytics.quotaTitle")}</Title><Text type="secondary">{t("agents.analytics.quotaDescription")}</Text></div></header>
                      <Form layout="vertical" className="agent-policy-form">
                        <Form.Item label={t("agents.analytics.principalMinute")}><InputNumber aria-label={t("agents.analytics.principalMinute")} min={1} value={governanceDraft.principal_requests_per_minute} onChange={(value) => policyField("principal_requests_per_minute", value || 1)} /></Form.Item>
                        <Form.Item label={t("agents.analytics.principalDay")}><InputNumber aria-label={t("agents.analytics.principalDay")} min={1} value={governanceDraft.principal_requests_per_day} onChange={(value) => policyField("principal_requests_per_day", value || 1)} /></Form.Item>
                        <Form.Item label={t("agents.analytics.ipMinute")}><InputNumber aria-label={t("agents.analytics.ipMinute")} min={1} value={governanceDraft.ip_requests_per_minute} onChange={(value) => policyField("ip_requests_per_minute", value || 1)} /></Form.Item>
                        <Form.Item label={t("agents.analytics.publicationDay")}><InputNumber aria-label={t("agents.analytics.publicationDay")} min={1} value={governanceDraft.publication_requests_per_day} onChange={(value) => policyField("publication_requests_per_day", value || 1)} /></Form.Item>
                        <Form.Item label={t("agents.analytics.maxAgents")}><InputNumber aria-label={t("agents.analytics.maxAgents")} min={1} value={governanceDraft.max_agents_per_tenant} onChange={(value) => policyField("max_agents_per_tenant", value || 1)} /></Form.Item>
                        <Form.Item label={t("agents.analytics.maxPublications")}><InputNumber aria-label={t("agents.analytics.maxPublications")} min={1} value={governanceDraft.max_active_publications} onChange={(value) => policyField("max_active_publications", value || 1)} /></Form.Item>
                        <Form.Item label={t("agents.analytics.maxConcurrentRuns")}><InputNumber aria-label={t("agents.analytics.maxConcurrentRuns")} min={1} value={governanceDraft.max_concurrent_runs} onChange={(value) => policyField("max_concurrent_runs", value || 1)} /></Form.Item>
                        <Form.Item label={t("agents.analytics.maxDailyTokens")}><InputNumber aria-label={t("agents.analytics.maxDailyTokens")} min={1} value={governanceDraft.max_daily_tokens} onChange={(value) => policyField("max_daily_tokens", value || 1)} /></Form.Item>
                        <Form.Item label={t("agents.analytics.maxDailyMcpCalls")}><InputNumber aria-label={t("agents.analytics.maxDailyMcpCalls")} min={1} value={governanceDraft.max_daily_mcp_calls} onChange={(value) => policyField("max_daily_mcp_calls", value || 1)} /></Form.Item>
                        <Form.Item label={t("agents.analytics.maxStorageBytes")}><InputNumber aria-label={t("agents.analytics.maxStorageBytes")} min={1} value={governanceDraft.max_storage_bytes} onChange={(value) => policyField("max_storage_bytes", value || 1)} /></Form.Item>
                        <Form.Item label={t("agents.analytics.alertThreshold")}><InputNumber aria-label={t("agents.analytics.alertThreshold")} min={1} max={100} value={governanceDraft.alert_threshold_percent} onChange={(value) => policyField("alert_threshold_percent", value || 1)} /></Form.Item>
                      </Form>
                    </section>
                    <section className="agent-policy-card agent-danger-zone">
                      <header><div><Title level={3}>{t("agents.analytics.operationsTitle")}</Title><Text type="secondary">{t("agents.analytics.operationsDescription")}</Text></div></header>
                      <div className="agent-operation-list">
                        <Button icon={<DatabaseZap size={15} />} loading={cacheMutation.isPending} onClick={() => cacheMutation.mutate()}>{t("agents.analytics.invalidateCache")}</Button>
                        <Button danger icon={<KeyRound size={15} />} loading={credentialsMutation.isPending} onClick={confirmOperation}>{t("agents.analytics.revokeCredentials")}</Button>
                        <AgentDataDeletionPanel agentId={agentId} allowed={canGovern} legalHold={governanceDraft.legal_hold}
                          onReceipt={async () => { if (govern()) await Promise.all([analyticsQuery.refetch({ throwOnError: true }), auditQuery.refetch({ throwOnError: true })]); }} />
                      </div>
                    </section>
                  </div>
                ),
          },
        ]}
      />
    </main>
  );
}
