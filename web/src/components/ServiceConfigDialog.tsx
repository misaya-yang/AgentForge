import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import type { RequestBody } from "@/api/contracts";
import { SdkAgentSettings } from "@/components/SdkAgentSettings";
import { isNativeAgentSdk } from "@/lib/sdkAgentConfig";
import { useState, useEffect, useMemo, useCallback } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import { Badge } from "@/components/ui/badge";
import { api } from "@/lib/api";
import { getService, updateService, deleteService as deleteServiceDef } from "@/api/gateway";
import * as providersApi from "@/api/providers";
import * as modelsApi from "@/api/models";
import type {
  ServiceDetail,
  ServiceModelFailoverCandidate,
  ServiceModelOverride,
} from "@/types/gateway";
import { useTranslation } from "react-i18next";

interface ServiceConfig {
  rate_limit: {
    enabled: boolean;
    requests: number;
    window: number;
    burst: number;
    strategy: string;
  };
  auth: {
    enabled: boolean;
    require_auth: boolean;
    allowed_roles: string[];
    allowed_api_keys: string[];
    public: boolean;
  };
  cache: {
    enabled: boolean;
    ttl: number;
    semantic_cache: boolean;
  };
  priority: {
    priority: number;
    weight: number;
    max_queue_size: number;
    enforced?: boolean;
    scheduler?: string;
  };
  capacity: {
    upstream_group?: string | null;
    concurrency_limit?: number | null;
    queue_max: number;
    queue_timeout_ms: number;
    enforced?: boolean;
    source_status?: string;
  };
}

interface CapacityBudgetStatus {
  key: string;
  limit: number;
  queue_max: number;
  queue_timeout_ms: number;
  scope: string;
  source: string;
  source_status: string;
  shared: boolean;
  enforced: boolean;
  inflight: number;
  queue_depth: number;
}

interface ServiceCapacityStatus {
  request_class: string;
  provider_id?: string | null;
  gateway_instance_id: string;
  cluster_epoch: string;
  mode: string;
  budgets: CapacityBudgetStatus[];
}

interface ServiceConfigResponse {
  service_id: string;
  name: string;
  config: ServiceConfig;
  legacy: {
    timeout: number;
    max_retries: number;
    circuit_breaker_enabled: boolean;
    failure_threshold: number;
    recovery_timeout: number;
  };
  capacity_status?: ServiceCapacityStatus;
}

const DEFAULT_MODEL_FAILOVER_PROVIDER_PRIORITY = [
  "google",
  "dashscope",
  "dashscope-intl",
  "dashscope-cn",
];
const DEFAULT_MODEL_FAILOVER_MAX_CANDIDATES = 2;
const LOAD_BALANCE_STRATEGIES = ["round_robin", "least_connections", "random"] as const;

function providerHasRuntimeCredentials(provider?: providersApi.Provider): boolean {
  return Boolean(provider?.has_api_key || provider?.allow_environment_credentials);
}

function normalizeUrl(value: string): string {
  const trimmed = value.trim();
  if (!trimmed) return "";
  // Only allow http(s) — reject javascript:, data:, ftp:, etc. (SSRF/XSS defense-in-depth)
  if (!/^https?:\/\//i.test(trimmed)) return "";
  return trimmed.replace(/\/+$/, "");
}

function normalizeUrlList(value: string): string[] {
  return value
    .split(/[\n,]/)
    .map(normalizeUrl)
    .filter(Boolean);
}

function readUpstreamUrls(connectorConfig: Record<string, unknown>): string[] {
  const rawUrls = connectorConfig.upstream_urls;
  if (Array.isArray(rawUrls)) {
    return rawUrls.map((url) => normalizeUrl(String(url || ""))).filter(Boolean);
  }

  const instanceUrls = connectorConfig.instance_urls;
  if (typeof instanceUrls === "string") {
    return normalizeUrlList(instanceUrls);
  }

  return [];
}

function readLoadBalanceStrategy(connectorConfig: Record<string, unknown>): string {
  const strategy = String(connectorConfig.load_balance_strategy || "round_robin");
  return LOAD_BALANCE_STRATEGIES.includes(
    strategy as (typeof LOAD_BALANCE_STRATEGIES)[number]
  )
    ? strategy
    : "round_robin";
}

function upstreamGroupFromBudget(budget?: CapacityBudgetStatus): string {
  const key = String(budget?.key || "");
  return key.startsWith("upstream.") ? key.slice("upstream.".length) : "";
}

function detectLangGraphService(serviceDetail?: ServiceDetail): boolean {
  if (!serviceDetail) return false;

  const metadata = (serviceDetail.metadata || {}) as Record<string, unknown>;
  const connector = (serviceDetail.connector_config || {}) as Record<string, unknown>;
  const serviceType = String(serviceDetail.service_type || "").toLowerCase();
  const adapterType = String(metadata.adapter_type || connector.adapter_type || "").toLowerCase();
  const proxyMode = String(connector.proxy_mode || metadata.proxy_mode || "").toLowerCase();
  const hasAssistantIdentity = Boolean(
    String(connector.graph_id || "").trim() || String(connector.assistant_id || "").trim()
  );

  return (
    serviceType === "langgraph" ||
    adapterType === "langgraph" ||
    (proxyMode === "transparent" && hasAssistantIdentity)
  );
}

function readModelOverride(value: unknown): ServiceModelOverride {
  if (!value || typeof value !== "object") {
    return {
      enabled: false,
      temperature: 0.1,
      failover: { enabled: false, max_attempts: 3, candidates: [] },
    };
  }

  const override = value as Record<string, unknown>;
  const failover = override.failover as Record<string, unknown> | undefined;
  const rawCandidates = Array.isArray(failover?.candidates) ? failover.candidates : [];
  const candidates = rawCandidates
    .filter((candidate): candidate is Record<string, unknown> =>
      Boolean(candidate && typeof candidate === "object")
    )
    .map((candidate) => ({
      provider_id: typeof candidate.provider_id === "string" ? candidate.provider_id : undefined,
      model_id: typeof candidate.model_id === "string" ? candidate.model_id : undefined,
    }));
  const temperature =
    typeof override.temperature === "number" && Number.isFinite(override.temperature)
      ? override.temperature
      : 0.1;

  return {
    enabled: Boolean(override.enabled),
    provider_id: typeof override.provider_id === "string" ? override.provider_id : undefined,
    model_id: typeof override.model_id === "string" ? override.model_id : undefined,
    temperature,
    failover: {
      enabled: Boolean(failover?.enabled),
      max_attempts:
        typeof failover?.max_attempts === "number" && Number.isFinite(failover.max_attempts)
          ? failover.max_attempts
          : 3,
      candidates,
    },
  };
}

async function getServiceConfig(serviceId: string): Promise<ServiceConfigResponse> {
  const { data } = await api.get<ServiceConfigResponse>(`/api/v1/config/services/${serviceId}/config`);
  return data;
}

async function updateServiceConfig(serviceId: string, config: RequestBody<"PUT /api/v1/config/services/{service_id}/config">) {
  const { data } = await api.put(`/api/v1/config/services/${serviceId}/config`, (config satisfies RequestBody<"PUT /api/v1/config/services/{service_id}/config">));
  return data;
}

export function ServiceConfigDialog({
  serviceId,
  serviceName,
  open,
  onOpenChange,
}: {
  serviceId: string;
  serviceName: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { t } = useTranslation();
  const providerReadScope = useAuthenticatedReadScope();
  const queryClient = useQueryClient();
  const [activeTab, setActiveTab] = useState("basic");
  const [basicError, setBasicError] = useState<string | null>(null);

  const serviceQuery = useQuery({
    queryKey: ["service-detail", serviceId],
    queryFn: () => getService(serviceId),
    enabled: open && !!serviceId,
  });

  // 查询服务配置
  const configQuery = useQuery({
    queryKey: ["service-config", serviceId],
    queryFn: () => getServiceConfig(serviceId),
    enabled: open && !!serviceId,
  });

  const config = configQuery.data?.config;
  const capacityStatus = configQuery.data?.capacity_status;
  const capacityBudgets = capacityStatus?.budgets || [];
  const priorityEnforced = config?.priority?.enforced === true;
  const capacityEnforced = config?.capacity?.enforced !== false;
  const upstreamBudget = capacityBudgets.find((budget) => budget.scope === "upstream");
  const serviceDetail: ServiceDetail | undefined = serviceQuery.data;
  const isLangGraphService = detectLangGraphService(serviceDetail);

  // 表单状态
  const [rateLimitForm, setRateLimitForm] = useState({
    enabled: false,
    requests: 100,
    window: 60,
    burst: 0,
    strategy: "sliding_window",
  });

  const [authForm, setAuthForm] = useState({
    enabled: false,
    require_auth: true,
    allowed_roles: [] as string[],
    public: false,
  });

  const [cacheForm, setCacheForm] = useState({
    enabled: false,
    ttl: 300,
    semantic_cache: false,
  });

  const [priorityForm, setPriorityForm] = useState({
    priority: 5,
    weight: 1,
    max_queue_size: 100,
  });
  const [capacityForm, setCapacityForm] = useState({
    upstream_group: "",
    concurrency_limit: "",
    queue_max: 16,
    queue_timeout_ms: 3000,
  });

  const [basicForm, setBasicForm] = useState({
    name: "",
    description: "",
    status: "active",
    deployment_url: "",
    upstream_urls_text: "",
    load_balance_strategy: "round_robin",
    graph_id: "",
    session_enabled: true,
  });
  const [modelOverrideForm, setModelOverrideForm] = useState<ServiceModelOverride>({
    enabled: false,
    temperature: 0.1,
    failover: { enabled: false, max_attempts: 3, candidates: [] },
  });

  const selectedProviderId = modelOverrideForm.provider_id || "";
  const providersQuery = useQuery({
    ...providersApi.providersOptions(providerReadScope.id, true),
    enabled: open && isLangGraphService && providerReadScope.isCurrent(),
  });
  const modelsQuery = useQuery({
    queryKey: ["models", "all-enabled-and-disabled"],
    queryFn: () => modelsApi.listModels(undefined, true),
    enabled: open && isLangGraphService,
  });

  const providers = useMemo(() => providersQuery.data ?? [], [providersQuery.data]);
  const models = useMemo(() => modelsQuery.data ?? [], [modelsQuery.data]);
  const providerById = useMemo(
    () => new Map(providers.map((provider) => [provider.provider_id, provider])),
    [providers]
  );
  const selectableProviders = useMemo(
    () =>
      providers.filter(
        (provider) => provider.is_enabled && providerHasRuntimeCredentials(provider)
      ),
    [providers]
  );
  const modelsByProvider = useMemo(() => {
    const grouped = new Map<string, typeof models>();
    for (const model of models) {
      const current = grouped.get(model.provider_id) ?? [];
      current.push(model);
      grouped.set(model.provider_id, current);
    }
    return grouped;
  }, [models]);
  const providerModels = useMemo(
    () => modelsByProvider.get(selectedProviderId) ?? [],
    [modelsByProvider, selectedProviderId]
  );
  const selectableModels = useMemo(
    () => providerModels.filter((model) => model.is_enabled),
    [providerModels]
  );
  const selectedProvider = providerById.get(modelOverrideForm.provider_id || "");
  const selectedModel = providerModels.find(
    (model) => model.model_id === modelOverrideForm.model_id
  );
  const failover = modelOverrideForm.failover ?? {
    enabled: false,
    max_attempts: 3,
    candidates: [],
  };
  const failoverCandidates = failover.candidates ?? [];
  const getProviderModels = useCallback(
    (providerId?: string) =>
      providerId ? (modelsByProvider.get(providerId) ?? []).filter((model) => model.is_enabled) : [],
    [modelsByProvider]
  );
  const buildDefaultFailoverCandidates = useCallback(
    (primaryProviderId?: string, primaryModelId?: string): ServiceModelFailoverCandidate[] => {
      const candidates: ServiceModelFailoverCandidate[] = [];
      const seen = new Set([`${primaryProviderId || ""}::${primaryModelId || ""}`]);
      const seenProviders = new Set(primaryProviderId ? [primaryProviderId] : []);

      for (const providerId of DEFAULT_MODEL_FAILOVER_PROVIDER_PRIORITY) {
        if (seenProviders.has(providerId)) continue;

        const provider = providerById.get(providerId);
        if (!provider?.is_enabled || !providerHasRuntimeCredentials(provider)) continue;

        const model = [...getProviderModels(providerId)].sort((a, b) => {
          const order = (b.sort_order || 0) - (a.sort_order || 0);
          if (order !== 0) return order;
          return (a.display_name || a.model_id).localeCompare(b.display_name || b.model_id);
        })[0];
        if (!model) continue;

        const key = `${providerId}::${model.model_id}`;
        if (seen.has(key)) continue;

        candidates.push({ provider_id: providerId, model_id: model.model_id });
        seen.add(key);
        seenProviders.add(providerId);
        if (candidates.length >= DEFAULT_MODEL_FAILOVER_MAX_CANDIDATES) break;
      }

      return candidates;
    },
    [getProviderModels, providerById]
  );
  const seedDefaultFailover = useCallback(
    (
      current: ServiceModelOverride["failover"],
      primaryProviderId?: string,
      primaryModelId?: string
    ): NonNullable<ServiceModelOverride["failover"]> => {
      const currentFailover = current ?? {
        enabled: false,
        max_attempts: 3,
        candidates: [],
      };
      if ((currentFailover.candidates ?? []).length > 0) {
        return currentFailover;
      }

      const candidates = buildDefaultFailoverCandidates(primaryProviderId, primaryModelId);
      if (candidates.length === 0) {
        return currentFailover;
      }

      return {
        ...currentFailover,
        enabled: true,
        max_attempts: Math.max(currentFailover.max_attempts ?? 3, 2),
        candidates,
      };
    },
    [buildDefaultFailoverCandidates]
  );
  const reconcileFailoverForPrimary = useCallback(
    (
      current: ServiceModelOverride["failover"],
      primaryProviderId?: string,
      primaryModelId?: string
    ): NonNullable<ServiceModelOverride["failover"]> => {
      const currentFailover = current ?? {
        enabled: false,
        max_attempts: 3,
        candidates: [],
      };
      const candidates = (currentFailover.candidates ?? []).filter(
        (candidate) =>
          candidate.provider_id !== primaryProviderId ||
          candidate.model_id !== primaryModelId
      );
      const reconciled = {
        ...currentFailover,
        candidates,
        max_attempts:
          candidates.length > 0
            ? Math.max(currentFailover.max_attempts ?? 3, 2)
            : (currentFailover.max_attempts ?? 3),
      };

      if (!reconciled.enabled || candidates.length > 0) {
        return reconciled;
      }
      return seedDefaultFailover(reconciled, primaryProviderId, primaryModelId);
    },
    [seedDefaultFailover]
  );
  const fallbackInvalid = Boolean(
    modelOverrideForm.enabled &&
      failover.enabled &&
      (failoverCandidates.length === 0 ||
      failoverCandidates.some((candidate, index) => {
        const provider = providerById.get(candidate.provider_id || "");
        const candidateModels = getProviderModels(candidate.provider_id);
        const model = candidateModels.find((item) => item.model_id === candidate.model_id);
        const key = `${candidate.provider_id || ""}::${candidate.model_id || ""}`;
        const duplicatePrimary =
          candidate.provider_id === modelOverrideForm.provider_id &&
          candidate.model_id === modelOverrideForm.model_id;
        const duplicateFallback = failoverCandidates.some(
          (other, otherIndex) =>
            otherIndex !== index &&
            `${other.provider_id || ""}::${other.model_id || ""}` === key
        );
        return (
          !candidate.provider_id ||
          !candidate.model_id ||
          !provider ||
          !provider.is_enabled ||
          !providerHasRuntimeCredentials(provider) ||
          !model ||
          duplicatePrimary ||
          duplicateFallback
        );
      }))
  );
  const temperature = modelOverrideForm.temperature;
  const temperatureInvalid =
    modelOverrideForm.enabled &&
    (typeof temperature !== "number" || temperature < 0 || temperature > 2);
  const overrideInvalid = Boolean(
    isLangGraphService &&
      modelOverrideForm.enabled &&
      (!selectedProvider ||
        !selectedProvider.is_enabled ||
        !providerHasRuntimeCredentials(selectedProvider) ||
        !selectedModel ||
        !selectedModel.is_enabled ||
        temperatureInvalid ||
        fallbackInvalid)
  );

  // 当配置加载后更新表单
  useEffect(() => {
    if (config) {
      const effectiveUpstreamGroup = upstreamGroupFromBudget(upstreamBudget);
      setRateLimitForm(config.rate_limit);
      setAuthForm({
        enabled: config.auth.enabled,
        require_auth: config.auth.require_auth,
        allowed_roles: config.auth.allowed_roles,
        public: config.auth.public,
      });
      setCacheForm(config.cache);
      setPriorityForm(config.priority);
      setCapacityForm({
        upstream_group: config.capacity?.upstream_group || effectiveUpstreamGroup,
        concurrency_limit:
          config.capacity?.concurrency_limit == null
            ? ""
            : String(config.capacity.concurrency_limit),
        queue_max: config.capacity?.queue_max ?? 16,
        queue_timeout_ms: config.capacity?.queue_timeout_ms ?? 3000,
      });
    }
  }, [config, upstreamBudget]);

  useEffect(() => {
    if (!serviceDetail) return;
    const cc = (serviceDetail.connector_config || {}) as Record<string, unknown>;
    const baseUrl = normalizeUrl(String(cc.base_url || ""));
    const upstreamUrl = normalizeUrl(String(cc.upstream_url || ""));
    const upstreamUrls = readUpstreamUrls(cc);
    const proxyMode = String(cc.proxy_mode || serviceDetail.metadata?.proxy_mode || "");
    const deploymentUrl =
      baseUrl && upstreamUrl && baseUrl !== upstreamUrl && proxyMode === "transparent"
        ? baseUrl
        : (upstreamUrl || baseUrl);
    setBasicForm({
      name: serviceDetail.name || "",
      description: serviceDetail.description || "",
      status: serviceDetail.status || "active",
      deployment_url: deploymentUrl,
      upstream_urls_text: upstreamUrls.join("\n"),
      load_balance_strategy: readLoadBalanceStrategy(cc),
      graph_id: String(cc.graph_id || cc.assistant_id || ""),
      session_enabled: Boolean(serviceDetail.session_enabled ?? true),
    });
    setModelOverrideForm(readModelOverride(cc.model_override));
  }, [serviceDetail]);

  useEffect(() => {
    if (
      !open ||
      !isLangGraphService ||
      !modelOverrideForm.enabled ||
      !modelOverrideForm.provider_id ||
      !modelOverrideForm.model_id ||
      failoverCandidates.length > 0
    ) {
      return;
    }

    setModelOverrideForm((current) => ({
      ...current,
      failover: seedDefaultFailover(current.failover, current.provider_id, current.model_id),
    }));
  }, [
    failoverCandidates.length,
    isLangGraphService,
    modelOverrideForm.enabled,
    modelOverrideForm.model_id,
    modelOverrideForm.provider_id,
    open,
    seedDefaultFailover,
  ]);

  // 更新配置
  const updateMutation = useMutation({
    mutationFn: (data: Partial<ServiceConfig>) => updateServiceConfig(serviceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["service-config", serviceId] });
      queryClient.invalidateQueries({ queryKey: ["services"] });
      queryClient.invalidateQueries({ queryKey: ["playground-services"] });
      onOpenChange(false);
    },
  });

  // 删除服务
  const deleteMutation = useMutation({
    mutationFn: () => deleteServiceDef(serviceId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["services"] });
      queryClient.invalidateQueries({ queryKey: ["playground-services"] });
      onOpenChange(false);
    },
  });

  const updateServiceMutation = useMutation({
    mutationFn: (patch: Record<string, unknown>) => updateService(serviceId, patch),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["service-detail", serviceId] });
      queryClient.invalidateQueries({ queryKey: ["services"] });
      queryClient.invalidateQueries({ queryKey: ["playground-services"] });
      onOpenChange(false);
    },
    onError: (err) => {
      const msg = err instanceof Error ? err.message : t("services.configDialog.updateFailed");
      setBasicError(msg);
    },
  });

  const handleSaveRateLimit = () => {
    updateMutation.mutate({ rate_limit: rateLimitForm });
  };

  const handleSaveAuth = () => {
    updateMutation.mutate({ auth: { ...authForm, allowed_api_keys: [] } });
  };

  const handleSaveCache = () => {
    updateMutation.mutate({ cache: cacheForm });
  };

  const handleSavePriority = () => {
    if (!priorityEnforced) return;
    updateMutation.mutate({ priority: priorityForm });
  };

  const handleSaveCapacity = () => {
    if (!capacityEnforced) return;
    updateMutation.mutate({
      capacity: {
        upstream_group: capacityForm.upstream_group || null,
        concurrency_limit: capacityForm.concurrency_limit
          ? Number(capacityForm.concurrency_limit)
          : null,
        queue_max: capacityForm.queue_max,
        queue_timeout_ms: capacityForm.queue_timeout_ms,
      },
    });
  };

  const handleProviderChange = (providerId: string) => {
    setModelOverrideForm((current) => {
      const modelId = current.provider_id === providerId ? current.model_id : undefined;
      return {
        ...current,
        provider_id: providerId,
        model_id: modelId,
        failover: reconcileFailoverForPrimary(current.failover, providerId, modelId),
      };
    });
  };

  const handleModelChange = (modelId: string) => {
    setModelOverrideForm((current) => ({
      ...current,
      model_id: modelId,
      failover: reconcileFailoverForPrimary(current.failover, current.provider_id, modelId),
    }));
  };

  const handleTemperatureChange = (value: string) => {
    const parsed = Number(value);
    setModelOverrideForm((current) => ({
      ...current,
      temperature: Number.isFinite(parsed) ? parsed : null,
    }));
  };

  const updateFailover = (
    updater: (
      current: NonNullable<ServiceModelOverride["failover"]>
    ) => NonNullable<ServiceModelOverride["failover"]>
  ) => {
    setModelOverrideForm((current) => {
      const currentFailover = current.failover ?? {
        enabled: false,
        max_attempts: 3,
        candidates: [],
      };
      return { ...current, failover: updater(currentFailover) };
    });
  };

  const handleFailoverToggle = (enabled: boolean) => {
    setModelOverrideForm((current) => {
      const currentFailover = current.failover ?? {
        enabled: false,
        max_attempts: 3,
        candidates: [],
      };
      const next = {
        ...currentFailover,
        enabled,
        max_attempts: enabled ? Math.max(currentFailover.max_attempts ?? 3, 2) : (currentFailover.max_attempts ?? 3),
        candidates: currentFailover.candidates ?? [],
      };
      return {
        ...current,
        failover: reconcileFailoverForPrimary(next, current.provider_id, current.model_id),
      };
    });
  };

  const handleAddFallback = () => {
    updateFailover((current) => ({
      ...current,
      enabled: true,
      candidates: [...(current.candidates ?? []), {}],
    }));
  };

  const handleFallbackCandidateChange = (
    index: number,
    patch: { provider_id?: string; model_id?: string }
  ) => {
    updateFailover((current) => {
      const next = [...(current.candidates ?? [])];
      const previous = next[index] ?? {};
      next[index] = {
        ...previous,
        ...patch,
        model_id:
          patch.provider_id && patch.provider_id !== previous.provider_id
            ? undefined
            : (patch.model_id ?? previous.model_id),
      };
      return { ...current, candidates: next };
    });
  };

  const handleRemoveFallback = (index: number) => {
    updateFailover((current) => ({
      ...current,
      candidates: (current.candidates ?? []).filter((_, i) => i !== index),
    }));
  };

  const handleMoveFallback = (index: number, direction: -1 | 1) => {
    updateFailover((current) => {
      const next = [...(current.candidates ?? [])];
      const target = index + direction;
      if (target < 0 || target >= next.length) return current;
      [next[index], next[target]] = [next[target], next[index]];
      return { ...current, candidates: next };
    });
  };

  const handleDelete = () => {
    if (confirm(t("services.configDialog.deleteConfirm", { name: serviceName }))) {
      deleteMutation.mutate();
    }
  };

  const handleSaveBasic = () => {
    setBasicError(null);
    const deploymentUrl = normalizeUrl(basicForm.deployment_url);
    const upstreamUrls = normalizeUrlList(basicForm.upstream_urls_text);
    const primaryUrl = deploymentUrl || upstreamUrls[0] || "";

    const patch: Record<string, unknown> = {
      name: basicForm.name,
      description: basicForm.description,
      status: basicForm.status,
      session_enabled: basicForm.session_enabled,
    };

    if (isLangGraphService) {
      const graphId = String(basicForm.graph_id || "").trim();
      if (!graphId) {
        setBasicError(t("services.configDialog.basic.graphIdRequired"));
        return;
      }
      if (overrideInvalid) {
        setBasicError(t("services.configDialog.model.overrideInvalid"));
        return;
      }
      const defaultFailoverCandidates = failover.enabled
        ? buildDefaultFailoverCandidates(
            modelOverrideForm.provider_id,
            modelOverrideForm.model_id
          )
        : [];
      const effectiveFailoverCandidates =
        failoverCandidates.length > 0 ? failoverCandidates : defaultFailoverCandidates;
      const effectiveFailoverEnabled = Boolean(
        modelOverrideForm.enabled && failover.enabled && effectiveFailoverCandidates.length > 0
      );

      patch.connector_config = {
        ...(serviceDetail?.connector_config || {}),
        base_url: primaryUrl,
        upstream_url: primaryUrl,
        upstream_urls: upstreamUrls,
        load_balance_strategy: basicForm.load_balance_strategy,
        graph_id: graphId,
        assistant_id: graphId,
        model_override: {
          enabled: modelOverrideForm.enabled,
          provider_id: modelOverrideForm.provider_id,
          model_id: modelOverrideForm.model_id,
          temperature: modelOverrideForm.temperature ?? null,
          failover: {
            enabled: effectiveFailoverEnabled,
            max_attempts: effectiveFailoverEnabled
              ? Math.max(failover.max_attempts ?? 3, 2)
              : (failover.max_attempts ?? 3),
            candidates: effectiveFailoverEnabled ? effectiveFailoverCandidates : [],
          },
        },
      };
      patch.metadata = {
        ...((serviceDetail?.metadata || {}) as Record<string, unknown>),
        adapter_type: "langgraph",
        proxy_mode: "transparent",
      };
    }

    updateServiceMutation.mutate(patch);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="w-[95vw] max-w-2xl max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <span>{t("services.configDialog.title")}</span>
            <Badge variant="outline">{serviceName}</Badge>
          </DialogTitle>
          <DialogDescription className="sr-only">
            {t(
              "services.configDialog.description",
              "Configure service connection, model override, rate limits, authentication, cache, priority, and danger-zone settings."
            )}
          </DialogDescription>
        </DialogHeader>

        {(configQuery.isLoading || serviceQuery.isLoading) ? (
          <div className="py-8 text-center text-muted-foreground">{t("common.loading")}</div>
        ) : (
          <Tabs value={activeTab} onValueChange={setActiveTab}>
            <TabsList className="flex flex-wrap gap-1 h-auto p-1 sm:grid sm:grid-cols-7">
              <TabsTrigger value="basic" className="text-xs sm:text-sm">{t("services.configDialog.tabs.basic")}</TabsTrigger>
              <TabsTrigger value="rate_limit" className="text-xs sm:text-sm">{t("services.configDialog.tabs.rateLimit")}</TabsTrigger>
              <TabsTrigger value="capacity" className="text-xs sm:text-sm">{t("services.configDialog.tabs.capacity", "Capacity")}</TabsTrigger>
              <TabsTrigger value="auth" className="text-xs sm:text-sm">{t("services.configDialog.tabs.auth")}</TabsTrigger>
              <TabsTrigger value="cache" className="text-xs sm:text-sm">{t("services.configDialog.tabs.cache")}</TabsTrigger>
              <TabsTrigger value="priority" className="text-xs sm:text-sm">{t("services.configDialog.tabs.priority")}</TabsTrigger>
              <TabsTrigger value="danger" className="text-xs sm:text-sm">{t("services.configDialog.tabs.danger")}</TabsTrigger>
            </TabsList>

            <TabsContent value="basic" className="space-y-4 pt-4">
              {open && isNativeAgentSdk(serviceDetail?.metadata?.adapter_type) && <SdkAgentSettings serviceId={serviceId} />}
              <div className="rounded-lg border p-4 space-y-4">
                <div className="grid grid-cols-2 gap-4">
                  <div className="space-y-2">
                    <Label>{t("services.configDialog.basic.serviceName")}</Label>
                    <Input
                      value={basicForm.name}
                      onChange={(e) => setBasicForm({ ...basicForm, name: e.target.value })}
                    />
                  </div>
                  <div className="space-y-2">
                    <Label>{t("services.configDialog.basic.status")}</Label>
                    <Select
                      value={basicForm.status}
                      onValueChange={(v) => setBasicForm({ ...basicForm, status: v })}
                    >
                      <SelectTrigger>
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent disablePortal>
                        <SelectItem value="active">{t("common.active")}</SelectItem>
                        <SelectItem value="inactive">{t("services.configDialog.status.inactive")}</SelectItem>
                        <SelectItem value="disabled">{t("common.disabled")}</SelectItem>
                      </SelectContent>
                    </Select>
                  </div>
                </div>

                <div className="space-y-2">
                  <Label>{t("services.configDialog.basic.description")}</Label>
                  <Input
                    value={basicForm.description}
                    onChange={(e) => setBasicForm({ ...basicForm, description: e.target.value })}
                  />
                </div>

                {isLangGraphService && (
                  <>
                    <Separator />
                    <div className="grid grid-cols-2 gap-4">
                      <div className="space-y-2">
                        <Label>{t("services.configDialog.basic.langgraphUrl")}</Label>
                        <Input
                          placeholder={t("services.configDialog.basic.langgraphUrlPlaceholder")}
                          value={basicForm.deployment_url}
                          onChange={(e) =>
                            setBasicForm({ ...basicForm, deployment_url: e.target.value })
                          }
                        />
                      </div>
                      <div className="space-y-2">
                        <Label>{t("services.configDialog.basic.graphId")}</Label>
                        <Input
                          placeholder={t("services.configDialog.basic.graphIdPlaceholder")}
                          value={basicForm.graph_id}
                          onChange={(e) => setBasicForm({ ...basicForm, graph_id: e.target.value })}
                        />
                      </div>
                    </div>

                    <div className="grid gap-4 sm:grid-cols-[1fr_220px]">
                      <div className="space-y-2">
                        <Label>{t("services.configDialog.basic.upstreamUrls")}</Label>
                        <Textarea
                          value={basicForm.upstream_urls_text}
                          placeholder={"http://langgraph-agent-1:8000\nhttp://langgraph-agent-2:8000"}
                          rows={3}
                          onChange={(e) =>
                            setBasicForm({ ...basicForm, upstream_urls_text: e.target.value })
                          }
                        />
                        <p className="text-xs text-muted-foreground">
                          {t("services.configDialog.basic.upstreamUrlsHint")}
                        </p>
                      </div>
                      <div className="space-y-2">
                        <Label>{t("services.configDialog.basic.loadBalanceStrategy")}</Label>
                        <Select
                          value={basicForm.load_balance_strategy}
                          onValueChange={(value) =>
                            setBasicForm({ ...basicForm, load_balance_strategy: value })
                          }
                        >
                          <SelectTrigger>
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent disablePortal>
                            <SelectItem value="round_robin">
                              {t("services.configDialog.basic.strategyRoundRobin")}
                            </SelectItem>
                            <SelectItem value="least_connections">
                              {t("services.configDialog.basic.strategyLeastConnections")}
                            </SelectItem>
                            <SelectItem value="random">
                              {t("services.configDialog.basic.strategyRandom")}
                            </SelectItem>
                          </SelectContent>
                        </Select>
                        <p className="text-xs text-muted-foreground">
                          {t("services.configDialog.basic.loadBalanceStrategyHint")}
                        </p>
                      </div>
                    </div>

                    <Separator />
                    <div className="space-y-3">
                      <div className="flex items-center justify-between gap-3">
                        <Label>{t("services.configDialog.model.title")}</Label>
                        <Switch
                          checked={modelOverrideForm.enabled}
                          onCheckedChange={(checked) => {
                            const nextProvider =
                              modelOverrideForm.provider_id ||
                              selectableProviders[0]?.provider_id;
                            setModelOverrideForm({
                              ...modelOverrideForm,
                              enabled: checked,
                              provider_id: checked
                                ? nextProvider
                                : modelOverrideForm.provider_id,
                            });
                          }}
                        />
                      </div>

                      <div className="grid gap-4 sm:grid-cols-2">
                        <div className="space-y-2">
                          <Label>{t("services.configDialog.model.provider")}</Label>
                          <Select
                            value={modelOverrideForm.provider_id || ""}
                            onValueChange={handleProviderChange}
                            disabled={!modelOverrideForm.enabled || providersQuery.isLoading}
                          >
                            <SelectTrigger>
                              <SelectValue
                                placeholder={t("services.configDialog.model.providerPlaceholder")}
                              />
                            </SelectTrigger>
                            <SelectContent disablePortal>
                              {selectableProviders.map((provider) => (
                                <SelectItem key={provider.provider_id} value={provider.provider_id}>
                                  <div className="flex min-w-0 items-center gap-2">
                                    <span className="truncate">{provider.display_name}</span>
                                    <span className="text-xs text-muted-foreground">
                                      {providerHasRuntimeCredentials(provider)
                                        ? t("services.configDialog.model.keyConfigured")
                                        : t("services.configDialog.model.keyMissing")}
                                    </span>
                                  </div>
                                </SelectItem>
                              ))}
                              {selectableProviders.length === 0 && (
                                <div className="px-3 py-2 text-sm text-muted-foreground">
                                  {t(
                                    "services.configDialog.model.noRuntimeProviders",
                                    "No enabled provider with an API key is available."
                                  )}
                                </div>
                              )}
                            </SelectContent>
                          </Select>
                        </div>

                        <div className="space-y-2">
                          <Label>{t("services.configDialog.model.model")}</Label>
                          <Select
                            value={modelOverrideForm.model_id || ""}
                            onValueChange={handleModelChange}
                            disabled={
                              !modelOverrideForm.enabled ||
                              !modelOverrideForm.provider_id ||
                              modelsQuery.isLoading
                            }
                          >
                            <SelectTrigger>
                              <SelectValue
                                placeholder={t("services.configDialog.model.modelPlaceholder")}
                              />
                            </SelectTrigger>
                            <SelectContent disablePortal>
                              {selectableModels.map((model) => (
                                <SelectItem key={model.model_id} value={model.model_id}>
                                  {model.display_name || model.model_id}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        </div>
                      </div>

                      <div className="space-y-2">
                        <Label>{t("services.configDialog.model.temperature")}</Label>
                        <Input
                          type="number"
                          min="0"
                          max="2"
                          step="0.1"
                          value={modelOverrideForm.temperature ?? ""}
                          onChange={(e) => handleTemperatureChange(e.target.value)}
                          disabled={!modelOverrideForm.enabled}
                        />
                      </div>

                      <div className="rounded-lg border p-3 space-y-3">
                        <div className="flex items-center justify-between gap-3">
                          <div>
                            <Label>
                              {t("services.configDialog.model.failover", "Failover")}
                            </Label>
                            <p className="text-xs text-muted-foreground">
                              {t(
                                "services.configDialog.model.failoverHint",
                                "Try configured fallback models when the primary provider is unavailable."
                              )}
                            </p>
                          </div>
                          <Switch
                            checked={Boolean(failover.enabled)}
                            disabled={!modelOverrideForm.enabled}
                            onCheckedChange={handleFailoverToggle}
                          />
                        </div>

                        {failover.enabled && (
                          <>
                            <div className="grid gap-3 sm:grid-cols-[1fr_120px]">
                              <div className="space-y-2">
                                <Label>
                                  {t(
                                    "services.configDialog.model.maxAttempts",
                                    "Max attempts"
                                  )}
                                </Label>
                                <Input
                                  type="number"
                                  min="1"
                                  max="10"
                                  value={failover.max_attempts ?? 3}
                                  onChange={(event) => {
                                    const parsed = Number(event.target.value);
                                    updateFailover((current) => ({
                                      ...current,
                                      max_attempts: Number.isFinite(parsed) ? parsed : 3,
                                    }));
                                  }}
                                  disabled={!modelOverrideForm.enabled}
                                />
                              </div>
                              <div className="flex items-end">
                                <Button
                                  type="button"
                                  variant="outline"
                                  className="w-full"
                                  onClick={handleAddFallback}
                                  disabled={!modelOverrideForm.enabled}
                                >
                                  {t("services.configDialog.model.addFallback", "Add fallback")}
                                </Button>
                              </div>
                            </div>

                            <div className="space-y-2">
                              {failoverCandidates.map((candidate, index) => {
                                const candidateModels = getProviderModels(candidate.provider_id);
                                return (
                                  <div
                                    key={`${index}-${candidate.provider_id || "provider"}`}
                                    className="grid gap-2 rounded-md border p-2 sm:grid-cols-[1fr_1fr_auto]"
                                  >
                                    <Select
                                      value={candidate.provider_id || ""}
                                      onValueChange={(providerId) =>
                                        handleFallbackCandidateChange(index, {
                                          provider_id: providerId,
                                        })
                                      }
                                      disabled={!modelOverrideForm.enabled}
                                    >
                                      <SelectTrigger>
                                        <SelectValue
                                          placeholder={t(
                                            "services.configDialog.model.providerPlaceholder"
                                          )}
                                        />
                                      </SelectTrigger>
                                      <SelectContent disablePortal>
                                        {selectableProviders.map((provider) => (
                                          <SelectItem
                                            key={provider.provider_id}
                                            value={provider.provider_id}
                                          >
                                            {provider.display_name}
                                          </SelectItem>
                                        ))}
                                      </SelectContent>
                                    </Select>

                                    <Select
                                      value={candidate.model_id || ""}
                                      onValueChange={(modelId) =>
                                        handleFallbackCandidateChange(index, {
                                          model_id: modelId,
                                        })
                                      }
                                      disabled={!modelOverrideForm.enabled || !candidate.provider_id}
                                    >
                                      <SelectTrigger>
                                        <SelectValue
                                          placeholder={t(
                                            "services.configDialog.model.modelPlaceholder"
                                          )}
                                        />
                                      </SelectTrigger>
                                      <SelectContent disablePortal>
                                        {candidateModels.map((model) => (
                                          <SelectItem key={model.model_id} value={model.model_id}>
                                            {model.display_name || model.model_id}
                                          </SelectItem>
                                        ))}
                                      </SelectContent>
                                    </Select>

                                    <div className="flex items-center gap-1">
                                      <Button
                                        type="button"
                                        variant="ghost"
                                        size="sm"
                                        onClick={() => handleMoveFallback(index, -1)}
                                        disabled={index === 0}
                                      >
                                        Up
                                      </Button>
                                      <Button
                                        type="button"
                                        variant="ghost"
                                        size="sm"
                                        onClick={() => handleMoveFallback(index, 1)}
                                        disabled={index === failoverCandidates.length - 1}
                                      >
                                        Down
                                      </Button>
                                      <Button
                                        type="button"
                                        variant="ghost"
                                        size="sm"
                                        onClick={() => handleRemoveFallback(index)}
                                      >
                                        {t("common.remove", "Remove")}
                                      </Button>
                                    </div>
                                  </div>
                                );
                              })}

                              {failoverCandidates.length === 0 && (
                                <p className="text-xs text-muted-foreground">
                                  {t(
                                    "services.configDialog.model.noFallbacks",
                                    "Add at least one fallback provider/model."
                                  )}
                                </p>
                              )}
                            </div>
                          </>
                        )}
                      </div>

                      <div className="flex flex-wrap items-center gap-2 text-xs">
                        <Badge
                          variant={
                            selectedProvider?.is_enabled &&
                            providerHasRuntimeCredentials(selectedProvider)
                              ? "secondary"
                              : "destructive"
                          }
                        >
                          {selectedProvider
                            ? !selectedProvider.is_enabled
                              ? t("services.configDialog.model.providerDisabled")
                              : providerHasRuntimeCredentials(selectedProvider)
                                ? t("services.configDialog.model.keyConfigured")
                                : t("services.configDialog.model.keyMissing")
                            : t("services.configDialog.model.providerRequired")}
                        </Badge>
                        <Badge
                          variant={
                            selectedModel?.is_enabled || !modelOverrideForm.model_id
                              ? "secondary"
                              : "destructive"
                          }
                        >
                          {selectedModel
                            ? selectedModel.is_enabled
                              ? t("services.configDialog.model.modelEnabled")
                              : t("services.configDialog.model.modelDisabled")
                            : t("services.configDialog.model.modelRequired")}
                        </Badge>
                        {selectedProvider?.base_url && (
                          <span className="min-w-0 truncate text-muted-foreground">
                            {selectedProvider.base_url}
                          </span>
                        )}
                        {failover.enabled && (
                          <Badge variant={fallbackInvalid ? "destructive" : "secondary"}>
                            {fallbackInvalid
                              ? t(
                                  "services.configDialog.model.failoverInvalid",
                                  "Failover needs valid unique candidates"
                                )
                              : t("services.configDialog.model.failoverReady", {
                                  defaultValue: "{count} fallback candidates",
                                  count: failoverCandidates.length,
                                })}
                          </Badge>
                        )}
                      </div>
                    </div>

                    <div className="flex items-center justify-between rounded-lg border p-3">
                      <div>
                        <Label>{t("services.configDialog.basic.sessionEnabled")}</Label>
                        <p className="text-xs text-muted-foreground">{t("services.configDialog.basic.sessionHint")}</p>
                      </div>
                      <Switch
                        checked={basicForm.session_enabled}
                        onCheckedChange={(checked) =>
                          setBasicForm({ ...basicForm, session_enabled: checked })
                        }
                      />
                    </div>
                  </>
                )}
              </div>

              {basicError && <div className="text-sm text-destructive">{basicError}</div>}

              <div className="flex justify-end">
                <Button
                  onClick={handleSaveBasic}
                  disabled={updateServiceMutation.isPending || overrideInvalid}
                >
                  {updateServiceMutation.isPending ? t("common.saving") : t("services.configDialog.basic.save")}
                </Button>
              </div>
            </TabsContent>

            {/* 限流配置 */}
            <TabsContent value="rate_limit" className="space-y-4 pt-4">
              <div className="rounded-lg border p-4 space-y-4">
                <div className="flex items-center justify-between">
                  <div>
                    <Label>{t("services.configDialog.rateLimit.enable")}</Label>
                    <p className="text-xs text-muted-foreground">{t("services.configDialog.rateLimit.enableHint")}</p>
                  </div>
                  <Switch
                    checked={rateLimitForm.enabled}
                    onCheckedChange={(checked) =>
                      setRateLimitForm({ ...rateLimitForm, enabled: checked })
                    }
                  />
                </div>

                {rateLimitForm.enabled && (
                  <>
                    <Separator />
                    <div className="grid grid-cols-2 gap-4">
                      <div className="space-y-2">
                        <Label>{t("services.configDialog.rateLimit.requests")}</Label>
                        <Input
                          type="number"
                          value={rateLimitForm.requests}
                          onChange={(e) =>
                            setRateLimitForm({
                              ...rateLimitForm,
                              requests: parseInt(e.target.value) || 0,
                            })
                          }
                        />
                        <p className="text-xs text-muted-foreground">{t("services.configDialog.rateLimit.requestsHint")}</p>
                      </div>

                      <div className="space-y-2">
                        <Label>{t("services.configDialog.rateLimit.window")}</Label>
                        <Input
                          type="number"
                          value={rateLimitForm.window}
                          onChange={(e) =>
                            setRateLimitForm({
                              ...rateLimitForm,
                              window: parseInt(e.target.value) || 60,
                            })
                          }
                        />
                        <p className="text-xs text-muted-foreground">{t("services.configDialog.rateLimit.windowHint")}</p>
                      </div>

                      <div className="space-y-2">
                        <Label>{t("services.configDialog.rateLimit.burst")}</Label>
                        <Input
                          type="number"
                          value={rateLimitForm.burst}
                          onChange={(e) =>
                            setRateLimitForm({
                              ...rateLimitForm,
                              burst: parseInt(e.target.value) || 0,
                            })
                          }
                        />
                        <p className="text-xs text-muted-foreground">{t("services.configDialog.rateLimit.burstHint")}</p>
                      </div>

                      <div className="space-y-2">
                        <Label>{t("services.configDialog.rateLimit.strategy")}</Label>
                        <Select
                          value={rateLimitForm.strategy}
                          onValueChange={(value) =>
                            setRateLimitForm({ ...rateLimitForm, strategy: value })
                          }
                        >
                          <SelectTrigger>
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent disablePortal>
                            <SelectItem value="sliding_window">{t("services.configDialog.rateLimit.strategySliding")}</SelectItem>
                            <SelectItem value="fixed_window">{t("services.configDialog.rateLimit.strategyFixed")}</SelectItem>
                            <SelectItem value="token_bucket">{t("services.configDialog.rateLimit.strategyToken")}</SelectItem>
                          </SelectContent>
                        </Select>
                        <p className="text-xs text-muted-foreground">{t("services.configDialog.rateLimit.strategyHint")}</p>
                      </div>
                    </div>
                  </>
                )}
              </div>

              <div className="flex justify-end">
                <Button onClick={handleSaveRateLimit} disabled={updateMutation.isPending}>
                  {updateMutation.isPending ? t("common.saving") : t("services.configDialog.rateLimit.save")}
                </Button>
              </div>
            </TabsContent>

            {/* 容量配置 */}
            <TabsContent value="capacity" className="space-y-4 pt-4">
              <div className="rounded-lg border p-4 space-y-4">
                <div className="flex items-start justify-between gap-3">
                  <div>
                    <Label>{t("services.configDialog.capacity.title", "Admission Capacity")}</Label>
                    <p className="text-xs text-muted-foreground">
                      {t(
                        "services.configDialog.capacity.description",
                        "Controls in-flight runs and queueing before requests reach the agent upstream."
                      )}
                    </p>
                  </div>
                  <Badge variant={capacityEnforced ? "default" : "secondary"}>
                    {capacityEnforced
                      ? (config?.capacity?.source_status || "real")
                      : t("services.configDialog.capacity.notEnforced", "not enforced")}
                  </Badge>
                </div>

                <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
                  <div className="rounded-md border bg-muted/30 p-3">
                    <p className="text-xs text-muted-foreground">{t("services.configDialog.capacity.mode", "Mode")}</p>
                    <p className="mt-1 font-mono text-sm">{capacityStatus?.mode || "single-node"}</p>
                  </div>
                  <div className="rounded-md border bg-muted/30 p-3">
                    <p className="text-xs text-muted-foreground">{t("services.configDialog.capacity.inflight", "In-flight")}</p>
                    <p className="mt-1 font-mono text-sm">
                      {upstreamBudget ? `${upstreamBudget.inflight}/${upstreamBudget.limit}` : "-"}
                    </p>
                  </div>
                  <div className="rounded-md border bg-muted/30 p-3">
                    <p className="text-xs text-muted-foreground">{t("services.configDialog.capacity.queueDepth", "Queue")}</p>
                    <p className="mt-1 font-mono text-sm">
                      {upstreamBudget ? `${upstreamBudget.queue_depth}/${upstreamBudget.queue_max}` : "-"}
                    </p>
                  </div>
                  <div className="rounded-md border bg-muted/30 p-3">
                    <p className="text-xs text-muted-foreground">{t("services.configDialog.capacity.provider", "Provider")}</p>
                    <p className="mt-1 truncate font-mono text-sm">{capacityStatus?.provider_id || "-"}</p>
                  </div>
                </div>

                <Separator />

                <div className="grid grid-cols-2 gap-4">
                  <div className="space-y-2">
                    <Label>{t("services.configDialog.capacity.upstreamGroup", "Upstream Group")}</Label>
                    <Input
                      value={capacityForm.upstream_group}
                      disabled={!capacityEnforced}
                      placeholder="langgraph_agent"
                      onChange={(e) =>
                        setCapacityForm({ ...capacityForm, upstream_group: e.target.value })
                      }
                    />
                    <p className="text-xs text-muted-foreground">
                      {t("services.configDialog.capacity.upstreamGroupHint", "Budget group applied to this service.")}
                    </p>
                  </div>
                  <div className="space-y-2">
                    <Label>{t("services.configDialog.capacity.concurrencyLimit", "Concurrency Limit")}</Label>
                    <Input
                      type="number"
                      min="1"
                      value={capacityForm.concurrency_limit}
                      disabled={!capacityEnforced}
                      placeholder={String(upstreamBudget?.limit || "default")}
                      onChange={(e) =>
                        setCapacityForm({ ...capacityForm, concurrency_limit: e.target.value })
                      }
                    />
                    <p className="text-xs text-muted-foreground">
                      {t("services.configDialog.capacity.concurrencyHint", "Max simultaneous in-flight runs. Empty uses the default budget.")}
                    </p>
                  </div>
                  <div className="space-y-2">
                    <Label>{t("services.configDialog.capacity.queueMax", "Queue Max")}</Label>
                    <Input
                      type="number"
                      min="0"
                      value={capacityForm.queue_max}
                      disabled={!capacityEnforced}
                      onChange={(e) =>
                        setCapacityForm({
                          ...capacityForm,
                          queue_max: parseInt(e.target.value) || 0,
                        })
                      }
                    />
                    <p className="text-xs text-muted-foreground">
                      {t("services.configDialog.capacity.queueMaxHint", "Queued requests after concurrency is full. Use 0 to fail fast.")}
                    </p>
                  </div>
                  <div className="space-y-2">
                    <Label>{t("services.configDialog.capacity.queueTimeout", "Queue Timeout (ms)")}</Label>
                    <Input
                      type="number"
                      min="1"
                      value={capacityForm.queue_timeout_ms}
                      disabled={!capacityEnforced}
                      onChange={(e) =>
                        setCapacityForm({
                          ...capacityForm,
                          queue_timeout_ms: parseInt(e.target.value) || 3000,
                        })
                      }
                    />
                    <p className="text-xs text-muted-foreground">
                      {t("services.configDialog.capacity.queueTimeoutHint", "How long a queued request may wait before 503.")}
                    </p>
                  </div>
                </div>

                <Separator />

                <div className="space-y-2">
                  <Label>{t("services.configDialog.capacity.effectiveBudgets", "Effective Budgets")}</Label>
                  <div className="space-y-2">
                    {capacityBudgets.map((budget) => (
                      <div
                        key={budget.key}
                        className="grid grid-cols-[1fr_auto_auto_auto] items-center gap-3 rounded-md border bg-muted/20 px-3 py-2 text-sm"
                      >
                        <div className="min-w-0">
                          <p className="truncate font-mono">{budget.key}</p>
                          <p className="text-xs text-muted-foreground">
                            {budget.source} · {budget.source_status}
                            {budget.shared
                              ? ` · ${t("services.configDialog.capacity.shared", "shared")}`
                              : ""}
                          </p>
                        </div>
                        <span className="font-mono">
                          {budget.inflight}/{budget.limit}
                        </span>
                        <span className="font-mono text-muted-foreground">
                          q {budget.queue_depth}/{budget.queue_max}
                        </span>
                        <Badge variant={budget.enforced ? "default" : "secondary"}>
                          {budget.enforced
                            ? t("services.configDialog.capacity.enforcedOn", "on")
                            : t("services.configDialog.capacity.enforcedOff", "off")}
                        </Badge>
                      </div>
                    ))}
                    {capacityBudgets.length === 0 && (
                      <p className="rounded-md border bg-muted/20 p-3 text-sm text-muted-foreground">
                        {t("services.configDialog.capacity.noBudgets", "No capacity budgets reported for this service.")}
                      </p>
                    )}
                  </div>
                </div>
              </div>

              <div className="flex justify-end">
                <Button
                  onClick={handleSaveCapacity}
                  disabled={!capacityEnforced || updateMutation.isPending}
                >
                  {updateMutation.isPending
                    ? t("common.saving")
                    : t("services.configDialog.capacity.save", "Save Capacity")}
                </Button>
              </div>
            </TabsContent>

            {/* 鉴权配置 */}
            <TabsContent value="auth" className="space-y-4 pt-4">
              <div className="rounded-lg border p-4 space-y-4">
                <div className="flex items-center justify-between">
                  <div>
                    <Label>{t("services.configDialog.auth.enable")}</Label>
                    <p className="text-xs text-muted-foreground">{t("services.configDialog.auth.enableHint")}</p>
                  </div>
                  <Switch
                    checked={authForm.enabled}
                    onCheckedChange={(checked) => setAuthForm({ ...authForm, enabled: checked })}
                  />
                </div>

                {authForm.enabled && (
                  <>
                    <Separator />

                    <div className="flex items-center justify-between">
                      <div>
                        <Label>{t("services.configDialog.auth.public")}</Label>
                        <p className="text-xs text-muted-foreground">{t("services.configDialog.auth.publicHint")}</p>
                      </div>
                      <Switch
                        checked={authForm.public}
                        onCheckedChange={(checked) =>
                          setAuthForm({ ...authForm, public: checked, require_auth: !checked })
                        }
                      />
                    </div>

                    {!authForm.public && (
                      <>
                        <div className="flex items-center justify-between">
                          <div>
                            <Label>{t("services.configDialog.auth.requireAuth")}</Label>
                            <p className="text-xs text-muted-foreground">{t("services.configDialog.auth.requireAuthHint")}</p>
                          </div>
                          <Switch
                            checked={authForm.require_auth}
                            onCheckedChange={(checked) =>
                              setAuthForm({ ...authForm, require_auth: checked })
                            }
                          />
                        </div>

                        <div className="space-y-2">
                          <Label>{t("services.configDialog.auth.allowedRoles")}</Label>
                          <div className="flex flex-wrap gap-2">
                            {["user", "developer", "admin"].map((role) => (
                              <Badge
                                key={role}
                                variant={authForm.allowed_roles.includes(role) ? "default" : "outline"}
                                className="cursor-pointer"
                                onClick={() => {
                                  const roles = authForm.allowed_roles.includes(role)
                                    ? authForm.allowed_roles.filter((r) => r !== role)
                                    : [...authForm.allowed_roles, role];
                                  setAuthForm({ ...authForm, allowed_roles: roles });
                                }}
                              >
                                {role}
                              </Badge>
                            ))}
                          </div>
                          <p className="text-xs text-muted-foreground">
                            {t("services.configDialog.auth.rolesHint")}
                          </p>
                        </div>
                      </>
                    )}
                  </>
                )}
              </div>

              <div className="flex justify-end">
                <Button onClick={handleSaveAuth} disabled={updateMutation.isPending}>
                  {updateMutation.isPending ? t("common.saving") : t("services.configDialog.auth.save")}
                </Button>
              </div>
            </TabsContent>

            {/* 缓存配置 */}
            <TabsContent value="cache" className="space-y-4 pt-4">
              <div className="rounded-lg border p-4 space-y-4">
                <div className="flex items-center justify-between">
                  <div>
                    <Label>{t("services.configDialog.cache.enable")}</Label>
                    <p className="text-xs text-muted-foreground">{t("services.configDialog.cache.enableHint")}</p>
                  </div>
                  <Switch
                    checked={cacheForm.enabled}
                    onCheckedChange={(checked) => setCacheForm({ ...cacheForm, enabled: checked })}
                  />
                </div>

                {cacheForm.enabled && (
                  <>
                    <Separator />

                    <div className="space-y-2">
                      <Label>{t("services.configDialog.cache.ttl")}</Label>
                      <Input
                        type="number"
                        value={cacheForm.ttl}
                        onChange={(e) =>
                          setCacheForm({ ...cacheForm, ttl: parseInt(e.target.value) || 300 })
                        }
                      />
                      <p className="text-xs text-muted-foreground">{t("services.configDialog.cache.ttlHint")}</p>
                    </div>

                    <div className="flex items-center justify-between">
                      <div>
                        <Label>{t("services.configDialog.cache.semantic")}</Label>
                        <p className="text-xs text-muted-foreground">
                          {t("services.configDialog.cache.semanticHint")}
                        </p>
                      </div>
                      <Switch
                        checked={cacheForm.semantic_cache}
                        onCheckedChange={(checked) =>
                          setCacheForm({ ...cacheForm, semantic_cache: checked })
                        }
                      />
                    </div>
                  </>
                )}
              </div>

              <div className="flex justify-end">
                <Button onClick={handleSaveCache} disabled={updateMutation.isPending}>
                  {updateMutation.isPending ? t("common.saving") : t("services.configDialog.cache.save")}
                </Button>
              </div>
            </TabsContent>

            {/* 优先级配置 */}
            <TabsContent value="priority" className="space-y-4 pt-4">
              <div className="rounded-lg border p-4 space-y-4">
                {!priorityEnforced && (
                  <div className="flex items-start justify-between gap-3 rounded-md border bg-muted/30 p-3">
                    <div>
                      <Label>{t("services.configDialog.priority.notEnforced")}</Label>
                      <p className="text-xs text-muted-foreground">
                        {t("services.configDialog.priority.notEnforcedHint")}
                      </p>
                    </div>
                    <Badge variant="secondary">{config?.priority?.scheduler || "disabled"}</Badge>
                  </div>
                )}

                <div className="space-y-2">
                  <Label>{t("services.configDialog.priority.level")}</Label>
                  <div className="flex items-center gap-4">
                    <Input
                      type="range"
                      min="1"
                      max="10"
                      value={priorityForm.priority}
                      onChange={(e) =>
                        setPriorityForm({
                          ...priorityForm,
                          priority: parseInt(e.target.value),
                        })
                      }
                      disabled={!priorityEnforced}
                      className="flex-1"
                    />
                    <span className="w-8 text-center font-mono">{priorityForm.priority}</span>
                  </div>
                  <p className="text-xs text-muted-foreground">
                    {t("services.configDialog.priority.levelHint")}
                  </p>
                </div>

                <div className="grid grid-cols-2 gap-4">
                  <div className="space-y-2">
                    <Label>{t("services.configDialog.priority.weight")}</Label>
                    <Input
                      type="number"
                      min="1"
                      value={priorityForm.weight}
                      disabled={!priorityEnforced}
                      onChange={(e) =>
                        setPriorityForm({
                          ...priorityForm,
                          weight: parseInt(e.target.value) || 1,
                        })
                      }
                    />
                    <p className="text-xs text-muted-foreground">{t("services.configDialog.priority.weightHint")}</p>
                  </div>

                  <div className="space-y-2">
                    <Label>{t("services.configDialog.priority.maxQueue")}</Label>
                    <Input
                      type="number"
                      min="1"
                      value={priorityForm.max_queue_size}
                      disabled={!priorityEnforced}
                      onChange={(e) =>
                        setPriorityForm({
                          ...priorityForm,
                          max_queue_size: parseInt(e.target.value) || 100,
                        })
                      }
                    />
                    <p className="text-xs text-muted-foreground">{t("services.configDialog.priority.maxQueueHint")}</p>
                  </div>
                </div>
              </div>

              <div className="flex justify-end">
                <Button
                  onClick={handleSavePriority}
                  disabled={!priorityEnforced || updateMutation.isPending}
                >
                  {updateMutation.isPending ? t("common.saving") : t("services.configDialog.priority.save")}
                </Button>
              </div>
            </TabsContent>

            {/* 危险区 */}
            <TabsContent value="danger" className="space-y-4 pt-4">
              <div className="rounded-lg border border-destructive/50 p-4 space-y-4">
                <div>
                  <h3 className="text-lg font-semibold text-destructive">{t("services.configDialog.danger.title")}</h3>
                  <p className="text-sm text-muted-foreground mt-1">
                    {t("services.configDialog.danger.description")}
                  </p>
                </div>
                <Button variant="destructive" onClick={handleDelete} disabled={deleteMutation.isPending}>
                  {deleteMutation.isPending ? t("services.configDialog.danger.deleting") : t("services.configDialog.danger.delete")}
                </Button>
              </div>
            </TabsContent>
          </Tabs>
        )}

        {updateMutation.isSuccess && (
          <div className="text-sm text-green-600 dark:text-green-400">{t("services.configDialog.saved")}</div>
        )}
        {updateMutation.isError && (
          <div className="text-sm text-destructive">{t("services.configDialog.saveFailed")}</div>
        )}
      </DialogContent>
    </Dialog>
  );
}
