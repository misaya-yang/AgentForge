import type { RequestBody } from "@/api/contracts";
export type AgentRole = "owner" | "editor" | "viewer";
export type AgentStatus = "draft" | "active" | "archived" | "deleted";
export type AgentChannel = "hosted" | "embed" | "api";
export type AgentAuthMode = "private" | "tenant" | "public" | "token";
export type AgentReleaseStatus =
  | "queued"
  | "running"
  | "passed"
  | "failed"
  | "cancelled"
  | "stale";
export type AgentCapabilityType =
  | "native"
  | "model_native"
  | "mcp"
  | "skill"
  | "connector";

export interface AgentIdentitySpec {
  icon_url?: string | null;
  theme_color?: string | null;
  welcome_message: string;
  suggested_prompts: string[];
}

export interface AgentModelSpec {
  model_id: string;
  provider_id?: string | null;
  temperature?: number | null;
  max_tokens?: number | null;
  thinking_mode?: string | null;
}

export interface AgentOutcomeSpec {
  goal: string;
  triggers: string[];
  inputs: string[];
  human_boundaries: string[];
  success_criteria: string[];
}

export interface AgentCapabilityBinding {
  type: AgentCapabilityType;
  resource_id: string;
  resource_version?: string | null;
  schema_hash?: string | null;
  config: Record<string, unknown>;
}

export interface AgentKnowledgeBinding {
  dataset_id: string;
  retrieval_config: {
    mode?: "auto" | "tool" | "off";
    top_k?: number;
    threshold?: number;
    include_images?: boolean;
  };
}

export interface AgentSpec {
  schema_version: "agent-spec/v1";
  identity: AgentIdentitySpec;
  outcome: AgentOutcomeSpec;
  instructions: string;
  model: AgentModelSpec;
  capabilities: AgentCapabilityBinding[];
  knowledge: AgentKnowledgeBinding[];
  memory: Record<string, unknown>;
}

export interface AgentSummary {
  tenant_id: string;
  agent_id: string;
  slug: string;
  name: string;
  description: string;
  owner_id: string;
  status: AgentStatus;
  caller_role: AgentRole;
  draft_revision: number | null;
  created_at: string;
  updated_at: string;
}

export interface AgentDetail extends AgentSummary {
  draft?: {
    revision: number;
    schema_version: string;
    spec_hash: string;
    updated_at: string;
  } | null;
  archived_at?: string | null;
  deleted_at?: string | null;
}

export interface AgentDraft {
  tenant_id: string;
  draft_id: string;
  agent_id: string;
  revision: number;
  schema_version: string;
  spec: AgentSpec;
  spec_hash: string;
  updated_by: string;
  created_at: string;
  updated_at: string;
}

export interface AgentVersion {
  tenant_id: string;
  agent_version_id: string;
  agent_id: string;
  version_number: number;
  schema_version: string;
  spec: AgentSpec;
  spec_hash: string;
  source_draft_id: string;
  source_draft_revision: number;
  release_evaluation_id?: string | null;
  release_identity_hash?: string | null;
  created_by: string;
  created_at: string;
}

export interface AgentChannelPolicy {
  attachments: boolean;
  high_risk_tools: boolean;
  allowed_origins: string[];
  expires_at?: string | null;
  requests_per_minute?: number;
  requests_per_day?: number;
  ip_requests_per_minute?: number;
  ip_requests_per_day?: number;
  publication_requests_per_minute?: number;
  publication_requests_per_day?: number;
}

export interface AgentApiToken {
  token_id: string;
  publication_id: string;
  name: string;
  scopes: string[];
  expires_at: string | null;
  revoked_at: string | null;
  last_used_at: string | null;
  rotated_from_token_id: string | null;
  created_by: string;
  created_at: string;
}

export interface AgentApiTokenIssue {
  token: string;
  token_metadata: AgentApiToken;
  request_id: string;
}

export interface AgentReleaseFinding {
  code: string;
  field: string;
  message: string;
}

export interface AgentReleaseEvaluationEvent {
  event_id: string;
  evaluation_id: string;
  sequence: number;
  status: AgentReleaseStatus;
  summary: Record<string, unknown>;
  created_at: string;
}

export interface AgentReleaseGate {
  schema_version?: string;
  status: AgentReleaseStatus;
  profile_id?: string;
  profile_version?: string;
  execution_scope?: string;
  model_quality_evaluated?: boolean;
  blocking_findings: AgentReleaseFinding[];
  non_blocking_findings: AgentReleaseFinding[];
  metrics?: {
    critical_pass_rate?: number;
    configured_critical_pass_rate?: number;
    validation_duration_ms?: number;
    provider_cost_cents?: number;
    evaluator_results?: Array<{
      evaluator: string;
      blocking: boolean;
      status: "passed" | "failed";
    }>;
  };
}

export interface AgentReleaseEvaluation {
  tenant_id: string;
  evaluation_id: string;
  agent_id: string;
  draft_id: string;
  draft_revision: number;
  spec_hash: string;
  runtime_fingerprint: Record<string, string>;
  runtime_fingerprint_hash: string;
  release_identity_hash: string;
  evaluation_identity_hash?: string | null;
  profile_id: string;
  profile_version: string;
  dataset_id: string | null;
  dataset_version?: string | null;
  dataset_manifest_hash?: string | null;
  experiment_run_id: string | null;
  channel: AgentChannel;
  auth_mode: AgentAuthMode;
  channel_policy: AgentChannelPolicy;
  channel_policy_hash: string;
  status: AgentReleaseStatus;
  stale: boolean;
  stale_reasons: string[];
  validation_snapshot: Record<string, unknown>;
  gate_snapshot: AgentReleaseGate;
  events: AgentReleaseEvaluationEvent[];
  created_by: string;
  created_at: string;
  started_at?: string | null;
  completed_at?: string | null;
}

export interface AgentEvalDataset {
  dataset_id: string;
  tenant_id: string;
  name: string;
  description: string;
  version: string;
  schema: Record<string, unknown>;
  metadata: Record<string, unknown>;
  updated_at?: string | null;
}

export interface AgentReleaseDiffSection {
  changed: boolean;
  before_hash: string;
  after_hash: string;
  changed_paths: string[];
  before_length?: number;
  after_length?: number;
  before?: unknown;
  after?: unknown;
}

export interface AgentReleaseDiff {
  evaluation_id: string;
  draft_revision: number;
  publication_id: string | null;
  current_version_id: string | null;
  current_version_number: number | null;
  diff: {
    schema_version: string;
    changed_sections: string[];
    sections: Record<string, AgentReleaseDiffSection>;
  };
}

export interface AgentPublication {
  tenant_id: string;
  publication_id: string;
  agent_id: string;
  channel: AgentChannel;
  public_id: string;
  version_id: string | null;
  version_number?: number | null;
  version_spec_hash?: string | null;
  auth_mode: AgentAuthMode;
  policy: AgentChannelPolicy;
  expires_at?: string | null;
  status: "draft" | "active" | "disabled" | "degraded";
  created_by: string;
  updated_by: string;
  created_at: string;
  updated_at: string;
}

export interface AgentPublishEvent {
  event_id: string;
  publication_id: string;
  agent_id: string;
  from_version_id: string | null;
  to_version_id: string;
  actor_id: string;
  reason: string;
  validation_snapshot: Record<string, unknown>;
  operation: "promote" | "rollback";
  release_evaluation_id: string | null;
  request_hash: string | null;
  created_at: string;
}

export interface AgentReleaseMutation {
  request_id: string;
  version: AgentVersion;
  publication: AgentPublication;
  event: AgentPublishEvent;
  idempotent_replay: boolean;
}

export interface AgentPage {
  items: AgentSummary[];
  next_cursor: string | null;
}

export interface AgentRuntimeSession {
  session_id: string;
  agent_id: string;
  agent_version_id: string | null;
  draft_revision: number | null;
  publication_id: string | null;
  channel: "preview" | "hosted" | "embed" | "api";
  runtime_fingerprint: string;
  request_id: string;
}

export interface AgentStreamEvent {
  event?: string;
  event_type?: string;
  content?: string;
  message?: string;
  tool_name?: string;
  status?: string;
  duration_ms?: number;
  dataset_id?: string;
  dataset_name?: string;
  citation_count?: number;
  data?: Record<string, unknown> | string;
  [key: string]: unknown;
}

export interface AgentCatalogTool {
  id: string;
  name: string;
  title?: string;
  summary?: string;
  description: string;
  category: string;
  risk: string;
  capabilityKind?: string;
  mcpServer?: string;
  mcpTool?: string;
  effect?: string;
  approvalPolicy?: string;
  readOnly?: boolean;
}

export interface AgentCatalogMcpTool {
  tool_id: string;
  server_id: string;
  server_name: string;
  runtime_name: string;
  description: string;
  snapshot_id: string;
  schema_hash: string;
  risk_level: "low" | "medium" | "high" | "critical";
  connection_id: string | null;
  principal_type: "service_account" | "user_delegated" | null;
  enabled: boolean;
}

export interface AgentCatalogSkill {
  name: string;
  title?: string;
  description?: string;
  version?: string;
  version_id?: string;
  content_hash?: string;
  enabled?: boolean;
  status?: string;
  source?: string;
}

export interface AgentCatalogConnector {
  provider: string;
  display_name: string;
  description?: string | null;
  connected: boolean;
  grant_id?: string;
  principal_type?: "service_account" | "user_delegated";
  scopes?: string[];
  allowed_channels?: string[];
}

export interface AgentOperationsTrace {
  trace_id: string;
  agent_id: string;
  agent_version_id: string | null;
  publication_id: string | null;
  channel: AgentChannel | "preview" | "builtin";
  session_id: string | null;
  status: string;
  model_id: string | null;
  total_latency_ms: number;
  total_tokens: number;
  total_cost_cents: number;
  input_preview: string;
  output_preview: string;
  redaction_state: Record<string, unknown>;
  started_at: string | null;
  created_at: string | null;
}

export interface AgentOperationsMetrics {
  total_runs?: number;
  succeeded_runs?: number;
  failed_runs?: number;
  sessions?: number;
  success_rate?: number | null;
  avg_latency_ms?: number;
  p50_latency_ms?: number;
  p95_latency_ms?: number;
  avg_ttft_ms?: number;
  p50_ttft_ms?: number;
  p95_ttft_ms?: number;
  total_tokens?: number;
  total_cost_cents?: number;
  tool_calls?: number;
  tool_succeeded?: number;
  tool_success_rate?: number | null;
  knowledge_queries?: number;
  knowledge_hits?: number;
  knowledge_hit_rate?: number | null;
  feedback_count?: number;
  positive_feedback_count?: number;
  feedback_positive_rate?: number | null;
  oldest_trace_at?: string | null;
  newest_trace_at?: string | null;
  retention_limited?: boolean;
  retention?: {
    trace_retention_days?: number;
    legal_hold?: boolean;
    last_retention_cleanup_at?: string | null;
  };
  breakdown?: Array<Record<string, unknown>>;
}

export interface AgentAnalyticsResponse {
  agent_id: string;
  caller_role: AgentRole;
  metrics: AgentOperationsMetrics;
  traces: AgentOperationsTrace[];
  total: number;
  limit: number;
  offset: number;
  filters: Record<string, unknown>;
}

export interface AgentAuditEvent {
  id: number;
  user_id: string | null;
  action: string;
  status: string;
  agent_id: string;
  agent_version_id: string | null;
  publication_id: string | null;
  channel: string | null;
  request_summary: Record<string, unknown>;
  response_summary: Record<string, unknown>;
  redaction_state: Record<string, unknown>;
  created_at: string;
}

export interface AgentAuditPage {
  events: AgentAuditEvent[];
  total: number;
  limit: number;
  offset: number;
}

export interface AgentGovernancePolicy {
  tenant_id: string;
  agent_id: string;
  trace_retention_days: number;
  runtime_retention_days: number;
  attachment_retention_days: number;
  legal_hold: boolean;
  principal_requests_per_minute: number;
  principal_requests_per_day: number;
  ip_requests_per_minute: number;
  ip_requests_per_day: number;
  publication_requests_per_minute: number;
  publication_requests_per_day: number;
  max_agents_per_tenant: number;
  max_active_publications: number;
  max_concurrent_runs: number;
  max_daily_tokens: number;
  max_daily_mcp_calls: number;
  max_storage_bytes: number;
  alert_threshold_percent: number;
  cache_epoch: number;
  updated_by: string;
  created_at: string | null;
  updated_at: string | null;
}

export type AgentGovernancePolicyUpdate = RequestBody<"PUT /api/v1/agents/{agent}/governance">;

export interface AgentDataDeletion {
  deletion_id: string;
  tenant_id: string;
  agent_id: string;
  scope: "retention" | "user" | "tenant";
  subject_user_id: string | null;
  status: "pending" | "completed" | "failed" | "blocked";
  deleted_counts: Record<string, number>;
  error_code: string | null;
  requested_by: string;
  requested_at: string;
  attempt_count: number;
  last_attempt_at: string | null;
  completed_at: string | null;
}

export interface AgentApiErrorDetail {
  code?: string;
  message?: string;
  request_id?: string;
  current_revision?: number;
  errors?: Array<{ field: string; code: string; message: string }>;
  findings?: AgentReleaseFinding[];
}

export const DEFAULT_AGENT_INSTRUCTIONS =
  "Fulfill the Agent purpose described in its name and description. Follow the configured capability, knowledge, memory, and safety policies.";

export function createDefaultAgentSpec(): AgentSpec {
  return {
    schema_version: "agent-spec/v1",
    identity: {
      icon_url: null,
      theme_color: "#7B7BE8",
      welcome_message: "",
      suggested_prompts: [],
    },
    outcome: {
      goal: "",
      triggers: [],
      inputs: [],
      human_boundaries: [],
      success_criteria: [],
    },
    instructions: DEFAULT_AGENT_INSTRUCTIONS,
    model: {
      model_id: "", // empty → the server applies its deployment default
      provider_id: "dashscope",
      temperature: 0.3,
      max_tokens: 4096,
    },
    capabilities: [],
    knowledge: [],
    memory: { mode: "session" },
  };
}

export function normalizeAgentSpec(spec: Partial<AgentSpec>): AgentSpec {
  const defaults = createDefaultAgentSpec();
  return {
    ...defaults,
    ...spec,
    identity: { ...defaults.identity, ...(spec.identity ?? {}) },
    outcome: { ...defaults.outcome, ...(spec.outcome ?? {}) },
    model: { ...defaults.model, ...(spec.model ?? {}) },
    capabilities: Array.isArray(spec.capabilities) ? spec.capabilities : [],
    knowledge: Array.isArray(spec.knowledge) ? spec.knowledge : [],
    memory: spec.memory && typeof spec.memory === "object" ? spec.memory : defaults.memory,
  };
}
