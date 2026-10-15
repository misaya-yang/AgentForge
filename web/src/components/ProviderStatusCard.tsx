// web/src/components/ProviderStatusCard.tsx
// Provider table — 1:1 port of design-handoff dashboard.jsx ProviderTable.
// Uses letter avatars, 10-bar health meter, status pills, mono numerics.

import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { Modal, Table, Tag } from "antd";
import { useQuery } from "@tanstack/react-query";
import dayjs from "dayjs";
import { useProvidersHealth } from "@/hooks/useServices";
import { useAppStore } from "@/store/useAppStore";
import { listModels as listAssistantModels, type ModelInfo } from "@/api/assistant";
import type { ProviderStatus } from "@/api/gateway";
import { listModels as listConfiguredModels, type LLMModel } from "@/api/models";
import { listProviders, type Provider as LLMProvider } from "@/api/providers";
import { getUsageBreakdown, type UsageBreakdownItem } from "@/api/usage";
import { useDashboardContext } from "@/pages/dashboard/DashboardContext";
import { FONT_FAMILY, LAYOUT, getColors } from "@/pages/dashboard/styles";

// ── Design-handoff brand avatars — letter tint ────────────────────
const PROVIDER_META: Record<string, { color: string; letter: string }> = {
  openai:     { color: "#62656e", letter: "O" },
  anthropic:  { color: "#b77955", letter: "A" },
  deepseek:   { color: "#5f7396", letter: "D" },
  dashscope:  { color: "#b86e32", letter: "A" }, // 阿里云
  alibaba:    { color: "#b86e32", letter: "A" },
  qwen:       { color: "#b86e32", letter: "Q" },
  doubao:     { color: "#7b8798", letter: "D" },
  volcengine: { color: "#7b8798", letter: "V" },
  moonshot:   { color: "#5f7f9d", letter: "M" },
  zhipu:      { color: "#736984", letter: "Z" },
  siliconflow:{ color: "#566476", letter: "S" },
  minimax:    { color: "#a0783f", letter: "M" },
  xai:        { color: "#6f7c8e", letter: "X" },
  mistral:    { color: "#b85f55", letter: "M" },
  cohere:     { color: "#5f7f9d", letter: "C" },
  google:     { color: "#5b7fb0", letter: "G" },
  gemini:     { color: "#5b7fb0", letter: "G" },
  vertex:     { color: "#6f8db6", letter: "V" },
  "google-vertex": { color: "#6f8db6", letter: "V" },
};

const PROVIDER_DISPLAY_NAMES: Record<string, string> = {
  openai: "OpenAI",
  anthropic: "Anthropic",
  deepseek: "DeepSeek",
  dashscope: "Alibaba DashScope",
  alibaba: "Alibaba",
  qwen: "Qwen",
  google: "Google Gemini",
  gemini: "Google Gemini",
  vertex: "Google Vertex AI",
  "google-vertex": "Google Vertex AI",
  doubao: "Doubao",
  volcengine: "Volcengine",
  moonshot: "Moonshot",
  zhipu: "Zhipu",
  siliconflow: "SiliconFlow",
  minimax: "MiniMax",
  xai: "xAI",
  mistral: "Mistral",
  cohere: "Cohere",
};

const PROVIDER_ENDPOINTS = {
  health: "/api/v1/health/providers",
  models: "/api/v1/models?include_disabled=true",
  providers: "/api/v1/providers?include_disabled=true",
  assistantModels: "/api/v1/assistant/models",
} as const;

const ICON = {
  refresh: (
    <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
      <path d="M12 7a5 5 0 11-1.5-3.5M12 2v2h-2" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  ),
  mini: (
    <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
      <path d="M2 10l3-4 2.5 2 3-4 1.5 2" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  ),
  more: (
    <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
      <circle cx="3" cy="7" r="1.1" fill="currentColor" />
      <circle cx="7" cy="7" r="1.1" fill="currentColor" />
      <circle cx="11" cy="7" r="1.1" fill="currentColor" />
    </svg>
  ),
};

interface Model {
  model_id: string;
  provider_id: string;
  display_name: string;
  context_window: number;
  max_output_tokens: number;
  supports_vision: boolean;
  supports_tools: boolean;
  input_price_per_1k: number | string | null | undefined;
  output_price_per_1k: number | string | null | undefined;
  is_enabled: boolean;
  source: "configured" | "assistant";
}

type ProviderDataSource = "health" | "providers" | "models" | "assistant_models";

interface ProviderDisplayStatus extends Omit<ProviderStatus, "model_count" | "last_check"> {
  model_count: number;
  enabled_model_count: number;
  last_check?: string;
  sources: ProviderDataSource[];
}

type EndpointState = "loading" | "ok" | "empty" | "unauthorized" | "error";

interface EndpointDiagnostic {
  endpoint: string;
  state: EndpointState;
  count?: number;
  detail?: string;
}

function normalizeProviderKey(value: string | null | undefined) {
  const key = String(value || "").trim().toLowerCase();
  return key || "unknown";
}

function fallbackProviderName(providerKey: string) {
  return PROVIDER_DISPLAY_NAMES[providerKey] || providerKey;
}

function addSource(row: ProviderDisplayStatus, source: ProviderDataSource) {
  if (!row.sources.includes(source)) row.sources.push(source);
}

function ensureProviderRow(
  rows: Record<string, ProviderDisplayStatus>,
  providerKey: string,
  source: ProviderDataSource,
  name?: string | null
) {
  const key = normalizeProviderKey(providerKey);
  const fallback = fallbackProviderName(key);

  if (!rows[key]) {
    rows[key] = {
      name: name || fallback,
      status: "not_configured",
      configured: false,
      model_count: 0,
      enabled_model_count: 0,
      last_check: undefined,
      sources: [],
    };
  } else if (name && rows[key].name === fallback) {
    rows[key].name = name;
  }

  addSource(rows[key], source);
  return rows[key];
}

function configuredToModel(model: LLMModel): Model {
  return {
    model_id: model.model_id,
    provider_id: normalizeProviderKey(model.provider_id),
    display_name: model.display_name || model.model_id,
    context_window: Number(model.context_window) || 0,
    max_output_tokens: Number(model.max_output_tokens) || 0,
    supports_vision: Boolean(model.supports_vision),
    supports_tools: Boolean(model.supports_tools),
    input_price_per_1k: model.input_price_per_1k,
    output_price_per_1k: model.output_price_per_1k,
    is_enabled: model.is_enabled !== false,
    source: "configured",
  };
}

function assistantToModel(model: ModelInfo): Model {
  return {
    model_id: model.id,
    provider_id: normalizeProviderKey(model.provider),
    display_name: model.name || model.id,
    context_window: Number(model.context_window) || 0,
    max_output_tokens: Number(model.max_output_tokens) || 0,
    supports_vision: Boolean(model.supports_vision),
    supports_tools: Boolean(model.supports_tools),
    input_price_per_1k: model.input_price_per_1k,
    output_price_per_1k: model.output_price_per_1k,
    is_enabled: true,
    source: "assistant",
  };
}

function mergeModels(configuredModels: LLMModel[] = [], assistantModels: ModelInfo[] = []) {
  const merged = new Map<string, Model>();

  configuredModels.forEach((model) => {
    const item = configuredToModel(model);
    merged.set(`${item.provider_id}:${item.model_id}`, item);
  });

  assistantModels.forEach((model) => {
    const item = assistantToModel(model);
    const key = `${item.provider_id}:${item.model_id}`;
    if (!merged.has(key)) merged.set(key, item);
  });

  return Array.from(merged.values()).sort((a, b) => {
    if (a.provider_id !== b.provider_id) return a.provider_id.localeCompare(b.provider_id);
    return a.display_name.localeCompare(b.display_name);
  });
}

function buildProviderRows(
  healthProviders?: Record<string, ProviderStatus>,
  providerConfigs: LLMProvider[] = [],
  configuredModels: LLMModel[] = [],
  assistantModels: ModelInfo[] = []
) {
  const rows: Record<string, ProviderDisplayStatus> = {};

  Object.entries(healthProviders || {}).forEach(([providerKey, provider]) => {
    const row = ensureProviderRow(rows, providerKey, "health", provider.name);
    row.configured = row.configured || Boolean(provider.configured);
    row.status = provider.status;
    row.model_count = Math.max(row.model_count, Number(provider.model_count) || 0);
    row.enabled_model_count = Math.max(row.enabled_model_count, Number(provider.model_count) || 0);
    row.last_check = provider.last_check || row.last_check;
  });

  providerConfigs.forEach((provider) => {
    const row = ensureProviderRow(rows, provider.provider_id, "providers", provider.display_name);
    row.configured = row.configured || Boolean(
      provider.is_enabled !== false &&
      (provider.has_api_key || provider.allow_environment_credentials)
    );
    if (!row.sources.includes("health")) row.status = row.configured ? "unverified" : "not_configured";
  });

  const modelCounts = new Map<string, { all: number; enabled: number; sources: Set<ProviderDataSource> }>();
  mergeModels(configuredModels, assistantModels).forEach((model) => {
    const counts = modelCounts.get(model.provider_id) || { all: 0, enabled: 0, sources: new Set<ProviderDataSource>() };
    counts.all += 1;
    if (model.is_enabled) counts.enabled += 1;
    counts.sources.add(model.source === "assistant" ? "assistant_models" : "models");
    modelCounts.set(model.provider_id, counts);
  });

  modelCounts.forEach((counts, providerKey) => {
    const row = ensureProviderRow(rows, providerKey, counts.sources.has("assistant_models") ? "assistant_models" : "models");
    counts.sources.forEach((source) => addSource(row, source));
    row.model_count = Math.max(row.model_count, counts.all);
    row.enabled_model_count = Math.max(row.enabled_model_count, counts.enabled);
  });

  return rows;
}

function getHttpStatus(error: unknown) {
  return (error as { response?: { status?: number } } | undefined)?.response?.status;
}

function getErrorDetail(error: unknown) {
  const status = getHttpStatus(error);
  if (status) return `HTTP ${status}`;
  const message = (error as { message?: string } | undefined)?.message;
  return message ? "request failed" : undefined;
}

function getEndpointDiagnostic(
  endpoint: string,
  isLoading: boolean,
  error: unknown,
  count: number | undefined
): EndpointDiagnostic {
  if (isLoading) return { endpoint, state: "loading" };
  if (error) {
    const status = getHttpStatus(error);
    return {
      endpoint,
      state: status === 401 || status === 403 ? "unauthorized" : "error",
      detail: getErrorDetail(error),
    };
  }
  return {
    endpoint,
    state: count && count > 0 ? "ok" : "empty",
    count: count || 0,
  };
}

function formatCompactTokens(value: number | string | null | undefined) {
  const num = Number(value);
  if (!Number.isFinite(num) || num <= 0) return "—";
  if (num >= 1_000_000) return `${(num / 1_000_000).toFixed(1)}M`;
  if (num >= 1000) return `${(num / 1000).toFixed(0)}K`;
  return String(num);
}

function formatPrice(value: number | string | null | undefined) {
  const num = Number(value);
  if (!Number.isFinite(num)) return "—";
  if (num === 0) return "$0";
  if (num < 0.001) return `$${num.toFixed(6)}`;
  if (num < 0.01) return `$${num.toFixed(5)}`;
  return `$${num.toFixed(4)}`;
}

function endpointStateLabel(item: EndpointDiagnostic, t: TFunction) {
  if (item.state === "loading") return t("services.providersStatus.endpointStates.checking");
  if (item.state === "ok") return t("services.providersStatus.endpointStates.records", { count: item.count ?? 0 });
  if (item.state === "empty") return t("services.providersStatus.endpointStates.empty");
  if (item.state === "unauthorized") return item.detail ? `${t("services.providersStatus.endpointStates.unauthorized")} (${item.detail})` : t("services.providersStatus.endpointStates.unauthorized");
  return item.detail ? `${t("services.providersStatus.endpointStates.error")} (${item.detail})` : t("services.providersStatus.endpointStates.error");
}

function providerSourceLabel(sources: ProviderDataSource[], t: TFunction) {
  if (sources.includes("health")) return t("services.providersStatus.sources.health");
  if (sources.includes("assistant_models")) return t("services.providersStatus.sources.assistantModels");
  if (sources.includes("models")) return t("services.providersStatus.sources.models");
  if (sources.includes("providers")) return t("services.providersStatus.sources.providers");
  return t("services.providersStatus.sources.unknown");
}

function ProviderProbe({ status, lastCheck }: { status: ProviderStatus["status"]; lastCheck?: string }) {
  const { i18n } = useTranslation();
  const zh = i18n.language.startsWith("zh");
  const labels = {
    healthy: zh ? "连接已验证" : "Connection verified",
    unhealthy: zh ? "连接失败" : "Connection failed",
    stale: zh ? "探测已过期" : "Probe stale",
    unverified: zh ? "尚未探测" : "Not probed",
    configured: zh ? "尚未探测" : "Not probed",
    not_configured: zh ? "未配置" : "Not configured",
  };
  return <span title={lastCheck || undefined}>{labels[status]}</span>;
}

// ── Status badge ───────────────────────────────────────────────────
function StatusBadge({ on, tOn, tOff }: { on: boolean; tOn: string; tOff: string }) {
  const { darkMode } = useAppStore();
  const c = getColors(darkMode);
  if (on) {
    return (
      <span style={{
        display: "inline-flex", alignItems: "center", gap: 5,
        padding: "3px 9px", borderRadius: 5,
        background: c.successBg, color: c.success,
        fontSize: 11.5, fontWeight: 600,
      }}>
        <span style={{ width: 5, height: 5, borderRadius: "50%", background: c.success }} />
        {tOn}
      </span>
    );
  }
  return (
    <span style={{
      display: "inline-flex", alignItems: "center",
      padding: "3px 9px", borderRadius: 5,
      background: c.cardHover, color: c.textMuted,
      fontSize: 11.5, fontWeight: 500,
    }}>
      {tOff}
    </span>
  );
}

function ReadinessBadge({ state, label }: { state: "ready" | "warning" | "off"; label: string }) {
  const { darkMode } = useAppStore();
  const c = getColors(darkMode);
  const tone = state === "ready"
    ? { fg: c.success, bg: c.successBg }
    : state === "warning"
    ? { fg: c.warning, bg: c.warningBg }
    : { fg: c.textMuted, bg: c.cardHover };

  return (
    <span style={{
      display: "inline-flex",
      alignItems: "center",
      gap: 5,
      padding: "3px 9px",
      borderRadius: 5,
      background: tone.bg,
      color: tone.fg,
      fontSize: 11.5,
      fontWeight: 650,
      whiteSpace: "nowrap",
    }}>
      <span style={{ width: 5, height: 5, borderRadius: "50%", background: tone.fg }} />
      {label}
    </span>
  );
}

// ── Provider avatar — letter in a tinted rounded square ───────────
function ProviderAvatar({ color, letter }: { color: string; letter: string }) {
  return (
    <div style={{
      width: 26, height: 26, borderRadius: 7,
      background: `${color}18`,
      border: `1px solid ${color}38`,
      display: "flex", alignItems: "center", justifyContent: "center",
      color, fontSize: 11, fontWeight: 700, flexShrink: 0,
    }}>
      {letter}
    </div>
  );
}

// ── Detail Modal ───────────────────────────────────────────────────
function ProviderDetailModal({
  open, onClose, providerKey, providerName,
}: {
  open: boolean; onClose: () => void; providerKey: string; providerName: string;
}) {
  const { t } = useTranslation();
  const { darkMode } = useAppStore();
  const c = getColors(darkMode);
  const meta = PROVIDER_META[providerKey.toLowerCase()] || { color: c.accent, letter: providerKey[0]?.toUpperCase() || "?" };

  const configuredModelsQuery = useQuery({
    queryKey: ["models", providerKey],
    queryFn: () => listConfiguredModels(providerKey, true),
    enabled: open && Boolean(providerKey),
  });

  const assistantModelsQuery = useQuery({
    queryKey: ["assistant-models", providerKey],
    queryFn: listAssistantModels,
    enabled: open && Boolean(providerKey),
  });

  const models = useMemo(() => {
    const selectedProvider = normalizeProviderKey(providerKey);
    return mergeModels(configuredModelsQuery.data, assistantModelsQuery.data)
      .filter((model) => model.provider_id === selectedProvider);
  }, [assistantModelsQuery.data, configuredModelsQuery.data, providerKey]);

  const isLoading = configuredModelsQuery.isLoading || assistantModelsQuery.isLoading;

  const columns = [
    {
      title: t("services.providersStatus.table.name"),
      dataIndex: "display_name",
      key: "display_name",
      render: (text: string, record: Model) => (
        <div>
          <div style={{ fontWeight: 500, color: c.textPrimary }}>{text}</div>
          <div style={{ fontSize: 11, color: c.textMuted, fontFamily: FONT_FAMILY.mono }}>{record.model_id}</div>
        </div>
      ),
    },
    {
      title: t("services.providersStatus.table.context"),
      dataIndex: "context_window",
      key: "context_window",
      width: 90,
      render: (val: number) => (
        <span style={{ fontFamily: FONT_FAMILY.mono, fontSize: 12 }}>{formatCompactTokens(val)}</span>
      ),
    },
    {
      title: t("services.providersStatus.table.capabilities"),
      key: "capabilities",
      width: 130,
      render: (_: unknown, record: Model) => (
        <div style={{ display: "flex", gap: 4, flexWrap: "wrap" }}>
          {record.supports_vision && <Tag style={{ margin: 0, fontSize: 10, background: c.accentBg, color: c.accent, border: "none" }}>{t("services.providersStatus.table.vision")}</Tag>}
          {record.supports_tools && <Tag style={{ margin: 0, fontSize: 10, background: c.successBg, color: c.success, border: "none" }}>{t("services.providersStatus.table.tools")}</Tag>}
        </div>
      ),
    },
    {
      title: t("services.providersStatus.table.price"),
      key: "price",
      width: 150,
      render: (_: unknown, record: Model) => (
        <div style={{ fontSize: 11, fontFamily: FONT_FAMILY.mono, color: c.textSecondary }}>
          <div>in {formatPrice(record.input_price_per_1k)}</div>
          <div>out {formatPrice(record.output_price_per_1k)}</div>
        </div>
      ),
    },
    {
      title: t("services.providersStatus.table.status"),
      dataIndex: "is_enabled",
      key: "is_enabled",
      width: 90,
      render: (enabled: boolean) => (
        <StatusBadge on={enabled}
          tOn={t("services.providersStatus.table.enabled", "已启用")}
          tOff={t("services.providersStatus.table.disabled", "已停用")}
        />
      ),
    },
  ];

  return (
    <Modal
      title={
        <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
          <ProviderAvatar color={meta.color} letter={meta.letter} />
          <span style={{ fontWeight: 600, letterSpacing: "-0.01em" }}>
            {t("services.providersStatus.modal.title", { provider: providerName })}
          </span>
        </div>
      }
      open={open}
      onCancel={onClose}
      footer={null}
      width={820}
      styles={{ body: { padding: "16px 0" } }}
    >
      <Table
        dataSource={models || []}
        columns={columns}
        rowKey="model_id"
        loading={isLoading}
        pagination={false}
        size="small"
        scroll={{ y: 420 }}
      />
    </Modal>
  );
}

// ── Public table ───────────────────────────────────────────────────
export function ProviderStatusCard() {
  const { t, i18n } = useTranslation();
  const { darkMode } = useAppStore();
  const c = getColors(darkMode);
  const {
    data: providers,
    isLoading,
    refetch,
    dataUpdatedAt,
    error: providersError,
  } = useProvidersHealth();
  const configuredModelsQuery = useQuery({
    queryKey: ["provider-status-models"],
    queryFn: () => listConfiguredModels(undefined, true),
    staleTime: 30000,
    refetchInterval: 60000,
  });
  const providerConfigsQuery = useQuery({
    queryKey: ["provider-status-provider-configs"],
    queryFn: () => listProviders(true),
    staleTime: 30000,
    refetchInterval: 60000,
  });
  const assistantModelsQuery = useQuery({
    queryKey: ["provider-status-assistant-models"],
    queryFn: listAssistantModels,
    staleTime: 30000,
    refetchInterval: 60000,
  });
  const [selected, setSelected] = useState<{ key: string; name: string } | null>(null);

  const { dateRange, serviceId, userId, lastRefresh } = useDashboardContext();
  const { data: usageByProvider } = useQuery({
    queryKey: ["provider-usage", dateRange, serviceId, userId, lastRefresh.getTime()],
    queryFn: () => getUsageBreakdown({
      start_date: dateRange[0],
      end_date: dateRange[1],
      dimension: "provider",
      service_id: serviceId !== "all" ? serviceId : undefined,
      user_id: userId !== "all" ? userId : undefined,
    }),
    staleTime: 30000,
  });

  const usageMap = useMemo(() => {
    const m: Record<string, UsageBreakdownItem> = {};
    (usageByProvider?.items ?? []).forEach((it) => {
      const key = (it.provider || "").toLowerCase();
      if (key) m[key] = it;
    });
    return m;
  }, [usageByProvider]);

  const usageItems = usageByProvider?.items ?? [];
  const unattributedUsage = usageItems.find((item) => {
    const provider = (item.provider || "").toLowerCase();
    return provider.startsWith("unattributed") || provider === "unknown" || provider === "none";
  });

  const providerRows = useMemo(() => buildProviderRows(
    providers,
    providerConfigsQuery.data,
    configuredModelsQuery.data,
    assistantModelsQuery.data
  ), [assistantModelsQuery.data, configuredModelsQuery.data, providerConfigsQuery.data, providers]);

  const list = useMemo(() => Object.entries(providerRows).sort(([ka, a], [kb, b]) => {
    if (a.configured && !b.configured) return -1;
    if (!a.configured && b.configured) return 1;
    const ua = usageMap[ka.toLowerCase()]?.requests || 0;
    const ub = usageMap[kb.toLowerCase()]?.requests || 0;
    if (ua !== ub) return ub - ua;
    return a.name.localeCompare(b.name);
  }), [providerRows, usageMap]);

  const endpointDiagnostics = useMemo(() => [
    getEndpointDiagnostic(PROVIDER_ENDPOINTS.health, isLoading, providersError, providers ? Object.keys(providers).length : undefined),
    getEndpointDiagnostic(PROVIDER_ENDPOINTS.models, configuredModelsQuery.isLoading, configuredModelsQuery.error, configuredModelsQuery.data?.length),
    getEndpointDiagnostic(PROVIDER_ENDPOINTS.providers, providerConfigsQuery.isLoading, providerConfigsQuery.error, providerConfigsQuery.data?.length),
    getEndpointDiagnostic(PROVIDER_ENDPOINTS.assistantModels, assistantModelsQuery.isLoading, assistantModelsQuery.error, assistantModelsQuery.data?.length),
  ], [
    assistantModelsQuery.data?.length,
    assistantModelsQuery.error,
    assistantModelsQuery.isLoading,
    configuredModelsQuery.data?.length,
    configuredModelsQuery.error,
    configuredModelsQuery.isLoading,
    isLoading,
    providerConfigsQuery.data?.length,
    providerConfigsQuery.error,
    providerConfigsQuery.isLoading,
    providers,
    providersError,
  ]);

  const isLoadingAny = isLoading || configuredModelsQuery.isLoading || providerConfigsQuery.isLoading || assistantModelsQuery.isLoading;
  const latestDataUpdatedAt = Math.max(
    dataUpdatedAt || 0,
    configuredModelsQuery.dataUpdatedAt || 0,
    providerConfigsQuery.dataUpdatedAt || 0,
    assistantModelsQuery.dataUpdatedAt || 0
  );
  const refreshAll = () => {
    void refetch();
    void configuredModelsQuery.refetch();
    void providerConfigsQuery.refetch();
    void assistantModelsQuery.refetch();
  };

  const totalModels = list.reduce((sum, [, p]) => sum + (p.model_count || 0), 0);
  const configuredProviders = list.filter(([, p]) => p.configured).length;
  const readyProviders = list.filter(([, p]) => p.configured && p.enabled_model_count > 0).length;
  const integrationGaps = list.length - readyProviders + (unattributedUsage ? 1 : 0);
  const cols = "2fr 0.9fr 0.9fr 1fr 1.6fr 1.1fr 0.9fr 0.9fr";
  const pad = "13px 18px";
  if (list.length === 0) {
    return (
      <section style={{
        background: c.cardBg,
        borderRadius: LAYOUT.CARD_RADIUS,
        border: `1px solid ${c.borderSoft}`,
        padding: "14px 16px",
        fontFamily: FONT_FAMILY.sans,
        overflow: "hidden",
        minHeight: "100%",
      }}>
        <div style={{ display: "flex", alignItems: "center", marginBottom: 12 }}>
          <span style={{ fontSize: 13.5, fontWeight: 650, color: c.textPrimary }}>
            {t("services.providersStatus.title", "厂商状态")}
          </span>
          <div style={{ flex: 1 }} />
          <button
            onClick={refreshAll}
            aria-label="refresh"
            style={rowBtn(c)}
            onMouseEnter={(e) => { e.currentTarget.style.background = c.cardHover; e.currentTarget.style.borderColor = c.border; }}
            onMouseLeave={(e) => { e.currentTarget.style.background = c.cardBg; e.currentTarget.style.borderColor = c.borderSoft; }}
          >
            {ICON.refresh}
          </button>
        </div>

        <div
          style={{
            display: "grid",
            gridTemplateColumns: "minmax(260px, 1.1fr) minmax(320px, 1.4fr)",
            gap: 12,
          }}
        >
          <div style={{ padding: 14, borderRadius: 8, background: c.innerBg, border: `1px solid ${c.warning}33` }}>
            <div style={{ color: c.warning, fontSize: 12, fontWeight: 700, marginBottom: 6 }}>
              {isLoadingAny
                ? t("services.providersStatus.diagnostic.checking")
                : t("services.providersStatus.diagnostic.noRecords")}
            </div>
            <div style={{ color: c.textSecondary, fontSize: 12, lineHeight: 1.6 }}>
              {isLoadingAny
                ? t("services.providersStatus.diagnostic.checkingDesc")
                : t("services.providersStatus.diagnostic.noRecordsDesc")}
            </div>
            {unattributedUsage && (
              <div style={{ marginTop: 10, padding: "8px 10px", borderRadius: 6, background: c.warningBg, color: c.warning, fontSize: 12, fontWeight: 650 }}>
                {t("services.providersStatus.diagnostic.unattributed")} · {unattributedUsage.requests} req · ${Number(unattributedUsage.cost_usd || 0).toFixed(4)}
              </div>
            )}
          </div>

          <div style={{ padding: 14, borderRadius: 8, background: c.innerBg, border: `1px solid ${c.borderSoft}` }}>
            <div style={{ color: c.textPrimary, fontSize: 12, fontWeight: 700, marginBottom: 10 }}>
              {t("services.providersStatus.diagnostic.endpointTitle")}
            </div>
            <div style={{ display: "grid", gap: 8 }}>
              {endpointDiagnostics.map((item) => (
                <div
                  key={item.endpoint}
                  style={{
                    display: "grid",
                    gridTemplateColumns: "minmax(0, 1fr) auto",
                    gap: 10,
                    alignItems: "center",
                    padding: "7px 9px",
                    borderRadius: 7,
                    border: `1px solid ${c.borderSoft}`,
                    background: c.cardBg,
                    color: c.textSecondary,
                    fontSize: 12,
                  }}
                >
                  <span style={{ fontFamily: FONT_FAMILY.mono, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                    {item.endpoint}
                  </span>
                  <span style={{
                    color: item.state === "ok" ? c.success : item.state === "loading" ? c.textMuted : item.state === "empty" ? c.warning : c.error,
                    fontWeight: 650,
                    whiteSpace: "nowrap",
                  }}>
                    {endpointStateLabel(item, t)}
                  </span>
                </div>
              ))}
            </div>
          </div>
        </div>
      </section>
    );
  }

  return (
    <section style={{
      background: c.cardBg,
      borderRadius: LAYOUT.CARD_RADIUS,
      border: `1px solid ${c.borderSoft}`,
      padding: "16px 18px",
      fontFamily: FONT_FAMILY.sans,
      overflow: "hidden",
      height: "100%",
      display: "flex",
      flexDirection: "column",
    }}>
      {/* Header */}
      <div style={{ display: "flex", alignItems: "center", marginBottom: 8 }}>
        <span style={{ fontSize: 13.5, fontWeight: 600, color: c.textPrimary }}>
          {t("services.providersStatus.title", "厂商状态")}
        </span>
        <div style={{ flex: 1 }} />
        <div style={{ display: "flex", alignItems: "center", gap: 12, fontSize: 11.5, color: c.textSecondary }}>
          <span style={{ fontFamily: FONT_FAMILY.mono, fontFeatureSettings: '"tnum"' }}>
            {t("dashboard.trend.refreshedAt", i18n.language.startsWith("zh") ? "更新于" : "Updated")}
            {" "}
            {latestDataUpdatedAt > 0 ? dayjs(latestDataUpdatedAt).format("HH:mm:ss") : "—"}
          </span>
          <span style={{
            display: "inline-flex", alignItems: "center", gap: 5,
            padding: "3px 9px", borderRadius: 5,
            background: c.successBg, color: c.success,
            fontWeight: 600, fontSize: 11,
          }}>
            <span style={{ width: 5, height: 5, borderRadius: "50%", background: c.success }} />
            {configuredProviders}/{list.length} {t("services.providersStatus.configuredCount")}
          </span>
          <span style={{
            display: "inline-flex", alignItems: "center", gap: 5,
            padding: "3px 9px", borderRadius: 5,
            background: c.accentBg, color: c.accent,
            fontWeight: 600, fontSize: 11,
          }}>
            <span style={{ width: 5, height: 5, borderRadius: "50%", background: c.accent }} />
            {totalModels} {t("services.providersStatus.modelUnit", i18n.language.startsWith("zh") ? "个模型" : "models")}
          </span>
          {integrationGaps > 0 && (
            <span style={{
              display: "inline-flex", alignItems: "center", gap: 5,
              padding: "3px 9px", borderRadius: 5,
              background: c.warningBg, color: c.warning,
              fontWeight: 650, fontSize: 11,
            }}>
              <span style={{ width: 5, height: 5, borderRadius: "50%", background: c.warning }} />
              {integrationGaps} {t("services.providersStatus.gaps")}
            </span>
          )}
          <button
            onClick={refreshAll}
            aria-label="refresh"
            style={{
              width: 30, height: 30, borderRadius: 6,
              border: `1px solid ${c.borderSoft}`,
              background: c.cardBg,
              color: c.textSecondary,
              display: "flex", alignItems: "center", justifyContent: "center",
              cursor: "pointer", transition: "color .12s ease-out, background-color .12s ease-out, border-color .12s ease-out",
            }}
            onMouseEnter={(e) => {
              e.currentTarget.style.background = c.cardHover;
              e.currentTarget.style.borderColor = c.border;
              e.currentTarget.style.color = c.textPrimary;
            }}
            onMouseLeave={(e) => {
              e.currentTarget.style.background = c.cardBg;
              e.currentTarget.style.borderColor = c.borderSoft;
              e.currentTarget.style.color = c.textSecondary;
            }}
          >
            {ICON.refresh}
          </button>
        </div>
      </div>

      {/* Table grid — negative margin so lines go edge-to-edge */}
      <div style={{ margin: "0 -18px -16px", borderTop: `1px solid ${c.divider}`, flex: 1, overflow: "auto", minHeight: 0 }}>
        {/* Header row */}
        <div style={{
          display: "grid", gridTemplateColumns: cols,
          padding: "10px 18px", borderBottom: `1px solid ${c.divider}`,
          fontSize: 11, color: c.textMuted, fontWeight: 500,
        }}>
          <span>{t("services.providersStatus.col.provider", i18n.language.startsWith("zh") ? "厂商 / 平台" : "Provider")}</span>
          <span>{t("services.providersStatus.col.models", i18n.language.startsWith("zh") ? "模型数量" : "Models")}</span>
          <span>{t("services.providersStatus.col.enabled", i18n.language.startsWith("zh") ? "已启用" : "Enabled")}</span>
          <span>{t("services.providersStatus.col.status", i18n.language.startsWith("zh") ? "状态" : "Status")}</span>
          <span>{t("services.providersStatus.col.health", i18n.language.startsWith("zh") ? "健康度" : "Health")}</span>
          <span>{t("services.providersStatus.col.latency", i18n.language.startsWith("zh") ? "平均延迟 (ms)" : "Avg Latency (ms)")}</span>
          <span>{t("services.providersStatus.col.success", i18n.language.startsWith("zh") ? "成功率" : "Success")}</span>
          <span style={{ textAlign: "right" }}>{t("services.providersStatus.col.actions", i18n.language.startsWith("zh") ? "操作" : "Actions")}</span>
        </div>

        {isLoadingAny && list.length === 0 ? (
          Array.from({ length: 4 }).map((_, i) => (
            <div key={`sk-${i}`} style={{
              display: "grid", gridTemplateColumns: cols,
              padding: pad, alignItems: "center",
              borderBottom: `1px solid ${c.divider}`,
            }}>
              {Array.from({ length: 8 }).map((_, j) => (
                <div key={j} className="animate-shimmer" style={{ height: 12, width: j === 0 ? 160 : 60, borderRadius: 3 }} />
              ))}
            </div>
          ))
        ) : list.map(([key, p], i) => {
          const meta = PROVIDER_META[key.toLowerCase()] || {
            color: c.textMuted,
            letter: (p.name?.[0] || key[0] || "?").toUpperCase(),
          };
          const on = p.configured;
          const ready = p.configured && p.enabled_model_count > 0;
          const isLast = i === list.length - 1;

          return (
            <div
              key={key}
              onClick={on ? () => setSelected({ key, name: p.name }) : undefined}
              className="provider-row"
              style={{
                display: "grid", gridTemplateColumns: cols,
                padding: pad, alignItems: "center",
                borderBottom: isLast ? "none" : `1px solid ${c.divider}`,
                fontSize: 12.5, color: c.textPrimary,
                cursor: on ? "pointer" : "default",
                transition: "background .12s",
              }}
            >
              <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                <ProviderAvatar color={meta.color} letter={meta.letter} />
                <span>
                  <span style={{ display: "block", fontWeight: 500 }}>{p.name}</span>
                  <span style={{ display: "block", marginTop: 2, color: c.textMuted, fontSize: 11 }}>
                    {providerSourceLabel(p.sources, t)}
                  </span>
                </span>
              </div>
              <span style={{
                color: p.model_count > 0 ? c.textPrimary : c.textFaint,
                fontFeatureSettings: '"tnum"',
              }}>
                {p.model_count > 0 ? p.model_count : "—"}
              </span>
              <span style={{
                color: p.enabled_model_count > 0 ? c.textPrimary : c.textFaint,
                fontFeatureSettings: '"tnum"',
              }}>
                {p.enabled_model_count > 0 ? p.enabled_model_count : "—"}
              </span>
              <span>
                <ReadinessBadge
                  state={ready ? "ready" : on ? "warning" : "off"}
                  label={
                    ready
                      ? t("services.providersStatus.table.enabled", "已启用")
                      : on
                      ? t("services.providersStatus.noModels")
                      : t("services.providersStatus.unconfigured", i18n.language.startsWith("zh") ? "未配置" : "Unconfigured")
                  }
                />
              </span>
              <span><ProviderProbe status={p.status} lastCheck={p.last_check} /></span>
              <span style={{
                color: c.textFaint,
                fontFamily: FONT_FAMILY.mono,
                fontSize: 12,
              }}>
                —
              </span>
              <span style={{
                color: c.textFaint,
                fontFeatureSettings: '"tnum"',
              }}>
                —
              </span>
              <div style={{ display: "flex", gap: 6, justifyContent: "flex-end" }}>
                {on && (
                  <button
                    onClick={(e) => { e.stopPropagation(); setSelected({ key, name: p.name }); }}
                    aria-label="trend"
                    style={rowBtn(c)}
                    onMouseEnter={(e) => { e.currentTarget.style.borderColor = c.border; e.currentTarget.style.color = c.accent; }}
                    onMouseLeave={(e) => { e.currentTarget.style.borderColor = c.borderSoft; e.currentTarget.style.color = c.textSecondary; }}
                  >
                    {ICON.mini}
                  </button>
                )}
                <button
                  onClick={(e) => { e.stopPropagation(); if (on) setSelected({ key, name: p.name }); }}
                  aria-label="more"
                  style={rowBtn(c)}
                  onMouseEnter={(e) => { e.currentTarget.style.borderColor = c.border; e.currentTarget.style.color = c.textPrimary; }}
                  onMouseLeave={(e) => { e.currentTarget.style.borderColor = c.borderSoft; e.currentTarget.style.color = c.textSecondary; }}
                >
                  {ICON.more}
                </button>
              </div>
            </div>
          );
        })}
      </div>

      <style>{`
        .provider-row:hover { background: ${c.innerBg}; }
      `}</style>

      <ProviderDetailModal
        open={!!selected}
        onClose={() => setSelected(null)}
        providerKey={selected?.key || ""}
        providerName={selected?.name || ""}
      />
    </section>
  );
}

function rowBtn(c: ReturnType<typeof getColors>) {
  return {
    width: 28, height: 28, borderRadius: 6,
    border: `1px solid ${c.borderSoft}`,
    background: c.cardBg,
    display: "flex", alignItems: "center", justifyContent: "center",
    color: c.textSecondary, cursor: "pointer",
    transition: "color .12s ease-out, background-color .12s ease-out, border-color .12s ease-out",
  } as React.CSSProperties;
}
