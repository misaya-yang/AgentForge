import type { EvalAgentVersionTargetSnapshot, TraceFamily } from "@/api/eval";

export function buildExperimentTargetConfig(
  draft: Record<string, unknown>,
  traceFamily: TraceFamily,
): Record<string, unknown> {
  return { trace_family: traceFamily, ...draft };
}

export function agentVersionTarget(value: unknown): EvalAgentVersionTargetSnapshot | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const target = value as Record<string, unknown>;
  if (
    target.candidate_type !== "agent_version"
    || typeof target.agent_id !== "string" || !target.agent_id
    || typeof target.agent_version_id !== "string" || !target.agent_version_id
  ) return null;
  return {
    candidate_type: "agent_version",
    agent_id: target.agent_id,
    agent_version_id: target.agent_version_id,
    agent_spec_hash: typeof target.agent_spec_hash === "string" ? target.agent_spec_hash : undefined,
    agent_runtime_snapshot_hash: typeof target.agent_runtime_snapshot_hash === "string" ? target.agent_runtime_snapshot_hash : undefined,
    model_id: typeof target.model_id === "string" ? target.model_id : undefined,
    knowledge_dataset_ids: Array.isArray(target.knowledge_dataset_ids)
      ? target.knowledge_dataset_ids.filter((item): item is string => typeof item === "string")
      : undefined,
  };
}
