import {
  isSubAgentStatus,
  isSubAgentType,
  type SubAgentEvidence,
  type SubAgentState,
  type SubAgentStatus,
} from "./types";

type JsonRecord = Record<string, unknown>;

const TERMINAL_STATUSES = new Set<SubAgentStatus>([
  "completed",
  "failed",
  "cancelled",
  "blocked",
  "partial",
]);

const SUPPORTED_EVENTS = new Set([
  "subagent_started",
  "subagent_step",
  "subagent_text_delta",
  "subagent_tool_start",
  "subagent_tool_result",
  "subagent_finished",
]);

function asRecord(value: unknown): JsonRecord | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as JsonRecord)
    : undefined;
}

function nonEmptyString(value: unknown, maxLength = 500): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed ? trimmed.slice(0, maxLength) : undefined;
}

function finiteNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function finiteInteger(value: unknown): number | undefined {
  const number = finiteNumber(value);
  return number === undefined ? undefined : Math.max(0, Math.floor(number));
}

function limitationsFrom(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value
    .map((item) => nonEmptyString(item))
    .filter((item): item is string => Boolean(item))
    .slice(0, 20);
}

function mergeLimitations(current: string[] | undefined, incoming: string[]): string[] {
  return [...new Set([...(current ?? []), ...incoming])].slice(0, 20);
}

function usageFrom(value: unknown): Record<string, number> | undefined {
  const record = asRecord(value);
  if (!record) return undefined;
  const usage = Object.fromEntries(
    Object.entries(record).flatMap(([key, rawValue]) => {
      const number = finiteNumber(rawValue);
      return number === undefined ? [] : [[key, Math.max(0, number)]];
    }),
  );
  return Object.keys(usage).length > 0 ? usage : undefined;
}

function evidenceFrom(value: unknown): SubAgentEvidence[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((item) => {
    const record = asRecord(item);
    if (!record) return [];
    return [{
      evidenceId: nonEmptyString(record.evidence_id, 160),
      toolName: nonEmptyString(record.tool_name, 120),
      callId: nonEmptyString(record.call_id, 160),
      status: nonEmptyString(record.status, 40),
      summary: nonEmptyString(record.summary, 500),
    }];
  }).slice(0, 50);
}

function unsupported(kind: "status" | "type", value: unknown): string {
  const rendered = nonEmptyString(value, 80) ?? "missing";
  return `Unsupported sub-agent ${kind}: ${rendered}`;
}

function shell(agentId: string, nowMs: number): SubAgentState {
  return {
    agentId,
    agentType: "task",
    description: "Delegated task",
    status: "running",
    steps: [],
    startedAtMs: nowMs,
  };
}

function identityPatch(data: JsonRecord): Partial<SubAgentState> {
  const patch: Partial<SubAgentState> = {};
  const dispatchIndex = finiteInteger(data.dispatch_index);
  const profileId = nonEmptyString(data.profile_id, 160);
  const profileName = nonEmptyString(data.profile_name, 160);
  const sourcePlugin = nonEmptyString(data.source_plugin, 160);
  const definitionSha256 = nonEmptyString(data.definition_sha256, 128);
  const delegationId = nonEmptyString(data.delegation_id, 160);
  const taskId = nonEmptyString(data.task_id, 160);
  const parentTaskId = nonEmptyString(data.parent_task_id, 160);
  const attemptId = nonEmptyString(data.attempt_id, 160);
  const depth = finiteInteger(data.depth);
  const lineage = Array.isArray(data.lineage)
    ? data.lineage.flatMap((value) => nonEmptyString(value, 160) ?? []).slice(0, 20)
    : undefined;
  if (profileId) patch.profileId = profileId;
  if (profileName) patch.profileName = profileName;
  if (sourcePlugin) patch.sourcePlugin = sourcePlugin;
  if (definitionSha256) patch.definitionSha256 = definitionSha256;
  if (delegationId) patch.delegationId = delegationId;
  if (taskId) patch.taskId = taskId;
  if (parentTaskId) patch.parentTaskId = parentTaskId;
  if (attemptId) patch.attemptId = attemptId;
  if (depth !== undefined) patch.depth = depth;
  if (lineage) patch.lineage = lineage;
  if (dispatchIndex !== undefined) patch.dispatchIndex = dispatchIndex;
  return patch;
}

function patchIdentity(current: SubAgentState, data: JsonRecord): SubAgentState {
  return { ...current, ...identityPatch(data) };
}

function updateAgent(
  agents: SubAgentState[],
  agentId: string,
  nowMs: number,
  update: (current: SubAgentState) => SubAgentState,
): SubAgentState[] {
  const index = agents.findIndex((agent) => agent.agentId === agentId);
  if (index < 0) return [...agents, update(shell(agentId, nowMs))];
  const next = [...agents];
  next[index] = update(agents[index]);
  return next;
}

function parseTerminalStatus(value: unknown): {
  status: SubAgentStatus;
  protocolError?: string;
} {
  if (isSubAgentStatus(value) && TERMINAL_STATUSES.has(value)) return { status: value };
  return {
    status: "failed",
    protocolError: value === "running"
      ? "Invalid terminal sub-agent status: running"
      : unsupported("status", value),
  };
}

/**
 * Idempotent lifecycle reducer for the public sub-agent SSE contract.
 *
 * Lifecycle identities are natural protocol keys (`agent_id`, `call_id`).
 * Terminal state is monotonic and accepted once, so reconnect duplicates or
 * late non-terminal frames cannot resurrect a child. Raw text deltas are
 * intentionally ignored: the workbench displays observable progress and
 * receipts, never private reasoning text.
 */
export function reduceSubAgentEvent(
  agents: SubAgentState[],
  eventType: string,
  rawData: unknown,
  nowMs = Date.now(),
): SubAgentState[] {
  if (!SUPPORTED_EVENTS.has(eventType)) return agents;
  const data = asRecord(rawData);
  if (!data) return agents;
  const agentId = nonEmptyString(data.agent_id, 160);
  if (!agentId) return agents;

  if (eventType === "subagent_started") {
    const rawType = data.agent_type;
    const agentType = isSubAgentType(rawType) ? rawType : "task";
    const typeLimitations = isSubAgentType(rawType) ? [] : [unsupported("type", rawType)];
    return updateAgent(agents, agentId, nowMs, (current) => ({
      ...patchIdentity(current, data),
      agentType,
      description: nonEmptyString(data.description) ?? current.description,
      startedAtMs: current.startedAtMs ?? nowMs,
      startedMonotonicMs: finiteNumber(data.started_monotonic_ms) ?? current.startedMonotonicMs,
      limitations: mergeLimitations(current.limitations, typeLimitations),
    }));
  }

  if (eventType === "subagent_finished") {
    return updateAgent(agents, agentId, nowMs, (unpatched) => {
      const current = patchIdentity(unpatched, data);
      if (TERMINAL_STATUSES.has(current.status)) return current;
      const result = asRecord(data.result);
      const effective = asRecord(data.effective_execution);
      const { status, protocolError } = parseTerminalStatus(data.status);
      const durationMs = finiteNumber(data.duration_ms);
      const turns = finiteInteger(data.turns);
      const toolCalls = finiteInteger(data.tool_calls);
      const resultUsage = usageFrom(result?.usage);
      const effectiveUsage = usageFrom(effective?.usage);
      const toolNames = Array.isArray(effective?.tool_names)
        ? effective.tool_names.flatMap((value) => nonEmptyString(value, 120) ?? []).slice(0, 50)
        : undefined;
      const toolCategories = Array.isArray(effective?.tool_categories)
        ? effective.tool_categories.flatMap((value) => nonEmptyString(value, 80) ?? []).slice(0, 50)
        : undefined;
      const runningStepStatus = status === "partial" ? "partial" : status;

      return {
        ...current,
        status,
        currentStepStatus: current.currentStepStatus === "running"
          ? runningStepStatus
          : current.currentStepStatus,
        steps: current.steps.map((step) => step.status === "running"
          ? { ...step, status: runningStepStatus }
          : step),
        resultSummary: nonEmptyString(data.result_summary, 4_000),
        evidence: evidenceFrom(result?.evidence),
        limitations: mergeLimitations(current.limitations, [
          ...limitationsFrom(result?.limitations),
          ...limitationsFrom(data.limitations),
        ]),
        usage: resultUsage ?? effectiveUsage,
        structuredResult: result?.structured_payload,
        effectiveExecution: effective ? {
          modelId: nonEmptyString(effective.model_id, 160),
          toolNames,
          toolCategories,
          extensions: finiteInteger(effective.extensions),
          stopReason: nonEmptyString(effective.stop_reason, 160),
        } : undefined,
        error: protocolError ?? nonEmptyString(data.error, 1_000),
        durationMs: durationMs === undefined ? current.durationMs : Math.max(0, durationMs),
        turnsCompleted: turns ?? current.turnsCompleted,
        toolCallsMade: toolCalls ?? current.toolCallsMade,
        finishedAtMs: nowMs,
        startedMonotonicMs: finiteNumber(data.started_monotonic_ms) ?? current.startedMonotonicMs,
        finishedMonotonicMs: finiteNumber(data.finished_monotonic_ms),
      };
    });
  }

  if (eventType === "subagent_text_delta") return agents;

  return updateAgent(agents, agentId, nowMs, (unpatched) => {
    const current = patchIdentity(unpatched, data);
    if (TERMINAL_STATUSES.has(current.status)) return current;

    if (eventType === "subagent_step") {
      const step = nonEmptyString(data.step, 240);
      if (!step) return current;
      const rawStatus = data.status ?? "running";
      const stepStatus = isSubAgentStatus(rawStatus) ? rawStatus : "failed";
      const inferredTurn = /^Turn\s+(\d+)/i.exec(step)?.[1];
      const turns = finiteInteger(data.turns_completed)
        ?? (inferredTurn ? Number(inferredTurn) : undefined);
      return {
        ...current,
        currentStep: step,
        currentStepStatus: stepStatus,
        turnsCompleted: turns ?? current.turnsCompleted,
        limitations: mergeLimitations(
          current.limitations,
          isSubAgentStatus(rawStatus) ? [] : [unsupported("status", rawStatus)],
        ),
      };
    }

    if (eventType === "subagent_tool_start") {
      const callId = nonEmptyString(data.call_id, 160);
      if (!callId) return current;
      const toolName = nonEmptyString(data.tool_name, 120) ?? "External tool";
      const existing = current.steps.find((step) => step.callId === callId);
      return {
        ...current,
        steps: existing
          ? current.steps.map((step) => step.callId === callId
            ? { ...step, toolName: step.toolName === "External tool" ? toolName : step.toolName }
            : step)
          : [...current.steps, { callId, toolName, status: "running" }],
        toolCallsMade: existing ? current.toolCallsMade : (current.toolCallsMade ?? 0) + 1,
      };
    }

    if (eventType === "subagent_tool_result") {
      const callId = nonEmptyString(data.call_id, 160);
      if (!callId) return current;
      const toolName = nonEmptyString(data.tool_name, 120) ?? "External tool";
      const status: SubAgentStatus = data.success === true ? "completed" : "failed";
      const durationMs = finiteNumber(data.duration_ms);
      const patch = {
        callId,
        toolName,
        status,
        summary: nonEmptyString(data.summary, 500),
        ...(durationMs === undefined ? {} : { durationMs: Math.max(0, durationMs) }),
      };
      const existing = current.steps.find((step) => step.callId === callId);
      // A tool call result is terminal for that call. Reconnect replays (even
      // conflicting ones) must not rewrite the first observed receipt.
      if (existing && existing.status !== "running") return current;
      return {
        ...current,
        steps: existing
          ? current.steps.map((step) => step.callId === callId ? { ...step, ...patch } : step)
          : [...current.steps, patch],
        toolCallsMade: existing ? current.toolCallsMade : (current.toolCallsMade ?? 0) + 1,
      };
    }

    return current;
  });
}
