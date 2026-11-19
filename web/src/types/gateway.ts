import type { RequestBody, Defined } from "@/api/contracts";
export type ContentType =
  | "text"
  | "image"
  | "audio"
  | "video"
  | "file"
  | "json"
  | "tool_call"
  | "tool_result";

export type StreamEventType =
  | "approval_required"
  | "approval_result"
  | "text_delta"
  | "tool_call_start"
  | "tool_call_delta"
  | "tool_call_end"
  | "tool_result"
  | "thinking"
  | "final"
  | "stream_end";

export interface ToolCall {
  tool_call_id: string;
  name: string;
  arguments: string;
  status: "pending" | "running" | "completed" | "error";
}

export interface StreamChunk {
  request_id: string;
  chunk_index: number;
  content: ContentItem;
  is_final: boolean;
  event_type: StreamEventType;
  tool_call?: ToolCall;
  metadata?: Record<string, unknown>;
}

export type InvocationMode = "sync" | "async" | "stream" | "webhook";
export type ServiceType =
  | "assistant"
  | "conversational"
  | "generative"
  | "processing"
  | "embedding"
  | "classification"
  | "langgraph"
  | "proxy"
  | "custom";

export interface ContentItem {
  type: ContentType;
  data?: string;
  url?: string;
  mime_type?: string;
  metadata?: Record<string, unknown>;
}

export type UnifiedRequest = Defined<RequestBody<"POST /api/v1/invoke">, "service_id" | "inputs">;

export interface UnifiedResponse {
  request_id: string;
  status: "success" | "error" | "pending";
  outputs: ContentItem[];
  session_id?: string;
  task_id?: string;
  usage?: Record<string, unknown>;
  error?: Record<string, unknown>;
  metadata?: Record<string, unknown>;
  timestamp?: string;
}

// 服务级别配置类型
export interface ServiceRateLimitConfig {
  enabled: boolean;
  requests: number;
  window: number;
  burst: number;
  strategy: string;
}

export interface ServiceAuthConfig {
  enabled: boolean;
  require_auth: boolean;
  allowed_roles: string[];
  allowed_api_keys: string[];
  public: boolean;
}

export interface ServiceCacheConfig {
  enabled: boolean;
  ttl: number;
  semantic_cache: boolean;
}

export interface ServicePriorityConfig {
  priority: number;
  weight: number;
  max_queue_size: number;
}

export interface ServiceCapacityConfig {
  upstream_group?: string | null;
  concurrency_limit?: number | null;
  queue_max: number;
  queue_timeout_ms: number;
  enforced?: boolean;
}

export interface ServiceConfig {
  rate_limit: ServiceRateLimitConfig;
  auth: ServiceAuthConfig;
  cache: ServiceCacheConfig;
  priority: ServicePriorityConfig;
  capacity?: ServiceCapacityConfig;
}

export interface ServiceModelOverride {
  enabled: boolean;
  provider_id?: string;
  model_id?: string;
  temperature?: number | null;
  cache_epoch?: number | string;
  failover?: ServiceModelFailoverConfig;
}

export interface ServiceModelFailoverCandidate {
  provider_id?: string;
  model_id?: string;
}

export interface ServiceModelFailoverConfig {
  enabled: boolean;
  max_attempts?: number;
  candidates: ServiceModelFailoverCandidate[];
}

export interface ServiceDefinition {
  service_id: string;
  name: string;
  display_name?: string;
  description?: string;
  version?: string;
  service_type: ServiceType;
  supported_modes: InvocationMode[];
  accepted_content_types: ContentType[];
  output_content_types: ContentType[];
  status: string;
  tags?: string[];
  metadata?: Record<string, unknown>;
  // 服务级别配置
  service_config?: ServiceConfig;
}

export interface ServiceUiPreferences {
  hide_thinking?: boolean;
  hide_timeline?: boolean;
  tool_calls_mode?: "collapsed" | "hidden" | "full";
  tool_calls_default_open?: boolean;
}

export interface ServiceDetail extends ServiceDefinition {
  connector_type: string;
  connector_config: Record<string, unknown>;
  session_enabled?: boolean;
  session_adapter?: string | null;
  timeout?: number;
  max_retries?: number;
  retry_delay?: number;
  circuit_breaker_enabled?: boolean;
  failure_threshold?: number;
  recovery_timeout?: number;
  rate_limit?: Record<string, unknown> | null;
  concurrency_limit?: number | null;
  input_schema?: Record<string, unknown>;
  output_schema?: Record<string, unknown>;
  async_config?: Record<string, unknown> | null;
}

export interface HealthStatus {
  status: string;
  latency?: number;
  last_check?: string;
  error?: string | null;
  qps?: number;
  avg_latency_ms?: number;
  error_rate?: number;
}

export interface Task {
  task_id: string;
  request_id: string;
  service_id: string;
  status: string;
  created_at: string;
  updated_at?: string;
  result?: unknown;
  error?: string;
  progress?: number;
}
