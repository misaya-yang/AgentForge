import { api } from "@/lib/api";
import { sseFetch } from "@/lib/sse";
import { useAuthStore } from "@/store/useAuthStore";
import type {
  AgentCatalogConnector,
  AgentCatalogMcpTool,
  AgentCatalogSkill,
  AgentCatalogTool,
  AgentApiToken,
  AgentApiTokenIssue,
  AgentAnalyticsResponse,
  AgentAuditPage,
  AgentDataDeletion,
  AgentDetail,
  AgentDraft,
  AgentEvalDataset,
  AgentPage,
  AgentPublication,
  AgentPublishEvent,
  AgentReleaseDiff,
  AgentReleaseEvaluation,
  AgentReleaseFinding,
  AgentReleaseMutation,
  AgentGovernancePolicy,
  AgentGovernancePolicyUpdate,
  AgentRuntimeSession,
  AgentSpec,
  AgentStreamEvent,
  AgentSummary,
  AgentVersion,
} from "@/types/agents";
import type { Dataset } from "@/types/knowledge";
import type { ModelInfo } from "@/api/assistant";

export interface ListAgentsParams {
  limit?: number;
  cursor?: string;
  status?: string;
  owner_id?: string;
  search?: string;
  channel?: string;
}

export interface AgentAnalyticsFilters {
  agent_version_id?: string;
  publication_id?: string;
  channel?: string;
  started_after?: string;
  started_before?: string;
  status?: string;
  limit?: number;
  offset?: number;
}

export async function listAgents(params: ListAgentsParams = {}, signal?: AbortSignal): Promise<AgentPage> {
  const { data } = await api.get<AgentPage>("/api/v1/agents", { params, signal });
  return data;
}

export async function getAgent(agentId: string): Promise<AgentDetail> {
  const { data } = await api.get<AgentDetail>(`/api/v1/agents/${agentId}`);
  return data;
}

export async function getAgentAnalytics(
  agentId: string,
  params: AgentAnalyticsFilters = {},
): Promise<AgentAnalyticsResponse> {
  const { data } = await api.get<AgentAnalyticsResponse>(
    `/api/v1/agents/${agentId}/analytics`,
    { params },
  );
  return data;
}

export async function listAgentAuditEvents(
  agentId: string,
  params: AgentAnalyticsFilters & { action?: string } = {},
): Promise<AgentAuditPage> {
  const { data } = await api.get<AgentAuditPage>(
    `/api/v1/agents/${agentId}/audit-events`,
    { params },
  );
  return data;
}

export async function getAgentGovernance(agentId: string): Promise<AgentGovernancePolicy> {
  const { data } = await api.get<AgentGovernancePolicy>(
    `/api/v1/agents/${agentId}/governance`,
  );
  return data;
}

export async function updateAgentGovernance(
  agentId: string,
  input: AgentGovernancePolicyUpdate,
): Promise<AgentGovernancePolicy> {
  const { data } = await api.put<AgentGovernancePolicy>(
    `/api/v1/agents/${agentId}/governance`,
    input,
  );
  return data;
}

export async function invalidateAgentCache(
  agentId: string,
): Promise<{ request_id: string; cache_epoch: number; deleted_cache_rows: number }> {
  const { data } = await api.post(
    `/api/v1/agents/${agentId}/governance/cache:invalidate`,
    {},
  );
  return data;
}

export async function revokeAllAgentCredentials(
  agentId: string,
): Promise<{ request_id: string; revoked: Record<string, number> }> {
  const { data } = await api.post(
    `/api/v1/agents/${agentId}/governance/credentials:revoke`,
    {},
  );
  return data;
}

export async function requestAgentDataDeletion(
  agentId: string,
  input: {
    scope: "retention" | "user" | "tenant";
    subject_user_id?: string;
    idempotency_key: string;
  },
): Promise<AgentDataDeletion> {
  const { data } = await api.post<AgentDataDeletion>(
    `/api/v1/agents/${agentId}/governance/data-deletions`,
    input,
  );
  return data;
}

export async function createAgent(payload: {
  name: string;
  slug?: string;
  description: string;
  spec: AgentSpec;
}): Promise<AgentSummary> {
  const { data } = await api.post<{ agent: AgentSummary }>("/api/v1/agents", payload);
  return data.agent;
}


export async function copyAgent(agentId: string): Promise<AgentSummary> {
  const { data } = await api.post<{ agent: AgentSummary }>(
    `/api/v1/agents/${agentId}/copy`,
    {},
  );
  return data.agent;
}

export async function archiveAgent(agentId: string): Promise<AgentSummary> {
  const { data } = await api.post<{ agent: AgentSummary }>(
    `/api/v1/agents/${agentId}/archive`,
    { disable_publications: false },
  );
  return data.agent;
}

export async function getAgentDraft(agentId: string): Promise<AgentDraft> {
  const { data } = await api.get<AgentDraft>(`/api/v1/agents/${agentId}/draft`);
  return data;
}

export async function updateAgentDraft(
  agentId: string,
  revision: number,
  spec: AgentSpec,
  agentChanges: { name?: string; description?: string } = {},
): Promise<AgentDraft> {
  const { data } = await api.put<{ draft: AgentDraft }>(
    `/api/v1/agents/${agentId}/draft`,
    { spec, ...agentChanges },
    { headers: { "If-Match": `"${revision}"` } },
  );
  return data.draft;
}

export async function listAgentVersions(agentId: string): Promise<AgentVersion[]> {
  const { data } = await api.get<AgentVersion[]>(`/api/v1/agents/${agentId}/versions`);
  return data;
}

export async function createAgentVersion(agentId: string, draftRevision: number): Promise<AgentVersion> {
  const { data } = await api.post<{ version: AgentVersion }>(
    `/api/v1/agents/${agentId}/versions`,
    {},
    { headers: { "If-Match": `"${draftRevision}"` } },
  );
  return data.version;
}

export async function runAgentReleaseEvaluation(
  agentId: string,
  input: {
    draft_revision: number;
    dataset_id?: string | null;
    channel: "hosted" | "embed" | "api";
    auth_mode: "private" | "tenant" | "public" | "token";
    channel_policy: {
      attachments: boolean;
      high_risk_tools: boolean;
      allowed_origins: string[];
      expires_at?: string | null;
    };
  },
): Promise<AgentReleaseEvaluation> {
  const { data } = await api.post<AgentReleaseEvaluation>(
    `/api/v1/agents/${agentId}/evals`,
    input,
  );
  return data;
}

export async function executeAgentReleaseEvaluation(
  agentId: string,
  evaluationId: string,
): Promise<AgentReleaseEvaluation> {
  const { data } = await api.post<AgentReleaseEvaluation>(
    `/api/v1/agents/${agentId}/evals/${evaluationId}/execute`,
    {},
  );
  return data;
}

export async function cancelAgentReleaseEvaluation(
  agentId: string,
  evaluationId: string,
): Promise<AgentReleaseEvaluation> {
  const { data } = await api.post<AgentReleaseEvaluation>(
    `/api/v1/agents/${agentId}/evals/${evaluationId}/cancel`,
    {},
  );
  return data;
}

export async function listAgentReleaseEvaluations(
  agentId: string,
): Promise<AgentReleaseEvaluation[]> {
  const { data } = await api.get<{ evaluations: AgentReleaseEvaluation[] }>(
    `/api/v1/agents/${agentId}/evals`,
  );
  return data.evaluations;
}

export async function getAgentReleaseDiff(
  agentId: string,
  evaluationId: string,
): Promise<AgentReleaseDiff> {
  const { data } = await api.get<AgentReleaseDiff>(
    `/api/v1/agents/${agentId}/evals/${evaluationId}/diff`,
  );
  return data;
}

export async function publishAgent(
  agentId: string,
  evaluationId: string,
  idempotencyKey: string,
  reason: string,
  experimentRunId?: string,
): Promise<AgentReleaseMutation> {
  const { data } = await api.post<AgentReleaseMutation>(
    `/api/v1/agents/${agentId}/publish`,
    { evaluation_id: evaluationId, reason, ...(experimentRunId ? { experiment_run_id: experimentRunId } : {}) },
    { headers: { "Idempotency-Key": idempotencyKey } },
  );
  return data;
}

export async function listAgentPublications(agentId: string): Promise<AgentPublication[]> {
  const { data } = await api.get<AgentPublication[]>(
    `/api/v1/agents/${agentId}/publications`,
  );
  return data;
}

export async function listAgentPublishEvents(agentId: string): Promise<AgentPublishEvent[]> {
  const { data } = await api.get<AgentPublishEvent[]>(
    `/api/v1/agents/${agentId}/publish-events`,
  );
  return data;
}

export async function listAgentApiTokens(publicationId: string): Promise<AgentApiToken[]> {
  const { data } = await api.get<{ tokens: AgentApiToken[] }>(
    `/api/v1/publications/${publicationId}/tokens`,
  );
  return data.tokens;
}

export async function createAgentApiToken(
  publicationId: string,
  input: { name: string; scopes: string[]; expires_at?: string | null },
): Promise<AgentApiTokenIssue> {
  const { data } = await api.post<AgentApiTokenIssue>(
    `/api/v1/publications/${publicationId}/tokens`,
    input,
  );
  return data;
}

export async function rotateAgentApiToken(
  publicationId: string,
  tokenId: string,
): Promise<AgentApiTokenIssue> {
  const { data } = await api.post<AgentApiTokenIssue>(
    `/api/v1/publications/${publicationId}/tokens/${tokenId}/rotate`,
    {},
  );
  return data;
}

export async function revokeAgentApiToken(
  publicationId: string,
  tokenId: string,
): Promise<AgentApiToken> {
  const { data } = await api.delete<AgentApiToken>(
    `/api/v1/publications/${publicationId}/tokens/${tokenId}`,
  );
  return data;
}

export async function rollbackAgentPublication(
  publicationId: string,
  targetVersionId: string,
  idempotencyKey: string,
  reason: string,
): Promise<AgentReleaseMutation> {
  const { data } = await api.post<AgentReleaseMutation>(
    `/api/v1/publications/${publicationId}/rollback`,
    { target_version_id: targetVersionId, reason },
    { headers: { "Idempotency-Key": idempotencyKey } },
  );
  return data;
}

export async function createDraftPreviewSession(
  agentId: string,
  draftRevision: number,
): Promise<AgentRuntimeSession> {
  const { data } = await api.post<AgentRuntimeSession>(
    `/api/v1/agents/${agentId}/preview/sessions`,
    { draft_revision: draftRevision },
  );
  return data;
}

export async function createVersionPreviewSession(
  agentId: string,
  versionId: string,
): Promise<AgentRuntimeSession> {
  const { data } = await api.post<AgentRuntimeSession>(
    `/api/v1/agents/${agentId}/versions/${versionId}/preview/sessions`,
    {},
  );
  return data;
}

export async function* streamAgentPreview(input: {
  agentId: string;
  draftRevision?: number;
  versionId?: string;
  sessionId: string;
  message: string;
  signal?: AbortSignal;
}): AsyncGenerator<AgentStreamEvent, void, void> {
  const token = useAuthStore.getState().token;
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  if (token) headers.Authorization = `Bearer ${token}`;
  const url = input.versionId
    ? `/api/v1/agents/${input.agentId}/versions/${input.versionId}/preview/chat/stream`
    : `/api/v1/agents/${input.agentId}/preview/chat/stream`;
  const body: Record<string, unknown> = {
    message: input.message,
    session_id: input.sessionId,
    attachments: [],
  };
  if (!input.versionId) body.draft_revision = input.draftRevision;
  for await (const event of sseFetch<AgentStreamEvent>(url, {
    method: "POST",
    headers,
    body: JSON.stringify(body),
    signal: input.signal,
  })) {
    yield event;
  }
}

export async function listAgentModels(): Promise<ModelInfo[]> {
  const { data } = await api.get<{ models: ModelInfo[] }>("/api/v1/assistant/models");
  return data.models;
}

export async function listAgentDatasets(): Promise<Dataset[]> {
  const { data } = await api.get<Dataset[]>("/api/v1/knowledge/datasets");
  return data;
}

export async function listAgentEvalDatasets(): Promise<AgentEvalDataset[]> {
  const { data } = await api.get<{ datasets: AgentEvalDataset[] }>(
    "/api/v1/eval/datasets",
    { params: { limit: 200, offset: 0 } },
  );
  return data.datasets;
}

export async function listAgentTools(): Promise<AgentCatalogTool[]> {
  const { data } = await api.get<{
    tools: Array<{
      name: string;
      description: string;
      category: string;
      risk_level: string;
      title?: string;
      summary?: string;
      capability_kind?: string;
      mcp_server?: string;
      mcp_tool?: string;
      effect?: string;
      approval_policy?: string;
      read_only?: boolean;
    }>;
  }>("/api/v1/assistant/tools");
  return data.tools
    .filter((tool) => !["mcp", "skill", "platform_tool_discovery"].includes(tool.capability_kind || ""))
    .map((tool) => ({
      id: tool.name,
      name: tool.name,
      title: tool.title,
      summary: tool.summary,
      description: tool.description,
      category: tool.category,
      risk: tool.risk_level,
      capabilityKind: tool.capability_kind,
      mcpServer: tool.mcp_server,
      mcpTool: tool.mcp_tool,
      effect: tool.effect,
      approvalPolicy: tool.approval_policy,
      readOnly: tool.read_only,
    }));
}

interface McpServerRecord {
  server_id: string;
  name: string;
  enabled: boolean;
}

interface McpConnectionRecord {
  connection_id: string;
  principal_type: "service_account" | "user_delegated";
  enabled: boolean;
  credential_configured: boolean;
}

export async function listAgentMcpTools(): Promise<AgentCatalogMcpTool[]> {
  const { data } = await api.get<{ servers: McpServerRecord[] }>("/api/v1/mcp/servers");
  const enabledServers = data.servers.filter((server) => server.enabled);
  const rows = await Promise.all(
    enabledServers.map(async (server) => {
      const [toolsResponse, connectionsResponse] = await Promise.all([
        api.get<{
          tools: Array<{
            tool_id: string;
            server_id: string;
            runtime_name: string;
            description: string;
            snapshot_id: string;
            schema_hash: string;
            risk_level: "low" | "medium" | "high" | "critical";
            enabled: boolean;
          }>;
        }>(`/api/v1/mcp/servers/${server.server_id}/tools`),
        api.get<{ connections: McpConnectionRecord[] }>(
          `/api/v1/mcp/servers/${server.server_id}/connections`,
        ),
      ]);
      const connection = connectionsResponse.data.connections.find(
        (item) => item.enabled && item.credential_configured,
      );
      return toolsResponse.data.tools.map((tool) => ({
        ...tool,
        server_name: server.name,
        connection_id: connection?.connection_id ?? null,
        principal_type: connection?.principal_type ?? null,
      }));
    }),
  );
  return rows.flat();
}

export async function listAgentSkills(): Promise<AgentCatalogSkill[]> {
  const { data } = await api.get<{ skills: AgentCatalogSkill[] }>("/api/v1/skills", {
    params: { enabled_only: true },
  });
  return data.skills;
}

interface ConnectorPrincipal {
  grant_id: string;
  principal_type: "service_account" | "user_delegated";
  scopes: string[];
  allowed_channels: string[];
  enabled: boolean;
}

export async function listAgentConnectors(): Promise<AgentCatalogConnector[]> {
  const [available, mine] = await Promise.all([
    api.get<AgentCatalogConnector[]>("/api/v1/connectors/available"),
    api.get<Array<{ provider: string; status?: string }>>("/api/v1/connectors/mine"),
  ]);
  const connected = new Set(mine.data.map((item) => item.provider));
  return Promise.all(
    available.data.map(async (connector) => {
      if (connector.provider !== "confluence" || !connected.has(connector.provider)) {
        return { ...connector, connected: connected.has(connector.provider) };
      }
      try {
        const { data } = await api.get<{ principals: ConnectorPrincipal[] }>(
          `/api/v1/connectors/${connector.provider}/principals`,
        );
        const principal = data.principals.find(
          (item) => item.enabled && item.allowed_channels.includes("preview"),
        );
        return {
          ...connector,
          connected: true,
          grant_id: principal?.grant_id,
          principal_type: principal?.principal_type,
          scopes: principal?.scopes,
          allowed_channels: principal?.allowed_channels,
        };
      } catch {
        return { ...connector, connected: true };
      }
    }),
  );
}

export function agentErrorDetail(error: unknown): {
  status?: number;
  code?: string;
  message: string;
  currentRevision?: number;
  errors?: Array<{ field: string; code: string; message: string }>;
  findings?: AgentReleaseFinding[];
} {
  const candidate = error as {
    message?: string;
    response?: {
      status?: number;
      data?: {
        detail?: {
          code?: string;
          message?: string;
          current_revision?: number;
          errors?: Array<{ field: string; code: string; message: string }>;
          findings?: AgentReleaseFinding[];
        } | string;
      };
    };
  };
  const detail = candidate.response?.data?.detail;
  return {
    status: candidate.response?.status,
    code: typeof detail === "object" ? detail.code : undefined,
    message:
      (typeof detail === "object" ? detail.message : detail) ||
      candidate.message ||
      "The request could not be completed.",
    currentRevision: typeof detail === "object" ? detail.current_revision : undefined,
    errors: typeof detail === "object" ? detail.errors : undefined,
    findings: typeof detail === "object" ? detail.findings : undefined,
  };
}
