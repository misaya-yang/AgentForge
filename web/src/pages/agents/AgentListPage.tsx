import { useLayoutEffect, useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Alert,
  App as AntApp,
  Button,
  Dropdown,
  Empty,
  Input,
  Select,
  Skeleton,
  Table,
  Tag,
  Typography,
  type MenuProps,
  type TableColumnsType,
} from "antd";
import {
  Archive,
  BarChart3,
  Bot,
  ChevronRight,
  Copy,
  MoreHorizontal,
  Plus,
  Search,
  ShieldAlert,
} from "lucide-react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import dayjs from "dayjs";
import relativeTime from "dayjs/plugin/relativeTime";
import "dayjs/locale/zh-cn";
dayjs.extend(relativeTime);
import { useTranslation } from "react-i18next";

import { agentErrorDetail, archiveAgent, copyAgent, listAgents } from "@/api/agents";
import type { AgentStatus, AgentSummary } from "@/types/agents";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { generateUUID } from "@/lib/utils";
import { agentDirectoryLocation, agentDirectoryParams, agentDirectoryNextCursor, AGENT_DIRECTORY_PAGE_SIZE } from "./agentDirectoryNavigation";
import "./agent-studio.css";

const { Text, Title } = Typography;

function statusColor(status: AgentStatus): string {
  if (status === "active") return "green";
  if (status === "archived") return "default";
  if (status === "deleted") return "red";
  return "blue";
}

function AgentActions({
  agent,
  onCopy,
  onArchive,
}: {
  agent: AgentSummary;
  onCopy: (agent: AgentSummary) => void;
  onArchive: (agent: AgentSummary) => void;
}) {
  const navigate = useNavigate();
  const { t } = useTranslation();
  const items: MenuProps["items"] = [
    {
      key: "open",
      label: t("agents.list.openStudio"),
      icon: <ChevronRight size={14} />,
      onClick: () => navigate(`/agents/${agent.agent_id}`),
    },
    {
      key: "analytics",
      label: t("agents.analytics.title"),
      icon: <BarChart3 size={14} />,
      onClick: () => navigate(`/agents/${agent.agent_id}/analytics`),
    },
    { key: "copy", label: t("agents.list.copyAgent"), icon: <Copy size={14} />, disabled: agent.caller_role === "viewer", onClick: () => onCopy(agent) },
    { key: "archive", label: t("agents.list.archive"), icon: <Archive size={14} />, disabled: agent.caller_role !== "owner", onClick: () => onArchive(agent) },
  ];
  return (
    <Dropdown menu={{ items }} trigger={["click"]}>
      <Button
        type="text"
        icon={<MoreHorizontal size={17} />}
        aria-label={t("agents.list.actionsFor", { name: agent.name })}
      />
    </Dropdown>
  );
}

function AgentMobileRow({
  agent,
  onCopy,
  onArchive,
}: {
  agent: AgentSummary;
  onCopy: (agent: AgentSummary) => void;
  onArchive: (agent: AgentSummary) => void;
}) {
  const { t, i18n } = useTranslation();
  const dateLocale = i18n.resolvedLanguage === "zh-CN" ? "zh-cn" : "en";
  return (
    <article className="agent-mobile-row">
      <span className="agent-avatar" aria-hidden><Bot size={18} /></span>
      <Link className="agent-mobile-copy" to={`/agents/${agent.agent_id}`}>
        <strong>{agent.name}</strong>
        <span>{agent.description || t("agents.list.noDescription")}</span>
        <span className="agent-mobile-meta">
          <Tag color={statusColor(agent.status)}>{t(`agents.common.statuses.${agent.status}`)}</Tag>
          <span>{t("agents.list.draftRevision", { revision: agent.draft_revision ?? 0 })}</span>
          <span>{dayjs(agent.updated_at).locale(dateLocale).fromNow()}</span>
        </span>
      </Link>
      <AgentActions agent={agent} onCopy={onCopy} onArchive={onArchive} />
    </article>
  );
}

export function AgentListPage() {
  const token = useAuthStore(state => state.token);
  const authority = useAuthStore(state => sdkAuthorityScope(state.user));
  const authenticated = useAuthStore(state => state.isAuthenticated);
  const [searchParams, setSearchParams] = useSearchParams();
  const [transition, setTransition] = useState(0);
  const scope = useMemo(() => ({ id: generateUUID(), token, authority, transition, invalidated: false }), [token, authority, transition]);
  useLayoutEffect(() => useAuthStore.subscribe((state, previous) => {
    if (state.token !== previous.token || state.isAuthenticated !== previous.isAuthenticated
      || sdkAuthorityScope(state.user) !== sdkAuthorityScope(previous.user)) {
      scope.invalidated = true;
      setTransition(value => value + 1);
      setSearchParams(current => agentDirectoryParams(current, { cursor: undefined }), { replace: true });
    }
  }), [scope, setSearchParams]);
  const isCurrent = () => {
    const state = useAuthStore.getState();
    return Boolean(!scope.invalidated && state.isAuthenticated && state.token && state.user
      && state.token === scope.token && sdkAuthorityScope(state.user) === scope.authority);
  };
  if (!authenticated || !token) return null;
  return <AgentDirectoryContent key={`${scope.id}:${searchParams.toString()}`} scopeId={scope.id} isCurrent={isCurrent} />;
}

function AgentDirectoryContent({ scopeId, isCurrent }: { scopeId: string; isCurrent: () => boolean }) {
  const { t, i18n } = useTranslation();
  const [searchParams, setSearchParams] = useSearchParams();
  const { search, status, owner, cursor } = agentDirectoryLocation(searchParams);
  const [draft, setDraft] = useState({ search, status, owner });
  const hasFilterEdits = draft.search !== search || draft.status !== status || draft.owner !== owner;
  const pageLocation = JSON.stringify({ search, status, owner, cursor });
  const changeFilters = (patch: { search?: string; status?: string; owner?: string }) => {
    if (isCurrent()) setSearchParams(current => JSON.stringify(agentDirectoryLocation(current)) === pageLocation
      ? agentDirectoryParams(current, { ...patch, cursor: undefined }) : current);
  };
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const { message, modal } = AntApp.useApp();
  const query = useQuery({
    queryKey: ["agents", scopeId, { search, status, owner, cursor }],
    queryFn: async ({ signal }) => {
      if (!isCurrent()) throw new Error("Agent directory authorization changed");
      const page = await listAgents({ limit: AGENT_DIRECTORY_PAGE_SIZE, cursor, search: search || undefined, status, owner_id: owner }, signal);
      if (!isCurrent()) throw new Error("Agent directory authorization changed");
      return page;
    },
    enabled: isCurrent(),
    retry: false,
  });
  const error = query.error ? agentErrorDetail(query.error) : null;
  const agents = isCurrent() && !error ? query.data?.items ?? [] : [];
  const nextCursor = !error && !query.isFetching ? agentDirectoryNextCursor(query.data?.next_cursor, cursor) : undefined;
  const filtersActive = Boolean(search || status || owner || cursor);
  const dateLocale = i18n.resolvedLanguage === "zh-CN" ? "zh-cn" : "en";
  const copyMutation = useMutation({
    mutationFn: (agent: AgentSummary) => { if (!isCurrent()) throw new Error("Agent directory authorization changed"); return copyAgent(agent.agent_id); },
    onSuccess: (copied) => {
      if (!isCurrent()) return;
      message.success(t("agents.list.copySuccess"));
      navigate(`/agents/${copied.agent_id}`);
    },
    onError: () => { if (isCurrent()) message.error(t("agents.list.copyError")); },
  });
  const archiveMutation = useMutation({
    mutationFn: (agent: AgentSummary) => { if (!isCurrent()) throw new Error("Agent directory authorization changed"); return archiveAgent(agent.agent_id); },
    onSuccess: async () => {
      if (!isCurrent()) return;
      setSearchParams(current => agentDirectoryParams(current, { cursor: undefined }), { replace: true });
      message.success(t("agents.list.archiveSuccess"));
      await queryClient.invalidateQueries({ queryKey: ["agents"] });
    },
    onError: () => { if (isCurrent()) message.error(t("agents.list.archiveError")); },
  });
  const confirmArchive = (agent: AgentSummary) => {
    modal.confirm({
      title: t("agents.list.archiveConfirmTitle", { name: agent.name }),
      content: t("agents.list.archiveConfirmDescription"),
      okText: t("agents.list.archiveConfirm"),
      okButtonProps: { danger: true },
      cancelText: t("agents.common.cancel"),
      onOk: () => archiveMutation.mutateAsync(agent),
    });
  };

  const columns: TableColumnsType<AgentSummary> = [
    {
      title: t("agents.list.columns.agent"),
      dataIndex: "name",
      key: "agent",
      width: "38%",
      render: (_, agent) => (
        <div className="agent-name-cell">
          <span className="agent-avatar" aria-hidden><Bot size={18} /></span>
          <span className="min-w-0">
            <Link className="agent-name-link" to={`/agents/${agent.agent_id}`}>{agent.name}</Link>
            <Text type="secondary" className="agent-row-description">{agent.description || t("agents.list.noDescription")}</Text>
          </span>
        </div>
      ),
    },
    {
      title: t("agents.list.columns.status"),
      dataIndex: "status",
      width: 118,
      render: (value: AgentStatus) => <Tag color={statusColor(value)}>{t(`agents.common.statuses.${value}`)}</Tag>,
    },
    { title: t("agents.list.columns.owner"), dataIndex: "owner_id", ellipsis: true },
    {
      title: t("agents.list.columns.draft"),
      dataIndex: "draft_revision",
      width: 96,
      render: (value: number | null) => `r${value ?? 0}`,
    },
    {
      title: t("agents.list.columns.updated"),
      dataIndex: "updated_at",
      width: 150,
      render: (value: string) => dayjs(value).locale(dateLocale).fromNow(),
    },
    {
      title: <span className="sr-only">{t("agents.list.columns.actions")}</span>,
      key: "actions",
      align: "right",
      width: 64,
      render: (_, agent) => <AgentActions agent={agent} onCopy={(row) => copyMutation.mutate(row)} onArchive={confirmArchive} />,
    },
  ];

  return (
    <main className="agent-page ui-page" data-testid="agents-page">
      <div className="agent-page-heading">
        <div>
          <Title level={2}>{t("agents.list.title")}</Title>
          <Text type="secondary">{t("agents.list.subtitle")}</Text>
        </div>
        {error?.status !== 403 && (
          <Button
            type="primary"
            size="large"
            icon={<Plus size={17} />}
            onClick={() => navigate("/agents/new")}
          >
            {t("agents.common.createAgent")}
          </Button>
        )}
      </div>

      <section className="agent-directory" aria-label={t("agents.list.directoryLabel")}>
        <div className="agent-toolbar">
          <Input
            allowClear
            prefix={<Search size={16} aria-hidden />}
            placeholder={t("agents.list.searchPlaceholder")}
            value={draft.search}
            maxLength={255}
            onChange={(event) => setDraft(current => ({ ...current, search: event.target.value }))}
            onPressEnter={(event) => { if (!event.nativeEvent.isComposing && hasFilterEdits) changeFilters(draft); }}
            aria-label={t("agents.list.searchLabel")}
          />
          <Select
            allowClear
            placeholder={t("agents.list.allStatuses")}
            value={draft.status}
            onChange={(value) => setDraft(current => ({ ...current, status: value }))}
            aria-label={t("agents.list.filterStatus")}
            options={[
              { value: "draft", label: t("agents.common.statuses.draft") },
              { value: "active", label: t("agents.common.statuses.active") },
              { value: "archived", label: t("agents.common.statuses.archived") },
            ]}
          />
          <Input
            allowClear
            placeholder={t("agents.list.ownerIdPlaceholder")}
            value={draft.owner ?? ""}
            onChange={(event) => setDraft(current => ({ ...current, owner: event.target.value || undefined }))}
            onPressEnter={(event) => { if (!event.nativeEvent.isComposing && hasFilterEdits) changeFilters(draft); }}
            aria-label={t("agents.list.filterOwner")}
          />
          <Button type="primary" disabled={!hasFilterEdits} onClick={() => { if (hasFilterEdits) changeFilters(draft); }}>
            {t("agents.list.applyFilters")}
          </Button>
        </div>

        <p className="px-4 py-2 text-xs text-muted-foreground" role="status">
          {t(hasFilterEdits ? "agents.list.unappliedFilters" : "agents.list.appliedFilters")}
        </p>

        {query.isLoading && (
          <div className="agent-loading" role="status" aria-label={t("agents.list.loading")}>
            <Skeleton active paragraph={{ rows: 6 }} />
          </div>
        )}

        {error?.status === 403 && (
          <div className="agent-permission-state" role="alert">
            <ShieldAlert size={30} />
            <Title level={3}>{t("agents.list.accessTitle")}</Title>
            <Text type="secondary">{t("agents.list.accessDescription")}</Text>
          </div>
        )}

        {error && error.status !== 403 && (
          <Alert
            type="error"
            showIcon
            title={t("agents.list.loadError")}
            description={error.message}
            action={<Button onClick={() => void query.refetch()}>{t("agents.common.retry")}</Button>}
          />
        )}

        {!query.isLoading && !error && agents.length === 0 && (
          <Empty
            className="agent-empty"
            image={Empty.PRESENTED_IMAGE_SIMPLE}
            description={
              filtersActive
                ? t("agents.list.filteredEmpty")
                : t("agents.list.empty")
            }
          >
            {filtersActive ? (
              <Button onClick={() => { changeFilters({ search: "", status: undefined, owner: undefined }); }}>
                {t("agents.list.clearFilters")}
              </Button>
            ) : (
              <>
              <div className="agent-empty-why text-sm text-muted-foreground/70 max-w-md">
                {t(
                  "agents.list.emptyWhy",
                  "Agents package your prompts, tools, and knowledge into a reusable runtime."
                )}
              </div>
              <div className="agent-empty-actions">
                <Button type="primary" icon={<Plus size={16} />} onClick={() => navigate("/agents/new")}>
                  {t("agents.list.createBlank")}
                </Button>
                <Button onClick={() => navigate("/agents/new?template=support")}>{t("agents.list.supportTemplate")}</Button>
                <Button onClick={() => navigate("/agents/new?template=knowledge")}>{t("agents.list.knowledgeTemplate")}</Button>
              </div>
              </>
            )}
          </Empty>
        )}

        {!query.isLoading && !error && agents.length > 0 && (
          <>
            <div className="agent-table-desktop">
              <Table
                rowKey="agent_id"
                columns={columns}
                dataSource={agents}
                pagination={false}
                size="middle"
              />
            </div>
            <div className="agent-list-mobile" aria-label={t("agents.list.mobileLabel")}>
              {agents.map((agent) => (
                <AgentMobileRow
                  key={agent.agent_id}
                  agent={agent}
                  onCopy={(row) => copyMutation.mutate(row)}
                  onArchive={confirmArchive}
                />
              ))}
            </div>

          </>
        )}
        <nav className="agent-directory-footer flex flex-wrap items-center justify-between gap-3" aria-label={t("agents.list.pagination")}>
          <span aria-live="polite">{!query.isLoading && !error && t("agents.list.pageCount", { count: agents.length })}</span>
          <div className="flex gap-2">
            <Button disabled={hasFilterEdits || !cursor || query.isFetching} onClick={() => {
              if (isCurrent() && !hasFilterEdits) setSearchParams(current => agentDirectoryParams(current, { cursor: undefined }));
            }}>{t("agents.list.firstPage")}</Button>
            <Button disabled={hasFilterEdits || !nextCursor || query.isFetching} onClick={() => {
              if (isCurrent() && !hasFilterEdits && nextCursor) setSearchParams(current => JSON.stringify(agentDirectoryLocation(current)) === pageLocation
                ? agentDirectoryParams(current, { cursor: nextCursor }) : current);
            }}>{t("agents.list.nextPage")}</Button>
          </div>
          <span className="w-full">{t("agents.list.paginationHint")}</span>
        </nav>
      </section>
    </main>
  );
}
