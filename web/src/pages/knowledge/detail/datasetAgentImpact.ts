import type { DatasetAgentImpact } from "@/api/knowledge";

function count(value: unknown): value is number {
  return Number.isSafeInteger(value) && Number(value) >= 0;
}

function category(value: unknown): value is { visible: number; hidden: number } {
  return !!value && typeof value === "object" && !Array.isArray(value)
    && count((value as { visible?: unknown }).visible)
    && count((value as { hidden?: unknown }).hidden);
}

export function isDatasetAgentImpact(value: unknown, datasetId: string): value is DatasetAgentImpact {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const impact = value as Partial<DatasetAgentImpact>;
  const counts = impact.counts;
  const agents = impact.visible_agents;
  return impact.dataset_id === datasetId
    && count(impact.hidden_agent_count)
    && Array.isArray(agents)
    && agents.every((agent) => agent && typeof agent.agent_id === "string"
      && agent.agent_id.length > 0 && typeof agent.name === "string"
      && typeof agent.current_draft === "boolean"
      && typeof agent.active_publication === "boolean"
      && count(agent.historical_version_count))
    && new Set(agents.map((agent) => agent.agent_id)).size === agents.length
    && !!counts && category(counts.current_draft)
    && category(counts.active_publication) && category(counts.historical_version)
    && counts.current_draft.visible === agents.filter((agent) => agent.current_draft).length
    && counts.active_publication.visible === agents.filter((agent) => agent.active_publication).length
    && counts.historical_version.visible === agents.filter((agent) => agent.historical_version_count > 0).length
    && counts.current_draft.hidden <= impact.hidden_agent_count
    && counts.active_publication.hidden <= impact.hidden_agent_count
    && counts.historical_version.hidden <= impact.hidden_agent_count;
}

export function canConfirmDatasetImpactDelete(
  permission: string | undefined,
  datasetId: string | undefined,
  impact: DatasetAgentImpact | null
): boolean {
  return permission === "owner" && !!datasetId && impact?.dataset_id === datasetId;
}
