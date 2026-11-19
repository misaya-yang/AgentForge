import type { RequestQuery } from "@/api/contracts";
/**
 * Usage API - 用量统计接口
 *
 * 提供按服务、模型、用户、助手分类的用量分析
 */
import { api } from "@/lib/api";

// ============ Types ============

export interface UsageMeteringSummary {
  schema: "agentforge.usage-metering/v1";
  token_totals_scope: "reported_token_usage_only";
  request_priced_invocations: number;
  tokens_not_applicable_records: number;
  request_priced_cost_nanos: number;
  legacy_cost_projection: "USD half-even to 1e-6";
}

export interface UsageSummary {
  total_requests: number;
  success_rate: number | null;
  total_input_tokens: number;
  total_output_tokens: number;
  total_tokens: number;
  total_cost_usd: number;
  avg_latency_ms: number;
  start_date: string;
  end_date: string;
  data_status: string;
  data_freshness_minutes: number;
  last_ingested_at?: string | null;
  metering?: UsageMeteringSummary;
}

export interface UsageBreakdownItem {
  model?: string;
  user?: string;
  assistant?: string;
  service?: string;
  provider?: string;
  label?: string;
  requests: number;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  cost_usd: number;
  percentage: number;
}

export interface UsageBreakdownResponse {
  dimension: string;
  items: UsageBreakdownItem[];
  start_date: string;
  end_date: string;
  total_cost_usd: number;
  data_status: string;
  data_freshness_minutes: number;
  last_ingested_at?: string | null;
}

export interface UsageTimeSeriesPoint {
  date: string;
  requests: number;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  cost_usd: number;
  avg_latency_ms: number;
}

export interface UsageTimeSeriesResponse {
  data: UsageTimeSeriesPoint[];
  start_date: string;
  end_date: string;
  granularity: string;
  data_status: string;
  data_freshness_minutes: number;
  last_ingested_at?: string | null;
}

export interface LatencyBreakdownPoint {
  date: string;
  request_count: number;
  avg_total_ms: number;
  p50_total_ms: number;
  p95_total_ms: number;
  p99_total_ms: number;
  avg_first_token_ms: number;
  avg_llm_inference_ms: number;
  avg_retrieval_ms: number;
  avg_tool_call_ms: number;
  avg_overhead_ms: number;
}

export interface LatencyBreakdownResponse {
  data: LatencyBreakdownPoint[];
  start_date: string;
  end_date: string;
  granularity: string;
  data_status: string;
  data_freshness_minutes: number;
  last_ingested_at?: string | null;
}

export interface FailureBreakdownItem {
  dimension: "service" | "model" | "provider" | "user";
  dimension_value: string;
  error_type: string;
  failure_count: number;
  success_count: number;
  total_count: number;
  success_rate: number;
}

export interface FailureBreakdownResponse {
  dimension: string;
  items: FailureBreakdownItem[];
  start_date: string;
  end_date: string;
}

export interface RequestTrace {
  trace_id: string;
  request_id: string;
  tenant_id: string;
  user_id: string;
  service_id: string;
  assistant_id: string;
  provider: string;
  model: string;
  status: "success" | "error";
  error_type?: string | null;
  request_total_duration_ms: number;
  first_token_latency_ms: number;
  llm_inference_duration_ms: number;
  retrieval_duration_ms: number;
  tool_call_duration_ms: number;
  agent_or_graph_overhead_ms: number;
  input_tokens: number;
  output_tokens: number;
  total_cost_usd: number;
  sample_reason: string;
  trace_steps: Array<{
    name: string;
    duration_ms: number;
    start_offset_ms: number;
    status: "success" | "error" | "pending";
  }>;
  metadata: Record<string, unknown>;
  timestamp?: string | null;
}

export interface QuotaLimitObservation {
  /** UTC observation time; this is not a reservation or future admission guarantee. */
  observed_at: string;
  effective: {
    daily_tokens: number | null;
    monthly_tokens: number | null;
    monthly_cost_cents: number | null;
  };
  sources: Record<"daily_tokens" | "monthly_tokens" | "monthly_cost_cents",
    "configured" | "configured_plus_temporary_boost" | "unlimited">;
  temporary_boost: {
    extra_tokens: number;
    extra_cost_cents: number;
    expires_at: string | null;
    active: boolean;
  };
}

export interface QuotaUserOverviewItem {
  user_id: string;
  daily_tokens_used: number;
  /** Configured base; effective limits are a separate observation. */
  daily_tokens_limit: number | null;
  limit_observation?: QuotaLimitObservation | null;
  monthly_cost_used_cents: number;
  monthly_cost_limit_cents: number | null;
  monthly_tokens_used: number;
  monthly_tokens_limit: number | null;
  is_blocked: boolean;
  blocked_reason?: string | null;
  overage_strategy: "hard_block" | "rate_limit" | "downgrade_model" | "allow_but_alert";
  downgraded_model?: string | null;
  status: "ok" | "warning" | "exceeded" | "blocked";
}

export interface QuotaUsersOverviewResponse {
  users: QuotaUserOverviewItem[];
  summary: {
    total: number;
    blocked: number;
    exceeded: number;
    warning: number;
    ok: number;
  };
}

// ============ API Functions ============

/**
 * 获取用量摘要
 */
export async function getUsageSummary(params?: RequestQuery<"GET /api/v1/usage/summary">): Promise<UsageSummary> {
  const searchParams = new URLSearchParams();
  if (params?.start_date) searchParams.set("start_date", params.start_date);
  if (params?.end_date) searchParams.set("end_date", params.end_date);
  if (params?.user_id) searchParams.set("user_id", params.user_id);
  if (params?.model) searchParams.set("model", params.model);
  if (params?.service_id) searchParams.set("service_id", params.service_id);
  if (params?.assistant_id) searchParams.set("assistant_id", params.assistant_id);
  if (params?.provider) searchParams.set("provider", params.provider);

  const query = searchParams.toString();
  const response = await api.get<UsageSummary>(`/api/v1/usage/summary${query ? `?${query}` : ""}`);
  return response.data;
}

/**
 * 获取用量分解 - 按维度分类
 */
export async function getUsageBreakdown(params: RequestQuery<"GET /api/v1/usage/breakdown">): Promise<UsageBreakdownResponse> {
  const searchParams = new URLSearchParams();
  searchParams.set("dimension", params.dimension ?? "model");
  if (params.start_date) searchParams.set("start_date", params.start_date);
  if (params.end_date) searchParams.set("end_date", params.end_date);
  if (params.user_id) searchParams.set("user_id", params.user_id);
  if (params.service_id) searchParams.set("service_id", params.service_id);
  if (params.limit) searchParams.set("limit", params.limit.toString());

  const response = await api.get<UsageBreakdownResponse>(`/api/v1/usage/breakdown?${searchParams}`);
  return response.data;
}

/**
 * 获取用量时间序列
 */
export async function getUsageTimeSeries(params?: RequestQuery<"GET /api/v1/usage/timeseries">): Promise<UsageTimeSeriesResponse> {
  const searchParams = new URLSearchParams();
  if (params?.start_date) searchParams.set("start_date", params.start_date);
  if (params?.end_date) searchParams.set("end_date", params.end_date);
  if (params?.user_id) searchParams.set("user_id", params.user_id);
  if (params?.model) searchParams.set("model", params.model);
  if (params?.service_id) searchParams.set("service_id", params.service_id);
  if (params?.provider) searchParams.set("provider", params.provider);
  if (params?.granularity) searchParams.set("granularity", params.granularity);

  const query = searchParams.toString();
  const response = await api.get<UsageTimeSeriesResponse>(`/api/v1/usage/timeseries${query ? `?${query}` : ""}`);
  return response.data;
}

export async function getPerformanceBreakdown(params?: RequestQuery<"GET /api/v1/usage/performance-breakdown">, signal?: AbortSignal): Promise<LatencyBreakdownResponse> {
  const searchParams = new URLSearchParams();
  if (params?.start_date) searchParams.set("start_date", params.start_date);
  if (params?.end_date) searchParams.set("end_date", params.end_date);
  if (params?.user_id) searchParams.set("user_id", params.user_id);
  if (params?.model) searchParams.set("model", params.model);
  if (params?.service_id) searchParams.set("service_id", params.service_id);
  if (params?.provider) searchParams.set("provider", params.provider);
  if (params?.granularity) searchParams.set("granularity", params.granularity);

  const query = searchParams.toString();
  const response = await api.get<LatencyBreakdownResponse>(
    `/api/v1/usage/performance-breakdown${query ? `?${query}` : ""}`, { signal }
  );
  return response.data;
}

export async function getFailureBreakdown(params?: RequestQuery<"GET /api/v1/usage/failure-breakdown">, signal?: AbortSignal): Promise<FailureBreakdownResponse> {
  const searchParams = new URLSearchParams();
  searchParams.set("dimension", params?.dimension || "service");
  if (params?.start_date) searchParams.set("start_date", params.start_date);
  if (params?.end_date) searchParams.set("end_date", params.end_date);
  if (params?.user_id) searchParams.set("user_id", params.user_id);
  if (params?.model) searchParams.set("model", params.model);
  if (params?.service_id) searchParams.set("service_id", params.service_id);
  if (params?.provider) searchParams.set("provider", params.provider);
  if (params?.limit) searchParams.set("limit", String(params.limit));

  const response = await api.get<FailureBreakdownResponse>(`/api/v1/usage/failure-breakdown?${searchParams}`, { signal });
  return response.data;
}

export async function getRequestTrace(requestId: string, signal?: AbortSignal): Promise<RequestTrace> {
  const response = await api.get<RequestTrace>(`/api/v1/usage/traces/${encodeURIComponent(requestId)}`, { signal });
  return response.data;
}

export async function listRequestTraces(params?: RequestQuery<"GET /api/v1/usage/traces">, signal?: AbortSignal): Promise<RequestTrace[]> {
  const searchParams = new URLSearchParams();
  if (params?.start_date) searchParams.set("start_date", params.start_date);
  if (params?.end_date) searchParams.set("end_date", params.end_date);
  if (params?.service_id) searchParams.set("service_id", params.service_id);
  if (params?.user_id) searchParams.set("user_id", params.user_id);
  if (params?.provider) searchParams.set("provider", params.provider);
  if (params?.model) searchParams.set("model", params.model);
  if (params?.status) searchParams.set("status", params.status);
  if (params?.error_type) searchParams.set("error_type", params.error_type);
  if (params?.limit) searchParams.set("limit", String(params.limit));
  const query = searchParams.toString();
  const response = await api.get<RequestTrace[]>(`/api/v1/usage/traces${query ? `?${query}` : ""}`, { signal });
  return response.data;
}

export async function getQuotaUsersOverview(params?: RequestQuery<"GET /api/v1/quota/users-overview">): Promise<QuotaUsersOverviewResponse> {
  const searchParams = new URLSearchParams();
  if (params?.limit) searchParams.set("limit", String(params.limit));
  if (params?.sort_by) searchParams.set("sort_by", params.sort_by);

  const query = searchParams.toString();
  const response = await api.get<QuotaUsersOverviewResponse>(
    `/api/v1/quota/users-overview${query ? `?${query}` : ""}`
  );
  return response.data;
}
