/* eslint-disable no-case-declarations, @typescript-eslint/no-explicit-any */

import { useState, useCallback, useRef, useEffect, useLayoutEffect, type SetStateAction } from "react";
import { useArtifactListRecovery } from "./useArtifactListRecovery";
import { useRestoredRunStop } from "./useRestoredRunStop";
import { useTranslation } from "react-i18next";
import {
  approveToolCall,
  cancelTask,
  chatStream,
  getArtifactDownloadUrl,
  getAssistantRunStatus,
  getSessionHistory as getAssistantSessionHistory,
  deleteSession as deleteAssistantSession,
  prepareAssistantRunResume,
  type AssistantMessage,
  type WebSearchResult,
  type ArtifactInfo,
  getSessionArtifacts,
} from "@/api/assistant";
import { SSEEventType } from "../sse-events";
import {
  listSessions,
  createSession,
  updateSession,
  getSession,
  type SessionSummary,
  type SessionConfig,
} from "@/api/sessions";
import { useAppStore } from "@/store/useAppStore";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { generateUUID } from "@/lib/utils";
import {
  applyUsageToTurnState,
  cancelStreamTurn,
  completeStreamTurn,
  createStreamReducerContext,
  createStreamTurnState,
  failStreamTurn,
  markStreamFirstToken,
  reduceLegacyStreamChunk,
  type StreamTurnState,
} from "@/features/chat/stream";
import {
  acceptPendingRunSession,
  confirmsAssistantRunAdmission,
  beginNewChatSession,
  persistNewChatSession,
  startChatWithoutAwaitingSessionCreate,
} from "@/features/chat/newChatStream";
import { createActivityFlushQueue } from "@/features/chat/coalesceUpdates";
import { updateMessageById } from "@/features/chat/messageRenderPerformance";
import {
  ACTIVE_RUN_METADATA_KEY,
  shouldBlockDuringRunRestore,
} from "@/features/chat/sessionRestoreWindow";
import {
  ASSISTANT_VIEW_DETACH_REASON,
  isExpectedApprovalRejection,
} from "@/features/chat/runtimeV2State";
import { mergeKnowledgeContexts } from "@/features/chat/knowledgeContexts";
import {
  createStreamTerminalLatch,
  type StreamTerminalOutcome,
} from "@/features/chat/stream/terminalLatch";
import { reduceSubAgentEvent } from "../subagentEventReducer";
import {
  buildLatestRunOutputFilesFromArtifacts,
  hydrateMessageArtifacts,
} from "../artifactHydration";
import {
  restoredMessageStatus,
  restoredProcessStatus,
} from "../restoredMessageStatus";
import type {
  ChatMessage as ChatMessageType,
  RetrievedContext,
  SearchStatusItem,
  RAGEvaluationEventData,
  RAGEvaluation,
  FileProcessedEventData,
  GeneratedArtifact,
  AgentPhaseStatus,
  ReActPhase,
  ProcessSummaryState,
  ProcessStepItem,
  ProcessStepStatus,
  ToolTimelineItem,
  // Agentic types
  TaskPlanningEventData,
  WorkingMemoryUpdateEventData,
  // Manus-style outline types
  OutlineReadyEventData,
  // Working memory and code execution state types
  WorkingMemory,
  CodeExecutionState,
} from "../types";
import { getQuiz } from "@/api/quiz";
import { getStyleSystemPrompt } from "../styles";
import type { Artifact } from "@/components/artifacts";
import {
  finishChatStreamTrace,
  markChatStreamFirstTextToken,
  markChatStreamFirstToken,
  startChatStreamTrace,
  trackChatHistoryRestored,
} from "@/features/chat/telemetry";
import { loadDeadApprovalReconciliation } from "../deadApprovalReconciliation";
import { redactRestrictedSourceMessages, redactSourceMessage } from "../sourceVisibility";
import { cancelledReceiptFlags } from "../cancelledReceipt";
import { getAgentRuntimeThread } from "@/api/agentThreads";

function buildTextParts(messageId: string, content: string, createdAt: string) {
  if (!content) return [];
  return [{ id: `${messageId}-part-0`, type: "text" as const, content, createdAt }];
}

function normalizeUnknownToString(value: unknown): string {
  if (typeof value === "string") return value;
  if (value == null) return "";
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

const CANCEL_CLOSURE_EVENT_TYPES = new Set<string>([
  SSEEventType.RUN_STARTED,
  SSEEventType.TOOL_CALL_START,
  SSEEventType.TOOL_CALL_RESULT,
  SSEEventType.TOOL_CALL_END,
  SSEEventType.APPROVAL_REQUIRED,
  SSEEventType.SIDE_EFFECT_UNKNOWN,
  SSEEventType.CANCELLED,
  SSEEventType.RUN_ERROR,
  SSEEventType.RUN_FINISHED,
]);

function parseToolArguments(value: string): Record<string, unknown> {
  if (!value) return {};
  try {
    const parsed = JSON.parse(value);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return parsed as Record<string, unknown>;
    }
  } catch {
    // Fall through to raw payload wrapper
  }
  return { raw: value };
}

function eventToolId(data: Record<string, unknown> | undefined): string {
  if (!data) return "";
  for (const key of ["tool_id", "tool_call_id", "call_id", "id"]) {
    const value = data[key];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return "";
}

function summarizeToolArguments(value: unknown): string | undefined {
  const args =
    typeof value === "string"
      ? parseToolArguments(value)
      : value && typeof value === "object" && !Array.isArray(value)
        ? (value as Record<string, unknown>)
        : undefined;
  if (!args) return undefined;
  for (const key of ["prompt", "title", "goal", "url", "path", "query", "command"]) {
    const item = args[key];
    if (typeof item === "string" && item.trim()) {
      const text = item.trim();
      return text.length > 160 ? `${text.slice(0, 157)}...` : text;
    }
  }
  return undefined;
}

function mapStreamToolCallsToAssistant(turnState: StreamTurnState) {
  return turnState.toolCalls.map((toolCall) => ({
    id: toolCall.id,
    name: toolCall.name,
    arguments: parseToolArguments(toolCall.arguments),
    status: toolCall.status,
  }));
}

function mergeUsageWithTurnState(
  usage: Record<string, unknown>,
  turnState: StreamTurnState
): Record<string, unknown> {
  return {
    ...usage,
    ...(turnState.usage.inputTokens != null
      ? { input_tokens: turnState.usage.inputTokens }
      : {}),
    ...(turnState.usage.outputTokens != null
      ? { output_tokens: turnState.usage.outputTokens }
      : {}),
    ...(turnState.usage.totalTokens != null
      ? { total_tokens: turnState.usage.totalTokens }
      : {}),
  };
}

const ASSISTANT_ACTIVE_RUN_METADATA_KEY = ACTIVE_RUN_METADATA_KEY;

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

function nonEmptyString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

async function restoreLatestRun(
  messages: ChatMessageType[],
  metadata: Record<string, unknown> | null | undefined,
  sessionId: string,
): Promise<{
  messages: ChatMessageType[];
  error?: string;
  blocksComposer?: boolean;
  activeRun?: { sessionId: string; runId: string };
}> {
  const marker = asRecord(metadata?.[ASSISTANT_ACTIVE_RUN_METADATA_KEY]);
  const runId = nonEmptyString(marker?.run_id);
  if (!marker || !runId) return { messages };
  const roles = messages.map((message) => message.role);
  const latestAssistantIndex = roles.lastIndexOf("assistant");
  const latestUserIndex = roles.lastIndexOf("user");
  const markerUpdatedAt = Date.parse(nonEmptyString(marker.updated_at) || "");
  const userUpdatedAt = Date.parse(messages[latestUserIndex]?.createdAt || "");
  if (
    Number.isFinite(markerUpdatedAt) &&
    Number.isFinite(userUpdatedAt) &&
    markerUpdatedAt < userUpdatedAt
  ) {
    return { messages };
  }

  const persistedSummary = messages[latestAssistantIndex]?.processSummary;
  const processSummary: ProcessSummaryState = persistedSummary?.runId === runId ? { ...persistedSummary } : {
    collapsed: true,
    runId,
    status: "running",
    startedAt: Number.isFinite(markerUpdatedAt) ? markerUpdatedAt : undefined,
    steps: [],
    tools: [],
  };
  const next = [...messages];
  let targetIndex = latestAssistantIndex;
  if (latestAssistantIndex > latestUserIndex) {
    next[latestAssistantIndex] = {
      ...next[latestAssistantIndex],
      processSummary,
    };
  } else {
    targetIndex = next.length;
    next.push({
      id: `${sessionId}-run-${runId}`,
      role: "assistant",
      content: "",
      createdAt: nonEmptyString(marker.updated_at) || new Date().toISOString(),
      parts: [],
      status: "streaming",
      isStreaming: false,
      processSummary,
    });
  }
  try {
    const { run } = await getAssistantRunStatus(runId);
    if (
      nonEmptyString(run.run_id) !== runId ||
      (nonEmptyString(run.session_id) && run.session_id !== sessionId)
    ) {
      throw new Error("run_scope_mismatch");
    }
    const checkpoint = asRecord(run.checkpoint);
    let status = nonEmptyString(run.status) || "unknown";
    const phase = nonEmptyString(checkpoint?.phase);
    const runtimeThreadId = nonEmptyString(run.harness_thread_id);
    let durableApproval: {
      approvalId: string;
      toolId: string;
      toolName: string;
      reason?: string;
      runId?: string;
      threadId?: string;
    } | undefined;
    if (
      runtimeThreadId &&
      ["running", "queued", "awaiting_approval"].includes(status)
    ) {
      try {
        const { getAgentRuntimeV2RunSnapshot } = await import("@/api/agentThreads");
        const snapshot = await getAgentRuntimeV2RunSnapshot(runtimeThreadId, runId);
        durableApproval = snapshot.pendingApproval;
        status = snapshot.terminalStatus ?? status;
      } catch {
        console.warn("Assistant durable run replay failed");
      }
    }
    const approvalId = nonEmptyString(checkpoint?.approval_id) ?? durableApproval?.approvalId;
    const current = next[targetIndex];
    const base = current.processSummary!;
    const terminal = ["succeeded", "completed", "failed", "cancelled"].includes(status);
    if (!terminal && (phase === "approval_pending" || durableApproval) && approvalId) {
      const pendingTool = asRecord(checkpoint?.pending_tool);
      next[targetIndex] = {
        ...current,
        isStreaming: false,
        status: "streaming",
        processSummary: {
          ...base,
          collapsed: false,
          status: "blocked",
          // The restored card must take the V2 decision branch, which keys off
          // ``runtimeThreadId``. The run-status response already carries the
          // authoritative ``harness_thread_id`` (== the runtime thread id the
          // live ``approval_required`` event reports as ``thread_id``), so
          // recover it here instead of falling through to the legacy
          // ``approveToolCall`` + resume path that cannot drive a V2 thread.
          runtimeThreadId: durableApproval?.threadId ?? runtimeThreadId ?? base.runtimeThreadId,
          tools: [{
            id: durableApproval?.toolId ?? nonEmptyString(pendingTool?.tool_id) ?? `approval-${approvalId}`,
            name: durableApproval?.toolName ?? nonEmptyString(pendingTool?.tool_name) ?? "Pending tool",
            status: "approval_required",
            approvalId,
            summary: durableApproval?.reason,
          }],
        },
      };
    } else {
      const processStatus = restoredProcessStatus(status);
      const active = processStatus === "running";
      const succeeded = processStatus === "succeeded";
      next[targetIndex] = {
        ...current,
        isStreaming: false,
        status: restoredMessageStatus(processStatus),
        outcomeUncertain:
          current.outcomeUncertain === true ||
          status === "unknown" ||
          phase === "side_effect_unknown",
        processSummary: {
          ...base,
          status: processStatus,
          runtimeThreadId: runtimeThreadId ?? base.runtimeThreadId,
          terminalReason: run.terminal_reason === "runtime_restart_interrupted"
            ? "runtime_restart_interrupted" : base.terminalReason,
          collapsed: succeeded || active ? base.collapsed : false,
          isErrorExpanded: processStatus === "failed" ? true : undefined,
          tools: [],
        },
      };
    }
    const blocksComposer =
      (!terminal && (phase === "approval_pending" || Boolean(durableApproval)) && Boolean(approvalId)) ||
      status === "running" ||
      status === "queued" ||
      status === "awaiting_approval";
    return {
      messages: next,
      blocksComposer,
      activeRun: blocksComposer ? { sessionId, runId } : undefined,
    };
  } catch {
    console.warn("Assistant run status reconciliation failed");
    return {
      messages: next,
      blocksComposer: true,
      activeRun: { sessionId, runId },
      error: "Run status is unavailable. This conversation is paused until its result can be checked.",
    };
  }
}

// Helper to restore message metadata
const restoreMessageMetadata = (msg: any, index: number, sessionId: string): ChatMessageType => {
  const createdAt = msg.timestamp || new Date().toISOString();
  const content = typeof msg.content === "string" ? msg.content : JSON.stringify(msg.content);
  const imageTurnId = nonEmptyString(msg.metadata?.image_turn_id);
  const messageId = imageTurnId ? `${sessionId}-image-${imageTurnId}` : `${sessionId}-${index}`;
  const baseMessage: ChatMessageType = {
    id: messageId,
    role: msg.role as "user" | "assistant",
    content,
    createdAt,
    parts: buildTextParts(messageId, content, createdAt),
    status: "completed",
  };

  // Restore attachments
  if (msg.role === "user" && msg.metadata?.attachments) {
    baseMessage.attachments = msg.metadata.attachments.map((att: any) => ({
      id: att.url,
      type: att.type,
      filename: att.filename,
      url: att.url,
    }));
  }

  // Restore assistant metadata
  if (msg.role === "assistant" && msg.metadata) {
    baseMessage.imageTurnId = imageTurnId;
    baseMessage.imageTaskId = nonEmptyString(msg.metadata.image_task_id);
    baseMessage.isGeneratingImage = msg.metadata.image_generating === true;
    baseMessage.imageGenerationPrompt = nonEmptyString(msg.metadata.image_generation_prompt);
    baseMessage.runtimeRunId = nonEmptyString(msg.metadata.runtime_run_id);
    baseMessage.sourceAccessRevoked = msg.metadata.source_access_revoked === true;
    // Initialize search status array
    const searchStatusItems: any[] = [];

    // Restore KB contexts
    if (msg.metadata.contexts && Array.isArray(msg.metadata.contexts)) {
      baseMessage.contexts = msg.metadata.contexts.map((ctx: any) => ({
        dataset_id: ctx.dataset_id,
        dataset_name: ctx.dataset_name,
        chunks: ctx.chunks || [],
        query: ctx.query,
        took_ms: ctx.took_ms,
      }));
      searchStatusItems.push(...msg.metadata.contexts.map((ctx: any) => ({
        type: "kb" as const,
        state: "completed" as const,
        resultCount: ctx.chunks?.length || 0,
        datasets: [ctx.dataset_name],
        durationMs: ctx.took_ms,
      })));
    }

    // Restore web search results
    if (msg.metadata.web_search_results && Array.isArray(msg.metadata.web_search_results.results)) {
      baseMessage.webSearchResults = msg.metadata.web_search_results.results;
      searchStatusItems.push({
        type: "web" as const,
        state: "completed" as const,
        resultCount: msg.metadata.web_search_results.results.length,
        durationMs: msg.metadata.web_search_results.response_time_ms,
      });
    }

    if (searchStatusItems.length > 0) {
      baseMessage.searchStatus = searchStatusItems;
    }

    if (msg.metadata.usage) {
      baseMessage.usage = {
        input_tokens: msg.metadata.usage.prompt_tokens,
        output_tokens: msg.metadata.usage.completion_tokens,
      };
    }

    // Mark quiz_id for async loading
    if (msg.metadata.quiz_id) {
      (baseMessage as any)._quizId = msg.metadata.quiz_id;
    }

    // Mark artifact_ids for post-hydration
    if (msg.metadata.artifact_ids && Array.isArray(msg.metadata.artifact_ids)) {
      baseMessage._artifactIds = msg.metadata.artifact_ids;
    }

    // Restore Activity-drawer fields. These three were persisted starting
    // with the 2026-04-21 fix — without them, the drawer shows
    // "No activity recorded · 0 steps" on session reload even though the
    // original turn ran native-search / emitted thinking. The frontend's
    // buildTimeline reads thinkingContent + toolCalls + toolResults.
    if (typeof msg.metadata.thinking_content === "string" && msg.metadata.thinking_content.trim()) {
      baseMessage.thinkingContent = msg.metadata.thinking_content;
    }
    if (Array.isArray(msg.metadata.tool_calls) && msg.metadata.tool_calls.length > 0) {
      baseMessage.toolCalls = msg.metadata.tool_calls.map((tc: any) => ({
        id: String(tc?.id ?? ""),
        name: String(tc?.name ?? ""),
        arguments:
          tc?.arguments && typeof tc.arguments === "object" && !Array.isArray(tc.arguments)
            ? (tc.arguments as Record<string, unknown>)
            : {},
        status:
          tc?.status === "pending" || tc?.status === "running"
            ? tc.status
            : tc?.status === "error" ||
                tc?.status === "cancelled" ||
                tc?.status === "not_executed"
              ? "error"
              : "completed",
      }));
    }
    if (Array.isArray(msg.metadata.tool_results) && msg.metadata.tool_results.length > 0) {
      baseMessage.toolResults = msg.metadata.tool_results.map((tr: any) => ({
        tool_call_id: String(tr?.tool_call_id ?? ""),
        name: typeof tr?.name === "string" ? tr.name : "",
        result: tr?.result,
        error: typeof tr?.error === "string" ? tr.error : undefined,
        duration_ms: typeof tr?.duration_ms === "number" ? tr.duration_ms : undefined,
      }));
    }

    // Runtime history may include the normalized child-agent lifecycle. Feed
    // it through the same monotonic reducer used by live SSE so refreshes do
    // not resurrect a terminal child or invent a second identity.
    let persistedSubagents: unknown[] = [];
    if (Array.isArray(msg.metadata.active_sub_agents)) {
      persistedSubagents = msg.metadata.active_sub_agents;
    } else if (Array.isArray(msg.metadata.subagents)) {
      persistedSubagents = msg.metadata.subagents;
    }
    if (persistedSubagents.length > 0) {
      let restored: ChatMessageType["activeSubAgents"] = [];
      for (const raw of persistedSubagents) {
        if (!raw || typeof raw !== "object") continue;
        const agent = raw as Record<string, unknown>;
        const agentId = typeof agent.agent_id === "string" ? agent.agent_id : "";
        if (!agentId) continue;
        restored = reduceSubAgentEvent(restored, "subagent_started", agent, Date.now());
        if (typeof agent.current_step === "string") {
          restored = reduceSubAgentEvent(restored, "subagent_step", {
            ...agent,
            step: agent.current_step,
            status: agent.current_step_status ?? "running",
          }, Date.now());
        }
        if (typeof agent.status === "string" && agent.status !== "running") {
          restored = reduceSubAgentEvent(restored, "subagent_finished", agent, Date.now());
        }
      }
      if (restored.length > 0) baseMessage.activeSubAgents = restored;
    }

    // Some Runtime history projections expose the append-only event list
    // instead of a pre-folded child array. Reuse the live reducer for those
    // records as well; unknown event types remain harmless no-ops.
    const persistedRuntimeEvents = Array.isArray(msg.metadata.runtime_events)
      ? msg.metadata.runtime_events
      : [];
    if (persistedRuntimeEvents.length > 0) {
      let restored = baseMessage.activeSubAgents ?? [];
      for (const raw of persistedRuntimeEvents) {
        if (!raw || typeof raw !== "object") continue;
        const record = raw as Record<string, unknown>;
        const eventType = typeof record.event_type === "string" ? record.event_type : "";
        if (!eventType) continue;
        restored = reduceSubAgentEvent(
          restored,
          eventType,
          record.data,
          typeof record.timestamp === "number" ? record.timestamp * 1000 : Date.now(),
        );
      }
      if (restored.length > 0) baseMessage.activeSubAgents = restored;
    }

    const persistedSummary = msg.metadata.process_summary;
    if (persistedSummary && typeof persistedSummary === "object") {
      const summary = persistedSummary as Record<string, unknown>;
      let status: ProcessSummaryState["status"] = "succeeded";
      if (
        summary.status === "failed" ||
        summary.status === "cancelled" ||
        summary.status === "blocked"
      ) {
        status = summary.status;
      } else if (summary.status === "running") {
        status = "running";
      }
      baseMessage.processSummary = {
        collapsed: summary.collapsed === true,
        runId: typeof summary.run_id === "string" ? summary.run_id : undefined,
        status,
        terminalReason: summary.terminal_reason === "runtime_restart_interrupted"
          ? "runtime_restart_interrupted" : undefined,
        steps: Array.isArray(summary.steps) ? summary.steps as ProcessStepItem[] : [],
        tools: Array.isArray(summary.tools) ? summary.tools as ToolTimelineItem[] : [],
        reasoning: summary.reasoning && typeof summary.reasoning === "object"
          ? summary.reasoning as ProcessSummaryState["reasoning"] : undefined,
        modelId: typeof summary.model_id === "string" ? summary.model_id : undefined,
        temperature: typeof summary.temperature === "number" ? summary.temperature : undefined,
      };
      baseMessage.status = restoredMessageStatus(status);
      baseMessage.diagnosticId = typeof summary.diagnostic_id === "string"
        ? summary.diagnostic_id : undefined;
      baseMessage.outcomeUncertain =
        summary.outcome_uncertain === true || summary.status === "unknown";
    }

  }
  return baseMessage.sourceAccessRevoked ? redactSourceMessage(baseMessage) : baseMessage;
};

function toArtifact(artifact: ArtifactInfo): Artifact {
  return {
    id: artifact.artifact_id,
    type: artifact.type as Artifact["type"],
    format: artifact.format,
    title: artifact.title,
    url: artifact.download_url || getArtifactDownloadUrl(artifact.artifact_id),
    createdAt: new Date(artifact.created_at),
    filename: artifact.filename,
    mimeType: artifact.mime_type,
    sizeBytes: artifact.size_bytes,
    ready: artifact.ready ?? artifact.size_bytes > 0,
    messageId: artifact.message_id,
    source: artifact.source as Artifact["source"],
  };
}

/** Async: load quiz data for messages that have _quizId, then update state */
async function hydrateQuizData(
  messages: ChatMessageType[],
  setMessages: React.Dispatch<React.SetStateAction<ChatMessageType[]>>,
) {
  const quizMessages = messages.filter((m) => !m.sourceAccessRevoked && (m as any)._quizId);
  if (quizMessages.length === 0) return;

  const results = await Promise.allSettled(
    quizMessages.map(async (m) => {
      const quizId = (m as any)._quizId as string;
      const quiz = await getQuiz(quizId);
      return { messageId: m.id, quiz };
    }),
  );

  const quizMap = new Map<string, any>();
  for (const r of results) {
    if (r.status === "fulfilled") {
      quizMap.set(r.value.messageId, r.value.quiz);
    }
  }

  if (quizMap.size > 0) {
    setMessages((prev) =>
      prev.map((m) => {
        const quiz = quizMap.get(m.id);
        if (quiz && !m.sourceAccessRevoked) {
          const updated = { ...m, quizData: quiz };
          delete (updated as any)._quizId;
          return updated;
        }
        return m;
      }),
    );
  }
}

function initProcessSummary(runId?: string, startedAt?: number): ProcessSummaryState {
  return {
    collapsed: true,
    runId,
    status: "running",
    startedAt,
    steps: [],
    tools: [],
  };
}

function upsertStep(
  steps: ProcessStepItem[],
  incoming: ProcessStepItem
): ProcessStepItem[] {
  const idx = steps.findIndex((s) => s.id === incoming.id);
  if (idx === -1) return [...steps, incoming];
  const next = [...steps];
  next[idx] = { ...next[idx], ...incoming };
  return next;
}

function upsertTool(
  tools: ToolTimelineItem[],
  incoming: ToolTimelineItem
): ToolTimelineItem[] {
  const idx = tools.findIndex((s) => s.id === incoming.id);
  if (idx === -1) return [...tools, incoming];
  const next = [...tools];
  next[idx] = { ...next[idx], ...incoming };
  return next;
}

function summarizeToolResult(result: unknown): string | undefined {
  if (result == null) return undefined;
  if (typeof result === "string") {
    return result.length > 120 ? `${result.slice(0, 120)}...` : result;
  }
  if (Array.isArray(result)) {
    return `items: ${result.length}`;
  }
  if (typeof result === "object") {
    const rec = result as Record<string, unknown>;
    if (typeof rec.total_results === "number") return `results: ${rec.total_results}`;
    if (typeof rec.count === "number") return `count: ${rec.count}`;
    if (Array.isArray(rec.files)) return `files: ${rec.files.length}`;
    if (Array.isArray(rec.citations)) return `citations: ${rec.citations.length}`;
    try {
      const text = JSON.stringify(rec);
      return text.length > 120 ? `${text.slice(0, 120)}...` : text;
    } catch {
      return undefined;
    }
  }
  return String(result);
}

function isZeroToolNoise(value: string | undefined): boolean {
  if (!value) return false;
  const normalized = value.trim().toLowerCase();
  if (!normalized) return false;

  const zhPattern = /(正在|执行).{0,24}0\s*个?\s*工具/u;
  const enPattern = /(running|executing).{0,24}\b0\s*tools?\b/u;
  return zhPattern.test(normalized) || enPattern.test(normalized);
}

function sanitizeProgressLabel(value: string | undefined): string | undefined {
  if (!value) return undefined;
  const trimmed = value.trim();
  if (!trimmed || isZeroToolNoise(trimmed)) return undefined;
  return trimmed;
}

function taskStatusToProcessStatus(
  status: WorkingMemoryUpdateEventData["tasks"][number]["status"] | "pending",
): ProcessStepStatus {
  if (status === "completed") return "completed";
  if (status === "failed" || status === "blocked") return "failed";
  if (status === "in_progress") return "running";
  return "pending";
}

function finalizeProcessSummary(
  summary: ProcessSummaryState | undefined,
  status: "succeeded" | "failed" | "cancelled",
  finishedAtMs: number,
  keepFailed = false
): ProcessSummaryState | undefined {
  if (!summary) {
    return summary;
  }

  const finalStatus =
    keepFailed && summary.status === "failed" ? "failed" : status;
  const totalDurationMs =
    summary.totalDurationMs ??
    (summary.startedAt ? finishedAtMs - summary.startedAt : undefined);

  if (finalStatus === "succeeded" && summary.tools.some((tool) => tool.status === "approval_required")) {
    return {
      ...summary,
      status: "blocked",
      collapsed: false,
      totalDurationMs,
    };
  }

  if (finalStatus === "failed") {
    return {
      ...summary,
      status: "failed",
      collapsed: false,
      isErrorExpanded: true,
      totalDurationMs,
    };
  }

  if (finalStatus === "cancelled") {
    return {
      ...summary,
      status: "cancelled",
      totalDurationMs,
    };
  }

  return {
    ...summary,
    status: "succeeded",
    totalDurationMs,
  };
}

export interface UseChatSessionOptions {
  /** Fail-closed, current Local Node eligibility gate for initial and resumed streams. */
  isOSAgentEligible?: () => boolean;
  getLocalNodeBinding?: () => { deviceId: string; grantIds: string[] } | undefined;
}

export function useChatSession(options: UseChatSessionOptions = {}) {
  const { t } = useTranslation();
  const osAgentEligibilityRef = useRef(options.isOSAgentEligible);
  osAgentEligibilityRef.current = options.isOSAgentEligible;
  const localNodeBindingRef = useRef(options.getLocalNodeBinding);
  localNodeBindingRef.current = options.getLocalNodeBinding;

  // 使用全局状态存储的 AI助手 专用会话 ID（与 Playground 完全分离）
  const {
    assistantActiveSessionId: activeSessionId,
    setAssistantActiveSessionId: setActiveSessionId,
    setAssistantLocalTitles,
  } = useAppStore();
  
  // State
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [modelRecreateNeeded, setModelRecreateNeeded] = useState(false);
  const [sourceRecreateNeeded, setSourceRecreateNeeded] = useState(false);
  const [messages, setMessages] = useState<ChatMessageType[]>([]);
  const messagesRef = useRef<ChatMessageType[]>([]);
  const activeSessionIdRef = useRef<string | null | undefined>(null);
  const [isStreaming, setIsStreaming] = useState(false);
  const [sessionsLoading, setSessionsLoading] = useState(true);
  const [historyRestoreState, setHistoryRestoreState] = useState<
    "idle" | "loading" | "ready" | "failed"
  >("idle");
  const [historyRestoreError, setHistoryRestoreError] = useState<string | null>(null);
  const [serverRunBlocking, setServerRunBlocking] = useState(false);
  
  // Artifacts & Agent State (Managed here as they are tied to session)
  const [artifacts, setArtifactsState] = useState<Artifact[]>([]);
  const artifactVersionRef = useRef(0);
  const setArtifacts = useCallback((next: SetStateAction<Artifact[]>) => {
    artifactVersionRef.current += 1; setArtifactsState(next);
  }, []);
  const artifactRestoreOwner = useRef<number | null>(null);
  const [artifactRestorePending, setArtifactRestorePending] = useState(false);
  const [artifactLoadError, setArtifactLoadError] = useState(false);
  const [artifactDrawer, setArtifactDrawer] = useState({ open: false, epoch: 0 });
  const artifactDrawerRef = useRef(artifactDrawer);
  const showArtifacts = artifactDrawer.open;
  const setShowArtifacts = useCallback((open: boolean) => {
    if (artifactDrawerRef.current.open === open) return;
    const next = { open, epoch: artifactDrawerRef.current.epoch + 1 };
    artifactDrawerRef.current = next; setArtifactDrawer(next);
  }, []);
  const isArtifactViewCurrent = useCallback((epoch: number) => artifactDrawerRef.current.open && artifactDrawerRef.current.epoch === epoch, []);
  const [workingMemory, setWorkingMemory] = useState<WorkingMemory | null>(null);
  const [showTaskPanel, setShowTaskPanel] = useState(false);
  const [codeExecution, setCodeExecution] = useState<CodeExecutionState>({
    isExecuting: false,
    executionId: null,
    code: null,
    output: "",
    executionTimeMs: null,
    status: "idle",
    outputFiles: [],
  });

  const abortControllerRef = useRef<AbortController | null>(null);
  const cancelRequestedRef = useRef(false);
  const activeTaskIdRef = useRef<string | null>(null);
  const cancelApiTaskIdRef = useRef<string | null>(null);
  const cancelFallbackTimerRef = useRef<ReturnType<typeof window.setTimeout> | null>(null);
  const sendInFlightRef = useRef(false);
  // Monotonic ownership token for every stream attempt. Session switches can
  // invalidate an in-flight createSession/fetch even before an AbortController
  // exists, and stale finally blocks must never clear a newer stream's refs.
  const streamEpochRef = useRef(0);
  const restoreEpochRef = useRef(0);
  const getRestoredRunVersion = useCallback(() => `${restoreEpochRef.current}:${streamEpochRef.current}`, []);
  const { observedRun, setObservedRun, setRestoredStopPending, stopRestoredRun } =
    useRestoredRunStop(getRestoredRunVersion, setHistoryRestoreError);
  const retireArtifactRestore = useCallback(() => {
    restoreEpochRef.current += 1;
    artifactRestoreOwner.current = null;
    setArtifactRestorePending(false);
  }, []);
  useLayoutEffect(() => useAuthStore.subscribe((state, prior) => {
    if (state.token !== prior.token || state.isAuthenticated !== prior.isAuthenticated || sdkAuthorityScope(state.user) !== sdkAuthorityScope(prior.user)) {
      const wasRestoring = artifactRestoreOwner.current !== null;
      retireArtifactRestore();
      if (wasRestoring) {
        setHistoryRestoreState("failed");
        setHistoryRestoreError(t("assistant.artifactRecovery.restoreInterrupted"));
      }
    }
  }), [retireArtifactRestore, t]);
  // Assistant-message ids whose SSE consumer exited while an approval hold was
  // still active (server EOF / proxy timeout / network drop mid-park). For
  // these the stream is dead, so a decision made later can never arrive as an
  // in-band APPROVAL_RESULT; ``handleToolApproval`` reconciles them from
  // authoritative history instead of relying on the abandoned stream. A live
  // stream clears the hold before its ``finally`` runs, so it is never added.
  const deadApprovalMessageIdsRef = useRef<Set<string>>(new Set());
  const pendingSessionIdRef = useRef<string | undefined>(undefined);
  const lastStreamConfigRef = useRef<{
    config: SessionConfig;
    selectedDatasets: string[];
    models: any[];
    datasets: any[];
  } | null>(null);

  const getArtifactVersion = useCallback(() => [artifactVersionRef.current, restoreEpochRef.current, streamEpochRef.current, artifactRestoreOwner.current].join(":"), []);
  const applyArtifactSnapshot = useCallback((items: ArtifactInfo[]) => setArtifacts(items.map(toArtifact)), [setArtifacts]);
  const { refreshArtifacts, artifactRefreshStatus, canRefreshArtifacts } = useArtifactListRecovery({
    sessionId: activeSessionId, open: showArtifacts, viewEpoch: artifactDrawer.epoch,
    isViewCurrent: isArtifactViewCurrent, eligible: historyRestoreState !== "loading" && !artifactRestorePending && !isStreaming && !observedRun && !codeExecution.isExecuting,
    getVersion: getArtifactVersion, apply: applyArtifactSnapshot, setLoadError: setArtifactLoadError,
  });

  // 用于跟踪是否已经初始化完成
  const isInitialized = useRef(false);

  const clearCancelFallback = useCallback(() => {
    if (cancelFallbackTimerRef.current !== null) {
      window.clearTimeout(cancelFallbackTimerRef.current);
      cancelFallbackTimerRef.current = null;
    }
  }, []);

  const scheduleCancelFallback = useCallback((controller: AbortController) => {
    clearCancelFallback();
    cancelFallbackTimerRef.current = window.setTimeout(() => {
      cancelFallbackTimerRef.current = null;
      if (abortControllerRef.current === controller && !controller.signal.aborted) {
        controller.abort();
      }
    }, 2500);
  }, [clearCancelFallback]);

  const requestTaskCancellation = useCallback(
    (taskId: string, controller: AbortController) => {
      if (!taskId || cancelApiTaskIdRef.current === taskId) return;
      cancelApiTaskIdRef.current = taskId;
      void cancelTask(taskId, "user_requested_stop")
        .then((result) => {
          if (
            !result.cancelled &&
            abortControllerRef.current === controller &&
            !controller.signal.aborted
          ) {
            controller.abort();
          }
        })
        .catch((error) => {
          console.warn("Assistant task cancellation request failed", error);
          if (
            abortControllerRef.current === controller &&
            !controller.signal.aborted
          ) {
            controller.abort();
          }
        });
    },
    [],
  );

  // Cleanup AbortController on unmount to prevent state updates on unmounted component
  useEffect(() => {
    messagesRef.current = messages;
  }, [messages]);

  const sourceWatchThreadId = [...messages].reverse().find((m) => m.processSummary?.runtimeThreadId)?.processSummary?.runtimeThreadId;
  const hasKnowledgeHistory = messages.some((m) => Boolean(m.contexts?.length || m.sourceAccessRevoked));
  const isRecoveringImage = messages.some((m) => m.imageTaskId && m.isGeneratingImage);
  useEffect(() => {
    if (!activeSessionId || !isRecoveringImage) return;
    const sessionId = activeSessionId;
    const restoreEpoch = restoreEpochRef.current;
    let disposed = false;
    let checking = false;
    const check = async () => {
      if (checking) return;
      checking = true;
      try {
        const [history, savedArtifacts] = await Promise.all([
          getAssistantSessionHistory(sessionId, 200), getSessionArtifacts(sessionId),
        ]);
        if (disposed || restoreEpoch !== restoreEpochRef.current || activeSessionIdRef.current !== sessionId) return;
        const restored = hydrateMessageArtifacts(
          history.messages.map((m, index) => restoreMessageMetadata(m, index, sessionId)), savedArtifacts,
        );
        // This observer reads the existing image ledger. It never creates a
        // Rust run or asks the image provider to repeat an earlier request.
        setMessages((previous) => previous.map((message) => message.imageTaskId
          ? restored.find((m) => m.imageTaskId === message.imageTaskId) ?? message : message));
        setArtifacts(savedArtifacts.map(toArtifact));
      } catch {
        // Keep the last known status on read failure; no automatic retry of execution.
      } finally {
        checking = false;
      }
    };
    void check();
    const timer = window.setInterval(() => void check(), 2000);
    return () => { disposed = true; window.clearInterval(timer); };
  }, [activeSessionId, isRecoveringImage, setArtifacts]);
  useEffect(() => {
    if (!activeSessionId || !sourceWatchThreadId || !hasKnowledgeHistory) return;
    let disposed = false;
    let checking = false;
    const check = async () => {
      if (checking) return;
      checking = true;
      try {
        const thread = await getAgentRuntimeThread(sourceWatchThreadId);
        const restricted = new Set(thread.restricted_source_run_ids || []);
        if (disposed || !restricted.size) return;
        setSourceRecreateNeeded(true);
        setMessages((previous) => redactRestrictedSourceMessages(previous, restricted));
        const allowed = await getSessionArtifacts(activeSessionId).catch(() => []);
        if (!disposed) setArtifacts(allowed.map(toArtifact));
      } catch {
        // A failed read never changes execution or grants access.
      } finally {
        checking = false;
      }
    };
    void check();
    const timer = window.setInterval(() => void check(), 3000);
    return () => { disposed = true; window.clearInterval(timer); };
  }, [activeSessionId, sourceWatchThreadId, hasKnowledgeHistory, setArtifacts]);

  useEffect(() => {
    activeSessionIdRef.current = activeSessionId;
  }, [activeSessionId]);

  useEffect(() => {
    return () => {
      streamEpochRef.current += 1;
      clearCancelFallback();
      abortControllerRef.current?.abort(ASSISTANT_VIEW_DETACH_REASON);
    };
  }, [clearCancelFallback]);

  // Load sessions on mount and restore active session if exists
  useEffect(() => {
    // 只在首次挂载时执行
    if (isInitialized.current) return;
    isInitialized.current = true;
    
    async function loadSessionsAndRestore() {
      const savedSessionId = useAppStore.getState().assistantActiveSessionId;
      const restoreEpoch = savedSessionId ? ++restoreEpochRef.current : 0;

      const listTask = listSessions({ service_id: "__builtin_assistant__", limit: 100 })
        .then((data) => {
          setSessions(data);
          setAssistantLocalTitles((prev: Record<string, string>) => {
            const updated = { ...prev };
            for (const session of data) {
              const serverTitle = session.metadata?.title as string | undefined;
              if (serverTitle && !updated[session.session_id]) {
                updated[session.session_id] = serverTitle;
              }
            }
            return updated;
          });
        })
        .catch((error) => {
          console.error("Failed to load sessions:", error);
        })
        .finally(() => setSessionsLoading(false));

      const restoreTask = savedSessionId
        ? (async () => {
            artifactRestoreOwner.current = restoreEpoch; setArtifactRestorePending(true);
            setHistoryRestoreState("loading");
            setHistoryRestoreError(null);
            const detailsPromise = getSession(savedSessionId);
            const historyPromise = getAssistantSessionHistory(savedSessionId, 200)
              .then((response) => response.messages);
            const artifactsPromise = getSessionArtifacts(savedSessionId).then((items) => {
              if (restoreEpoch === restoreEpochRef.current) setArtifactLoadError(false);
              return items;
            }).catch(() => {
              if (restoreEpoch === restoreEpochRef.current) setArtifactLoadError(true);
              return [];
            });
            try {
              const [sessionDetails, history] = await Promise.all([
                detailsPromise,
                historyPromise,
              ]);
              if (restoreEpoch !== restoreEpochRef.current) return;

              let chatMessages = history.map((message, index) =>
                restoreMessageMetadata(message, index, savedSessionId)
              );
              setMessages(chatMessages);
              hydrateQuizData(chatMessages, setMessages);
              setHistoryRestoreState("ready");
              const restoredConfig = sessionDetails.config || {};
              lastStreamConfigRef.current = {
                config: restoredConfig,
                selectedDatasets: restoredConfig.selected_datasets || [],
                models: [],
                datasets: [],
              };
              setServerRunBlocking(shouldBlockDuringRunRestore(sessionDetails.metadata));
              const reconciliationPromise = restoreLatestRun(
                chatMessages,
                sessionDetails.metadata,
                savedSessionId,
              );

              const [sessionArtifacts, reconciliation] = await Promise.all([
                artifactsPromise,
                reconciliationPromise,
              ]);
              if (restoreEpoch !== restoreEpochRef.current) return;
              const loadedArtifacts = sessionArtifacts.map(toArtifact);
              setArtifacts(loadedArtifacts);
              setShowArtifacts(false);
              chatMessages = hydrateMessageArtifacts(
                reconciliation.messages,
                sessionArtifacts,
              );
              setServerRunBlocking(Boolean(reconciliation.blocksComposer));
              setObservedRun(reconciliation.activeRun ?? null);
              setMessages(chatMessages);
              setCodeExecution({
                isExecuting: false,
                executionId: null,
                code: null,
                output: "",
                executionTimeMs: null,
                status: "idle",
                outputFiles: buildLatestRunOutputFilesFromArtifacts(
                  chatMessages,
                  sessionArtifacts,
                ),
              });
              setHistoryRestoreError(reconciliation.error || null);
              trackChatHistoryRestored("assistant", {
                sessionId: savedSessionId,
                messageCount: chatMessages.length,
                restored: true,
              });
            } catch (error) {
              if (restoreEpoch !== restoreEpochRef.current) return;
              const status = (error as { response?: { status?: number } })?.response?.status;
              if (status === 403 || status === 404) {
                setActiveSessionId(undefined);
                setHistoryRestoreState("idle");
                setHistoryRestoreError(null);
                return;
              }
              console.error("Failed to restore active session:", error);
              const reason = error instanceof Error ? error.message : "restore_failed";
              setHistoryRestoreState("failed");
              setHistoryRestoreError(reason);
              trackChatHistoryRestored("assistant", {
                sessionId: savedSessionId,
                messageCount: 0,
                restored: false,
                reason,
              });
            } finally {
              if (artifactRestoreOwner.current === restoreEpoch) { artifactRestoreOwner.current = null; setArtifactRestorePending(false); }
            }
          })()
        : Promise.resolve();

      await Promise.allSettled([listTask, restoreTask]);
    }
    loadSessionsAndRestore();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Session Actions
  const abandonActiveStream = useCallback(() => {
    streamEpochRef.current += 1;
    const controller = abortControllerRef.current;
    clearCancelFallback();
    abortControllerRef.current = null;
    activeTaskIdRef.current = null;
    cancelApiTaskIdRef.current = null;
    sendInFlightRef.current = false;
    setIsStreaming(false);
    controller?.abort(ASSISTANT_VIEW_DETACH_REASON);
  }, [clearCancelFallback]);

  const handleNewChat = useCallback(() => {
    // Invalidate every in-flight history restore before clearing UI state.
    // Otherwise a late restore can repopulate the newly opened blank chat.
    retireArtifactRestore();
    abandonActiveStream();
    setMessages([]);
    pendingSessionIdRef.current = undefined;
    setServerRunBlocking(false);
    setObservedRun(null);
    setModelRecreateNeeded(false);
    setSourceRecreateNeeded(false);
    setRestoredStopPending(false);
    setActiveSessionId(undefined);  // 清除 AI助手 的活动会话
    setHistoryRestoreState("idle");
    setHistoryRestoreError(null);
    setArtifacts([]);
    setArtifactLoadError(false);
    setShowArtifacts(false);
    setWorkingMemory(null);
    setShowTaskPanel(false);
    lastStreamConfigRef.current = null;
    setCodeExecution({
      isExecuting: false,
      executionId: null,
      code: null,
      output: "",
      executionTimeMs: null,
      status: "idle",
      outputFiles: [],
    });
  }, [abandonActiveStream, retireArtifactRestore, setActiveSessionId, setArtifacts, setShowArtifacts, setObservedRun, setRestoredStopPending]);

  const handleDeleteSession = useCallback(async (sessionId: string) => {
    try {
      await deleteAssistantSession(sessionId);
      setSessions((prev) => prev.filter((s) => s.session_id !== sessionId));
      if (activeSessionId === sessionId) {
        handleNewChat();
      }
      return true;
    } catch (error) {
      console.error("Failed to delete session:", error);
      return false;
    }
  }, [activeSessionId, handleNewChat]);

  const handleSelectSession = useCallback(async (sessionId: string) => {
    if (
      sessionId === activeSessionId &&
      messages.length > 0 &&
      historyRestoreState !== "failed"
    ) {
      return;
    }

    abandonActiveStream();
    setObservedRun(null);
    setModelRecreateNeeded(false);
    setSourceRecreateNeeded(false);
    setRestoredStopPending(false);
    const restoreEpoch = ++restoreEpochRef.current;
    try {
      artifactRestoreOwner.current = restoreEpoch; setArtifactRestorePending(true);
      setHistoryRestoreState("loading");
      setHistoryRestoreError(null);
      setMessages([]);
      const detailsPromise = getSession(sessionId);
      const historyPromise = getAssistantSessionHistory(sessionId, 200)
        .then((response) => response.messages);
      const artifactsPromise = getSessionArtifacts(sessionId).then((items) => {
        if (restoreEpoch === restoreEpochRef.current) setArtifactLoadError(false);
        return items;
      }).catch(() => {
        if (restoreEpoch === restoreEpochRef.current) setArtifactLoadError(true);
        return [];
      });
      const [sessionDetails, history] = await Promise.all([
        detailsPromise,
        historyPromise,
      ]);
      if (restoreEpoch !== restoreEpochRef.current) return;

      let chatMessages = history.map((message, index) =>
        restoreMessageMetadata(message, index, sessionId)
      );
      setMessages(chatMessages);
      hydrateQuizData(chatMessages, setMessages);
      pendingSessionIdRef.current = undefined;
      setActiveSessionId(sessionId);
      setHistoryRestoreState("ready");
      const restoredConfig = sessionDetails.config || {};
      lastStreamConfigRef.current = {
        config: restoredConfig,
        selectedDatasets: restoredConfig.selected_datasets || [],
        models: [],
        datasets: [],
      };
      setServerRunBlocking(shouldBlockDuringRunRestore(sessionDetails.metadata));
      const reconciliationPromise = restoreLatestRun(
        chatMessages,
        sessionDetails.metadata,
        sessionId,
      );

      const [sessionArtifacts, reconciliation] = await Promise.all([
        artifactsPromise,
        reconciliationPromise,
      ]);
      if (restoreEpoch !== restoreEpochRef.current) return;

      const loadedArtifacts = sessionArtifacts.map(toArtifact);
      setArtifacts(loadedArtifacts);
      setShowArtifacts(false);
      chatMessages = hydrateMessageArtifacts(
        reconciliation.messages,
        sessionArtifacts,
      );
      setServerRunBlocking(Boolean(reconciliation.blocksComposer));
      setObservedRun(reconciliation.activeRun ?? null);
      setMessages(chatMessages);
      setCodeExecution({
        isExecuting: false,
        executionId: null,
        code: null,
        output: "",
        executionTimeMs: null,
        status: "idle",
        outputFiles: buildLatestRunOutputFilesFromArtifacts(
          chatMessages,
          sessionArtifacts,
        ),
      });
      setHistoryRestoreError(reconciliation.error || null);
      trackChatHistoryRestored("assistant", {
        sessionId,
        messageCount: chatMessages.length,
        restored: true,
      });

      // Reset agent state
      setWorkingMemory(null);
      setShowTaskPanel(false);

      return sessionDetails.config;
    } catch (error) {
      if (restoreEpoch !== restoreEpochRef.current) return;
      console.error("Failed to load session:", error);
      const reason =
        error instanceof Error ? error.message : "load_session_failed";
      setHistoryRestoreState("failed");
      setHistoryRestoreError(reason);
      trackChatHistoryRestored("assistant", {
        sessionId,
        messageCount: 0,
        restored: false,
        reason,
      });
    } finally {
      if (artifactRestoreOwner.current === restoreEpoch) { artifactRestoreOwner.current = null; setArtifactRestorePending(false); }
    }
  }, [
    abandonActiveStream,
    activeSessionId,
    historyRestoreState,
    messages.length,
    setActiveSessionId,
    setArtifacts,
    setShowArtifacts,
    setObservedRun,
    setRestoredStopPending,
  ]);

  // A restored run is already admitted by the Runtime. Observe its persisted
  // history and status; never send the prompt or create a second turn here.
  useEffect(() => {
    if (!observedRun) return;
    const { sessionId, runId } = observedRun;
    const restoreEpoch = restoreEpochRef.current;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const poll = async () => {
      try {
        const [details, history, artifacts] = await Promise.all([
          getSession(sessionId),
          getAssistantSessionHistory(sessionId, 200),
          getSessionArtifacts(sessionId).catch(() => []),
        ]);
        if (disposed || restoreEpoch !== restoreEpochRef.current || activeSessionIdRef.current !== sessionId) return;
        let restored = history.messages.map((message, index) =>
          restoreMessageMetadata(message, index, sessionId),
        );
        let reconciliation = await restoreLatestRun(restored, details.metadata, sessionId);
        let settledArtifacts = artifacts;
        if (!reconciliation.blocksComposer) {
          // Status may commit after the first history/artifact reads. Read both
          // again after observing the terminal fact before ending observation.
          const [terminalHistory, terminalArtifacts] = await Promise.all([
            getAssistantSessionHistory(sessionId, 200),
            getSessionArtifacts(sessionId).catch(() => artifacts),
          ]);
          restored = terminalHistory.messages.map((message, index) =>
            restoreMessageMetadata(message, index, sessionId),
          );
          settledArtifacts = terminalArtifacts;
          reconciliation = await restoreLatestRun(restored, details.metadata, sessionId);
        }
        if (disposed || restoreEpoch !== restoreEpochRef.current || activeSessionIdRef.current !== sessionId) return;
        // Ignore a stale marker from an earlier run in the same conversation.
        if (reconciliation.activeRun && reconciliation.activeRun.runId !== runId) {
          setObservedRun(reconciliation.activeRun);
          return;
        }
        const loadedArtifacts = settledArtifacts.map(toArtifact);
        const next = hydrateMessageArtifacts(reconciliation.messages, settledArtifacts);
        setMessages(next);
        void hydrateQuizData(next, setMessages);
        setArtifacts(loadedArtifacts);
        setServerRunBlocking(Boolean(reconciliation.blocksComposer));
        setHistoryRestoreError(reconciliation.error || null);
        if (!reconciliation.blocksComposer) {
          setObservedRun(null);
          setRestoredStopPending(false);
          return;
        }
      } catch {
        if (!disposed) setHistoryRestoreError("Connection interrupted. Rechecking this run.");
      }
      if (!disposed) timer = window.setTimeout(() => void poll(), 2000);
    };
    timer = window.setTimeout(() => void poll(), 1500);
    return () => {
      disposed = true;
      if (timer) window.clearTimeout(timer);
    };
  }, [observedRun, setArtifacts, setObservedRun, setRestoredStopPending]);

  // Streaming Logic
  const stopStreaming = useCallback(() => {
    const controller = abortControllerRef.current;
    if (!controller) {
      stopRestoredRun();
      return;
    }
    // Preserve the user's terminal intent and ask the owner-checked backend
    // task to cancel before closing SSE. The grace window lets the runtime
    // deliver paired tool_result/tool_call_end plus the cancelled terminal.
    cancelRequestedRef.current = true;
    scheduleCancelFallback(controller);
    const taskId = activeTaskIdRef.current;
    if (taskId) {
      requestTaskCancellation(taskId, controller);
    }
  }, [requestTaskCancellation, scheduleCancelFallback, stopRestoredRun]);

  const sendMessage = useCallback(async (params: {
    messageContent: string;
    filePaths: string[];
    attachments: any[];
    config: SessionConfig;
    selectedDatasets: string[];
    models: any[];
    datasets: any[];
    resumeRunId?: string;
    resumeApprovalId?: string;
    targetAssistantMessageId?: string;
    onRunStarted?: (sessionId: string) => void;
    onSessionBound?: (sessionId: string) => void;
  }) => {
    const {
      messageContent,
      filePaths,
      attachments,
      config,
      selectedDatasets,
      datasets,
      resumeRunId,
      resumeApprovalId,
      targetAssistantMessageId,
    } = params;
    const isResume = Boolean(
      resumeRunId && resumeApprovalId && targetAssistantMessageId,
    );
    lastStreamConfigRef.current = {
      config,
      selectedDatasets,
      models: params.models,
      datasets,
    };

    const createdAt = new Date().toISOString();
    const userMessageId = generateUUID();

    const resumeTarget = isResume
      ? messages.find((message) => message.id === targetAssistantMessageId)
      : undefined;
    if (isResume && !resumeTarget) {
      return;
    }
    if (sendInFlightRef.current || abortControllerRef.current) {
      return;
    }
    if (config.execution_profile && config.execution_profile !== "safe") {
      setHistoryRestoreError("This saved execution profile is no longer available. Select the safe profile before sending.");
      return;
    }
    if (!isResume && !messageContent.trim() && attachments.length === 0) {
      return;
    }
    // Sending or resuming takes ownership from any in-flight history restore.
    // Bump before publishing optimistic messages so a late restore cannot
    // replace the user's new turn.
    retireArtifactRestore();
    setHistoryRestoreState("idle");
    setHistoryRestoreError(null);
    setModelRecreateNeeded(false);
    setSourceRecreateNeeded(false);
    const interactionStartedAtMs = performance.now();
    const streamEpoch = streamEpochRef.current + 1;
    streamEpochRef.current = streamEpoch;
    const isCurrentStream = () => streamEpochRef.current === streamEpoch;
    sendInFlightRef.current = true;

    // 1. Setup UI for new message
    const userMessage: ChatMessageType = {
      id: userMessageId,
      role: "user",
      content: messageContent,
      createdAt,
      parts: buildTextParts(userMessageId, messageContent, createdAt),
      status: "completed",
      attachments: attachments.length > 0 ? attachments : undefined,
    };

    const initialSearchStatus: SearchStatusItem[] = [];
    if (selectedDatasets.length > 0) {
      const datasetNames = selectedDatasets.map(id => datasets.find(d => d.dataset_id === id)?.name || id);
      initialSearchStatus.push({
        type: "kb",
        state: "searching",
        query: messageContent,
        datasets: datasetNames,
      });
    }
    if (config.web_search_enabled) {
      initialSearchStatus.push({
        type: "web",
        state: "searching",
        query: messageContent,
      });
    }

    const assistantMessage: ChatMessageType = isResume
      ? {
          ...resumeTarget!,
          status: "streaming",
          isStreaming: true,
        }
      : {
          id: generateUUID(),
          role: "assistant",
          content: "",
          createdAt,
          parts: [],
          status: "streaming",
          isStreaming: true,
          searchStatus: initialSearchStatus.length > 0 ? initialSearchStatus : undefined,
        };

    if (isResume) {
      setMessages((prev) =>
        prev.map((message) =>
          message.id === assistantMessage.id ? assistantMessage : message,
        ),
      );
    } else {
      setMessages((prev) => [...prev, userMessage, assistantMessage]);
    }
    setServerRunBlocking(false);
    setIsStreaming(true);

    // 2. Bind a session id without waiting for createSession. A new chat
    // mints a client id and opens SSE immediately; persistence is background.
    const preparedSession = beginNewChatSession(
      pendingSessionIdRef.current || activeSessionId || undefined,
    );
    const sessionId = preparedSession.sessionId;
    let shouldRefreshSessionsInBackground = preparedSession.isNew;
    if (preparedSession.isNew) {
      const sessionTitle = messageContent.slice(0, 50);
      pendingSessionIdRef.current = sessionId;
      setAssistantLocalTitles((prev: Record<string, string>) => ({
        ...prev,
        [sessionId]: sessionTitle,
      }));
    } else {
      updateSession(sessionId, { config }).catch(console.error);
    }

    if (!isCurrentStream()) return;

    let persistedRunId = resumeRunId;
    let runStartNotified = false;
    const persistAssistantRunId = async (value: unknown) => {
      const runId = nonEmptyString(value);
      if (!sessionId || !runId || runId === persistedRunId) return;
      persistedRunId = runId;
      try {
        await updateSession(sessionId, {
          metadata: {
            [ASSISTANT_ACTIVE_RUN_METADATA_KEY]: {
              run_id: runId,
              updated_at: new Date().toISOString(),
            },
          },
        });
      } catch {
        console.warn("Assistant run reconnect marker could not be persisted");
      }
    };

    // 3. Start Stream
    clearCancelFallback();
    cancelRequestedRef.current = false;
    activeTaskIdRef.current = null;
    cancelApiTaskIdRef.current = null;
    const streamAbortController = new AbortController();
    abortControllerRef.current = streamAbortController;
    const startTime = Date.now();
    const streamTrace = startChatStreamTrace(
      "assistant",
      {
        sessionId: sessionId || null,
        modelId: config.selected_model || null,
      },
      { interactionStartedAtMs }
    );
    let streamTraceClosed = false;
    const closeStreamTrace = (
      outcome: "completed" | "cancelled" | "failed",
      payload?: Record<string, unknown>
    ) => {
      if (streamTraceClosed) return;
      streamTraceClosed = true;
      finishChatStreamTrace(streamTrace, outcome, payload);
    };
    const refreshSessionsInBackground = () => {
      if (!shouldRefreshSessionsInBackground) return;
      shouldRefreshSessionsInBackground = false;
      void listSessions({ service_id: "__builtin_assistant__", limit: 100 })
        .then(setSessions)
        .catch((error) => {
          console.warn("Assistant session list refresh failed", error);
        });
    };
    let streamTurnState = createStreamTurnState(startTime);
    let approvalHoldOpen = false;
    let approvalRejected = false;
    const streamReducerContext = createStreamReducerContext();
    let firstTokenMs: number | undefined;
    let firstTextTokenMs: number | undefined;
    let content = "";
    let accumulatedThinkingContent = "";
    let thinkingActivityStarted = false;
    let sourceAccessWasRevoked = false;
    let contexts: RetrievedContext[] = [];
    const datasetNames = Object.fromEntries(
      datasets.map((dataset) => [dataset.dataset_id, dataset.name]),
    );
    let webSearchResults: WebSearchResult[] = [];
    let usage: any = {};
    let durationMs: number | undefined;
    const failVisibleStream = (message: string, timestampMs: number) => {
      streamTurnState = failStreamTurn(streamTurnState, message, timestampMs);
    };
    const markFirstResponse = (timestampMs: number) => {
      if (firstTokenMs === undefined) {
        streamTurnState = markStreamFirstToken(streamTurnState, timestampMs);
        firstTokenMs = streamTurnState.firstTokenMs;
        if (firstTokenMs !== undefined) {
          markChatStreamFirstToken(streamTrace, firstTokenMs);
        }
      }
    };
    const markFirstTextResponse = (timestampMs: number) => {
      if (firstTextTokenMs === undefined) {
        firstTextTokenMs = timestampMs - startTime;
        markChatStreamFirstTextToken(streamTrace, firstTextTokenMs);
      }
    };

    // Helper to update search status
    let searchStatus = [...initialSearchStatus];
    const updateSearchStatus = (type: "kb" | "web", updates: Partial<SearchStatusItem>) => {
      if (!isCurrentStream()) return;
      searchStatus = searchStatus.map((s) => s.type === type ? { ...s, ...updates } : s);
      // Ensure firstTokenMs is passed from the local scope to the state update
      setMessages((prev) => prev.map((m) => m.id === assistantMessage.id ? { ...m, searchStatus, firstTokenMs } : m));
    };

    const updateAssistantMessage = (updater: (m: ChatMessageType) => ChatMessageType) => {
      if (!isCurrentStream()) return;
      setMessages((prev) => updateMessageById(prev, assistantMessage.id, updater));
    };

    // RAF-batched sync: buffer turn state updates and flush once per frame
    // to avoid ~60 setState calls/s during token streaming.
    let pendingSyncTurnState = false;
    let syncRafId: number | null = null;

    const flushTurnStateToMessage = () => {
      if (!pendingSyncTurnState) return;
      pendingSyncTurnState = false;
      if (!isCurrentStream()) return;

      content = streamTurnState.content;
      firstTokenMs = streamTurnState.firstTokenMs ?? firstTokenMs;
      durationMs = streamTurnState.durationMs ?? durationMs;
      if (streamTurnState.firstTokenMs != null) {
        markChatStreamFirstToken(streamTrace, streamTurnState.firstTokenMs);
      }
      usage = mergeUsageWithTurnState(usage, streamTurnState);

      setMessages((prev) =>
        updateMessageById(prev, assistantMessage.id, (m) => ({
                ...m,
                content,
                parts: buildTextParts(assistantMessage.id, content, createdAt),
                firstTokenMs,
                durationMs,
                usage,
                toolCalls: mapStreamToolCallsToAssistant(streamTurnState),
                status:
                  streamTurnState.status === "idle"
                    ? "streaming"
                    : streamTurnState.status,
                isStreaming: streamTurnState.status === "streaming",
              }))
      );
    };

    const syncTurnStateToMessage = () => {
      pendingSyncTurnState = true;
      if (syncRafId === null) {
        syncRafId = requestAnimationFrame(() => {
          syncRafId = null;
          flushTurnStateToMessage();
        });
      }
    };

    const terminalLatch = createStreamTerminalLatch();
    // The turn parks on an approval only until the operator decides. Once the
    // run reaches a terminal outcome the hold is over, whether or not the
    // runtime echoed an approval_result — otherwise a granted approval would
    // leave the composer locked and "Stop generating" showing forever.
    const approvalHoldActive = () => approvalHoldOpen && !terminalLatch.current();
    const settleRunTerminal = (
      outcome: StreamTerminalOutcome,
      timestampMs: number,
      options: {
        error?: string;
        runId?: string;
        showInterruptionNotice?: boolean;
        terminalReason?: "runtime_restart_interrupted";
      } = {},
    ): boolean => {
      if (!terminalLatch.accept(outcome)) return false;

      if (outcome === "succeeded") {
        streamTurnState = completeStreamTurn(streamTurnState, timestampMs);
      } else if (outcome === "cancelled") {
        streamTurnState = cancelStreamTurn(streamTurnState, timestampMs);
      } else if (options.showInterruptionNotice === false) {
        streamTurnState = failStreamTurn(
          streamTurnState,
          options.error || "assistant_run_failed",
          timestampMs,
        );
      } else {
        failVisibleStream(options.error || "assistant_run_failed", timestampMs);
      }
      syncTurnStateToMessage();

      if (outcome !== "succeeded") {
        setWorkingMemory((prev) =>
          prev
            ? {
                ...prev,
                error:
                  options.error ||
                  (outcome === "cancelled" ? "cancelled" : "assistant_run_failed"),
              }
            : null,
        );
      }
      updateAssistantMessage((message) => {
        const previous = message.processSummary;
        const seeded = previous
          ? {
              ...previous,
              runId: options.runId || previous.runId,
              terminalReason: options.terminalReason || previous.terminalReason,
            }
          : { ...initProcessSummary(options.runId, timestampMs), terminalReason: options.terminalReason };
        return {
          ...message,
          processSummary: finalizeProcessSummary(
            seeded,
            outcome,
            timestampMs,
            true,
          ),
        };
      });
      return true;
    };

    const reconcileCancellationReceipt = async () => {
      const runId = messagesRef.current.find((m) => m.id === assistantMessage.id)?.processSummary?.runId;
      if (!runId) return;
      for (let attempt = 0; attempt < 2; attempt++) {
        try {
          const history = await getAssistantSessionHistory(sessionId, 200);
          if (!isCurrentStream()) return;
          const facts = cancelledReceiptFlags(history.messages, runId);
          if (facts.outcomeUncertain || facts.sourceAccessRevoked) {
            updateAssistantMessage((message) => {
              const next = { ...message, outcomeUncertain: message.outcomeUncertain || facts.outcomeUncertain };
              return facts.sourceAccessRevoked ? redactSourceMessage(next) : next;
            });
            if (facts.sourceAccessRevoked) setSourceRecreateNeeded(true);
          }
          if (facts.outcomeUncertain) return;
        } catch {
          // A missing read receipt cannot change the execution outcome.
        }
        if (attempt === 0) await new Promise((resolve) => window.setTimeout(resolve, 350));
      }
    };

    const activityQueue = createActivityFlushQueue({
      scheduleFlush: (flush) => {
        requestAnimationFrame(flush);
      },
      apply: (batch) => {
        if (!isCurrentStream()) return;
        updateAssistantMessage((message) => {
          let next = message;
          if (batch.thinkingStart) {
            const thinkStartMs = Date.now();
            const prev = next.processSummary ?? initProcessSummary(undefined, thinkStartMs);
            next = {
              ...next,
              isThinkingStreaming: true,
              streamingThinkingContent: "",
              processSummary: { ...prev, thinkingStartedAt: thinkStartMs },
            };
          }
          if (batch.thinkingDelta) {
            next = {
              ...next,
              streamingThinkingContent: (next.streamingThinkingContent || "") + batch.thinkingDelta,
              firstTokenMs,
            };
          }
          if (batch.thinkingEnd !== null) {
            const thinkEndMs = Date.now();
            const prev = next.processSummary;
            const thinkingDuration = prev?.thinkingStartedAt
              ? thinkEndMs - prev.thinkingStartedAt
              : undefined;
            next = {
              ...next,
              isThinkingStreaming: false,
              thinkingContent: batch.thinkingEnd.trim() || next.streamingThinkingContent?.trim() || "",
              streamingThinkingContent: undefined,
              ...(prev && thinkingDuration
                ? { processSummary: { ...prev, thinkingDurationMs: thinkingDuration } }
                : {}),
            };
          }
          for (const subagent of batch.subagentEvents) {
            next = {
              ...next,
              activeSubAgents: reduceSubAgentEvent(
                next.activeSubAgents ?? [],
                subagent.eventType,
                subagent.data,
                subagent.now,
              ),
            };
          }
          return next;
        });
      },
    });

    try {
      const styleSystemPrompt = getStyleSystemPrompt(config.selected_style || "default");
      const history: AssistantMessage[] = messages.map((m) => ({ role: m.role, content: m.content }));

      const localNodeBinding = localNodeBindingRef.current?.();
      const localNodeEnabled =
        config.os_agent_enabled === true &&
        (osAgentEligibilityRef.current?.() ?? false) &&
        Boolean(localNodeBinding);
      const sessionTitle = messageContent.slice(0, 50);
      const stream = startChatWithoutAwaitingSessionCreate({
        sessionId,
        isNew: preparedSession.isNew,
        persistSession: preparedSession.isNew
          ? (id) =>
              persistNewChatSession(createSession, updateSession, {
                sessionId: id,
                title: sessionTitle,
                config,
              }).then((created) => {
                if (pendingSessionIdRef.current === id) {
                  params.onSessionBound?.(created.session_id);
                  setActiveSessionId(created.session_id);
                  pendingSessionIdRef.current = undefined;
                }
                return created;
              }).catch((error) => {
                console.error("Failed to persist session:", error);
              })
          : undefined,
        openStream: (id) => chatStream({
        message: messageContent,
        session_id: id || undefined,
        history,
        model_id: config.selected_model || undefined,
        provider_id: config.selected_provider || undefined,
        temperature: config.temperature,
        system_prompt: styleSystemPrompt || undefined,
        kb_dataset_ids: config.selected_datasets,
        kb_mode: config.selected_datasets?.length ? "auto" : "off",
        kb_top_k: 5,
        kb_include_images: false,
        web_search_enabled: config.web_search_enabled,
        reasoning_option: config.reasoning_option || config.thinking_level || "auto",
        web_search_max_results: 5,
        file_paths: filePaths.length > 0 ? filePaths : undefined,
        execution_profile: "safe",
        memory_mode: config.memory_mode || "auto",
        os_agent_enabled: localNodeEnabled,
        local_node_device_id: localNodeEnabled ? localNodeBinding?.deviceId : undefined,
        local_node_grant_ids: localNodeEnabled ? localNodeBinding?.grantIds : undefined,
        resume_run_id: resumeRunId,
        resume_approval_id: resumeApprovalId,
      }, streamAbortController.signal, {
        onConnectionState: (streamConnectionState) => {
          if (!isCurrentStream() || streamAbortController.signal.aborted) return;
          setMessages(previous => previous.map(message => message.id === assistantMessage.id ? { ...message, streamConnectionState } : message));
        },
      }),
      });

      for await (const event of stream) {
        // The chat request is active before this background sidebar refresh.
        // A slow session-list query must never delay the first visible response.
        refreshSessionsInBackground();
        if (
          !isCurrentStream() ||
          streamAbortController.signal.aborted
        ) {
          break;
        }
        if (
          cancelRequestedRef.current &&
          !CANCEL_CLOSURE_EVENT_TYPES.has(event.event_type)
        ) {
          continue;
        }
        const now = Date.now();
        const eventPayload =
          typeof event.data === "object" && event.data !== null
            ? (event.data as Record<string, unknown>)
            : undefined;
        if (eventPayload?.source_access_revoked === true) {
          activityQueue.flushNow();
          content = "";
          accumulatedThinkingContent = "";
          contexts = [];
          webSearchResults = [];
          streamTurnState = { ...streamTurnState, content: "", toolCalls: [] };
          setSourceRecreateNeeded(true);
          setArtifacts([]);
          updateAssistantMessage(redactSourceMessage);
          if (!sourceAccessWasRevoked) {
            sourceAccessWasRevoked = true;
            // Read-only reconciliation covers earlier inherited answers too.
            void Promise.all([getAssistantSessionHistory(sessionId, 200), getSessionArtifacts(sessionId).catch(() => [])]).then(([history, allowedArtifacts]) => {
              if (!isCurrentStream()) return;
              const restrictedRuns = new Set(history.messages.filter((m) => m.metadata?.source_access_revoked).map((m) => m.metadata?.runtime_run_id).filter((runId) => typeof runId === "string" && runId.length > 0));
              const restrictedIds = new Set(history.messages.flatMap((m, index) => m.metadata?.source_access_revoked ? [`${sessionId}-${index}`] : []));
              setMessages((previous) => previous.map((m) => restrictedRuns.has(m.processSummary?.runId ?? m.runtimeRunId) || restrictedIds.has(m.id) ? redactSourceMessage(m) : m));
              setArtifacts(allowedArtifacts.map(toArtifact));
            }).catch(() => {
              if (isCurrentStream()) setMessages((previous) => previous.map(redactSourceMessage));
            });
          }
        }
        const toolCallForReducer = (() => {
          if (
            event.event_type !== SSEEventType.TOOL_CALL_START &&
            event.event_type !== "tool_call_delta" &&
            event.event_type !== SSEEventType.TOOL_CALL_END &&
            event.event_type !== SSEEventType.TOOL_CALL_RESULT
          ) {
            return undefined;
          }
          if (!eventPayload) return undefined;
          const toolCallId =
            (typeof eventPayload.tool_call_id === "string" &&
              eventPayload.tool_call_id) ||
            (typeof eventPayload.id === "string" ? eventPayload.id : "");
          if (!toolCallId) return undefined;
          return {
            tool_call_id: toolCallId,
            // Don't fall back to the literal "tool" — an empty string lets
            // the downstream reducer preserve the name set by an earlier
            // event with this same tool_call_id (tool_call_start usually
            // carries the real name; tool_call_end/result sometimes drop
            // it, and overwriting would mask the original).
            name:
              (typeof eventPayload.tool_name === "string" &&
                eventPayload.tool_name) ||
              (typeof eventPayload.name === "string" ? eventPayload.name : ""),
            arguments: normalizeUnknownToString(eventPayload.arguments),
            status:
              event.event_type === SSEEventType.TOOL_CALL_END ||
              event.event_type === SSEEventType.TOOL_CALL_RESULT
                ? ("completed" as const)
                : ("running" as const),
          };
        })();
        const reducerContent =
          typeof event.data === "string"
            ? { type: "text" as const, data: event.data }
            : event.event_type === SSEEventType.TOOL_CALL_RESULT
              ? {
                  type: "tool_result" as const,
                  data: normalizeUnknownToString(eventPayload?.result),
                }
              : event.event_type === "error"
                ? {
                    type: "text" as const,
                    data: normalizeUnknownToString(eventPayload?.message),
                  }
                : undefined;
        const reduced = reduceLegacyStreamChunk(
          streamTurnState,
          {
            event_type: event.event_type,
            content: reducerContent,
            metadata: eventPayload,
            tool_call: toolCallForReducer,
          },
          streamReducerContext,
          now
        );
        streamTurnState = reduced.state;
        if (reduced.changed) {
          syncTurnStateToMessage();
        }

        // Event Handling
        switch (event.event_type) {
          case SSEEventType.STARTED:
            // Immediate response received - stream connection established
            break;

          case "text_delta": {
            const textDelta =
              typeof event.data === "string"
                ? event.data
                : (event.data as { content?: string })?.content || "";
            if (textDelta) {
              markFirstResponse(now);
              markFirstTextResponse(now);
            }
            break;
          }

          case SSEEventType.THINKING_START:
          case "thinking_start": {
            markFirstResponse(now);
            if (!thinkingActivityStarted) {
              thinkingActivityStarted = true;
              activityQueue.enqueue({ kind: "thinking_start" });
            }
            break;
          }

          case SSEEventType.THINKING_DELTA:
          case "thinking_delta": {
            const thinkingDelta = typeof event.data === "string"
              ? event.data
              : (event.data as { content?: string })?.content || "";
            if (thinkingDelta) {
              markFirstResponse(now);
              if (!thinkingActivityStarted) {
                thinkingActivityStarted = true;
                activityQueue.enqueue({ kind: "thinking_start" });
              }
              accumulatedThinkingContent += thinkingDelta;
              activityQueue.enqueue({ kind: "thinking_delta", text: thinkingDelta });
            }
            break;
          }

          case SSEEventType.THINKING_END:
          case "thinking_end": {
            const thinkingText = typeof event.data === "string"
              ? event.data
              : ((event.data as { content?: string } | undefined)?.content || "");
            accumulatedThinkingContent =
              thinkingText.trim() || accumulatedThinkingContent;
            activityQueue.enqueue({
              kind: "thinking_end",
              text: accumulatedThinkingContent,
            });
            break;
          }

          // ADR-003: Sub-Agent events
          case SSEEventType.SUBAGENT_STARTED:
          case SSEEventType.SUBAGENT_STEP:
          case SSEEventType.SUBAGENT_TEXT_DELTA:
          case SSEEventType.SUBAGENT_TOOL_START:
          case SSEEventType.SUBAGENT_TOOL_RESULT:
          case SSEEventType.SUBAGENT_FINISHED:
            activityQueue.enqueue({
              kind: "subagent",
              eventType: event.event_type,
              data: event.data,
              now,
            });
            break;

          case SSEEventType.STATUS:
            // 状态事件不计入 TTFT - TTFT 只测量真正的文本内容出现时间
            // STATUS 事件是预处理阶段（如 "分析任务需求..."），不是最终内容
            const statusData = event.data as {
              status?: string;
              message: string;
              phase?: ReActPhase;
              is_document_task?: boolean;
              task_id?: string;
            };

            // Handle ReAct phase status (new format with "phase" field)
            if (statusData.phase) {
              const phaseStatus: AgentPhaseStatus = {
                phase: statusData.phase,
                message: sanitizeProgressLabel(statusData.message) ?? "",
                isDocumentTask: statusData.is_document_task,
                taskId: statusData.task_id,
              };
              setMessages(prev => prev.map(m =>
                m.id === assistantMessage.id
                  ? { ...m, agentPhase: phaseStatus, firstTokenMs }
                  : m
              ));
              break;
            }

            // Handle legacy search status (old format with "status" field)
            let statusType: "kb" | "web" | "files" | null = null;
            if (statusData.status === "searching_kb") statusType = "kb";
            else if (statusData.status === "searching_web") statusType = "web";
            else if (statusData.status === "processing_files") statusType = "files";

            if (statusType) {
               // Check if status entry already exists, if not add it
               if (!searchStatus.some(s => s.type === statusType)) {
                 searchStatus = [...searchStatus, {
                   type: statusType,
                   state: "searching",
                   query: statusData.message
                 }];
                 setMessages(prev => prev.map(m => m.id === assistantMessage.id ? { ...m, searchStatus, firstTokenMs } : m));
               }
            }
            break;

          case SSEEventType.ACTIVITY: {
            const activityData = (event.data || {}) as Record<string, unknown>;
            const label =
              (typeof activityData.summary === "string" && activityData.summary) ||
              (typeof activityData.message === "string" && activityData.message) ||
              (typeof activityData.name === "string" && activityData.name);
            if (label) {
              updateAssistantMessage((m) => ({
                ...m,
                processSummary: {
                  ...(m.processSummary ?? initProcessSummary(undefined, now)),
                  currentStep: sanitizeProgressLabel(label),
                },
              }));
            }
            break;
          }
          
          case "context_retrieved":
            // Runtime emits one event per search call. Aggregate by the
            // selected dataset identity instead of counting calls as KBs.
            contexts = mergeKnowledgeContexts(
              contexts,
              event.data,
              selectedDatasets,
              datasetNames,
            );
            const totalResults = contexts.reduce((sum, c) => sum + (c.chunks?.length || 0), 0);
            const totalDuration = contexts.reduce((sum, c) => sum + (c.took_ms || 0), 0);
            updateSearchStatus("kb", { state: "completed", resultCount: totalResults, durationMs: totalDuration });
            setMessages(prev => prev.map(m => m.id === assistantMessage.id ? { ...m, contexts, searchStatus, firstTokenMs } : m));
            break;

          case "web_search_results":
            const webData = event.data as any;
            if (webData.results && Array.isArray(webData.results)) {
              webSearchResults = webData.results;
              updateSearchStatus("web", { state: "completed", resultCount: webData.results.length, durationMs: webData.response_time_ms });
              setMessages(prev => prev.map(m => m.id === assistantMessage.id ? { ...m, webSearchResults, searchStatus } : m));
            }
            break;

          case "file_processed":
             const fileData = event.data as FileProcessedEventData;
             const fileCount =
               fileData.file_count ??
               fileData.file_metadata?.length ??
               fileData.image_count + (fileData.text_length > 0 ? 1 : 0);
             // Update existing "files" entry or add new one
             const hasFilesEntry = searchStatus.some(s => s.type === "files");
             if (hasFilesEntry) {
               searchStatus = searchStatus.map(s =>
                 s.type === "files" ? { ...s, state: "completed" as const, resultCount: fileCount } : s
               );
             } else {
               searchStatus = [...searchStatus, { type: "files" as const, state: "completed" as const, resultCount: fileCount }];
             }
             setMessages(prev => prev.map(m => m.id === assistantMessage.id ? { ...m, searchStatus } : m));
             break;

          case "rag_evaluation":
             const evalData = event.data as RAGEvaluationEventData;
             const ragEvaluation: RAGEvaluation = {
                quality_score: evalData.quality_score,
                quality_breakdown: evalData.quality_breakdown,
                chunks_retrieved: evalData.chunks_retrieved,
                chunks_used: evalData.chunks_used,
                response_grounding: evalData.response_grounding,
                citations: evalData.citations,
                evaluation_time_ms: evalData.evaluation_time_ms,
             };
             setMessages(prev => prev.map(m => m.id === assistantMessage.id ? { ...m, ragEvaluation, ragCitations: evalData.citations } : m));
             break;

          case SSEEventType.CONTEXT_BUDGET:
            updateAssistantMessage((m) => {
              const prev = m.processSummary ?? initProcessSummary(undefined, now);
              const budgetData = (event.data || {}) as Record<string, unknown>;
              return {
                ...m,
                processSummary: {
                  ...prev,
                  contextBudget: {
                    used_tokens:
                      typeof budgetData.used_tokens === "number" ? budgetData.used_tokens : undefined,
                    model_context_window:
                      typeof budgetData.model_context_window === "number"
                        ? budgetData.model_context_window
                        : undefined,
                    dropped_history_messages:
                      typeof budgetData.dropped_history_messages === "number"
                        ? budgetData.dropped_history_messages
                        : undefined,
                  },
                },
              };
            });
            break;

          case SSEEventType.CONTEXT_COMPACTED:
          case SSEEventType.CONTEXT_COMPACTION:
            updateAssistantMessage((m) => {
              const prev = m.processSummary ?? initProcessSummary(undefined, now);
              const compactData = (event.data || {}) as Record<string, unknown>;
              return {
                ...m,
                processSummary: {
                  ...prev,
                  contextCompacted: {
                    compacted:
                      typeof compactData.compacted === "boolean" ? compactData.compacted : undefined,
                    dropped_history_messages:
                      typeof compactData.dropped_history_messages === "number"
                        ? compactData.dropped_history_messages
                        : undefined,
                  },
                },
              };
            });
            break;

          case SSEEventType.QUEUE_STATE:
            updateAssistantMessage((m) => {
              const prev = m.processSummary ?? initProcessSummary(undefined, now);
              const queueData = (event.data || {}) as Record<string, unknown>;
              const toolId =
                (typeof queueData.tool_id === "string" && queueData.tool_id) ||
                (typeof queueData.command_id === "string" && queueData.command_id) ||
                "";
              if (!toolId) return m;
              const existing = prev.tools.find((tool) => tool.id === toolId);
              if (!existing) return m;
              return {
                ...m,
                processSummary: {
                  ...prev,
                  tools: upsertTool(prev.tools, {
                    id: existing.id,
                    name: existing.name,
                    status: existing.status,
                    queueState: typeof queueData.state === "string" ? queueData.state : existing.queueState,
                  }),
                },
              };
            });
            break;

          case SSEEventType.APPROVAL_REQUIRED:
            const approvalData = (event.data || {}) as Record<string, unknown>;
            const approvalToolId = eventToolId(approvalData);
            const approvalToolName =
              typeof approvalData.tool_name === "string"
                ? approvalData.tool_name
                : approvalToolId;
            approvalHoldOpen = true;
            approvalRejected = false;
            updateAssistantMessage((m) => {
              const prev = m.processSummary ?? initProcessSummary(undefined, now);
              if (!approvalToolId) return m;
              const existing = prev.tools.find((tool) => tool.id === approvalToolId);
              const toolName =
                existing?.name ||
                approvalToolName;
              const runId =
                typeof approvalData.run_id === "string"
                  ? approvalData.run_id
                  : prev.runId;
              return {
                ...m,
                isStreaming: true,
                status: "streaming",
                processSummary: {
                  ...prev,
                  collapsed: false,
                  status: "blocked",
                  runId,
                  runtimeThreadId:
                    typeof approvalData.thread_id === "string"
                      ? approvalData.thread_id
                      : prev.runtimeThreadId,
                  tools: upsertTool(prev.tools, {
                    id: approvalToolId,
                    name: toolName,
                    status: "approval_required",
                    approvalId:
                      typeof approvalData.approval_id === "string"
                        ? approvalData.approval_id
                        : existing?.approvalId,
                    summary:
                      typeof approvalData.reason === "string"
                        ? approvalData.reason
                        : existing?.summary ||
                          summarizeToolArguments(approvalData.arguments),
                  }),
                },
              };
            });
            await persistAssistantRunId(approvalData.run_id);
            if (!isCurrentStream()) return;
            break;

          case SSEEventType.SIDE_EFFECT_UNKNOWN:
            const sideEffectData = (event.data || {}) as Record<string, unknown>;
            updateAssistantMessage((message) => ({ ...message, outcomeUncertain: true }));
            settleRunTerminal("failed", now, {
              error: "side_effect_unknown",
              runId:
                typeof sideEffectData.run_id === "string"
                  ? sideEffectData.run_id
                  : undefined,
              showInterruptionNotice: false,
            });
            await persistAssistantRunId(sideEffectData.run_id);
            if (!isCurrentStream()) return;
            break;

          case SSEEventType.RAG_RETRIEVAL_STARTED:
          case SSEEventType.RAG_RETRIEVAL_COMPLETED:
          case SSEEventType.RAG_RETRIEVAL_FAILED:
            updateAssistantMessage((m) => {
              const prev = m.processSummary ?? initProcessSummary(undefined, now);
              const ragData = (event.data || {}) as Record<string, unknown>;
              const toolId =
                typeof ragData.tool_id === "string" ? ragData.tool_id : "rag-retrieval";
              const existing = prev.tools.find((tool) => tool.id === toolId);
              const status =
                event.event_type === SSEEventType.RAG_RETRIEVAL_FAILED
                  ? "error"
                  : event.event_type === SSEEventType.RAG_RETRIEVAL_COMPLETED
                    ? "completed"
                    : "running";
              return {
                ...m,
                processSummary: {
                  ...prev,
                  collapsed: false,
                  tools: upsertTool(prev.tools, {
                    id: toolId,
                    name: existing?.name || "search_knowledge_base",
                    status,
                    summary:
                      typeof ragData.query === "string"
                        ? ragData.query
                        : existing?.summary,
                  }),
                },
              };
            });
            break;

          case SSEEventType.APPROVAL_RESULT:
            approvalHoldOpen = false;
            approvalRejected =
              (event.data as { approved?: unknown; status?: unknown } | null)?.approved === false ||
              String((event.data as { status?: unknown } | null)?.status ?? "").toLowerCase() === "rejected";
            updateAssistantMessage((m) => {
              const prev = m.processSummary ?? initProcessSummary(undefined, now);
              const approvalResultData = (event.data || {}) as Record<string, unknown>;
              const toolId = eventToolId(approvalResultData);
              if (!toolId) return m;
              const existing = prev.tools.find((tool) => tool.id === toolId);
              const approved = approvalResultData.approved === true;
              const toolName =
                existing?.name ||
                (typeof approvalResultData.tool_name === "string"
                  ? approvalResultData.tool_name
                  : toolId);
              return {
                ...m,
                processSummary: {
                  ...prev,
                  tools: upsertTool(prev.tools, {
                    id: toolId,
                    name: toolName,
                    status: approved ? "running" : "error",
                    summary:
                      typeof approvalResultData.reason === "string"
                        ? approvalResultData.reason
                        : existing?.summary,
                  }),
                },
              };
            });
            break;

          case SSEEventType.GATEWAY_DECISION:
            updateAssistantMessage((m) => {
              const prev = m.processSummary ?? initProcessSummary(undefined, now);
              const decisionData = (event.data || {}) as Record<string, unknown>;
              const toolId = typeof decisionData.tool_id === "string" ? decisionData.tool_id : "";
              if (!toolId) return m;
              const existing = prev.tools.find((tool) => tool.id === toolId);
              const reason = typeof decisionData.reason === "string" ? decisionData.reason : undefined;
              const toolName =
                existing?.name ||
                (typeof decisionData.tool_name === "string" ? decisionData.tool_name : toolId);
              return {
                ...m,
                processSummary: {
                  ...prev,
                  tools: upsertTool(prev.tools, {
                    id: toolId,
                    name: toolName,
                    status: existing?.status || "running",
                    summary: reason || existing?.summary,
                  }),
                },
              };
            });
            break;

          // === AG-UI Lifecycle Events ===
          case SSEEventType.RUN_STARTED:
            // Agent execution started - initialize working memory if needed
            const runStartedData = event.data as {
              run_id?: string;
              thread_id?: string;
              session_id?: string;
              task_id?: string | null;
              timestamp?: number;
              reasoning?: {
                requested_option?: string;
                effective_option?: string;
                canonical_effort?: string | null;
                adapter_id?: string;
                capability_revision?: number;
                fallback_reason?: string | null;
              } | null;
              model_id?: string;
              temperature?: number;
            };
            const acceptedSessionId = acceptPendingRunSession({
              requestedSessionId: sessionId,
              pendingSessionId: pendingSessionIdRef.current,
              eventSessionId: runStartedData?.session_id,
            });
            if (acceptedSessionId && isCurrentStream()) {
              setActiveSessionId(acceptedSessionId);
              pendingSessionIdRef.current = undefined;
            }
            if (
              !runStartNotified && isCurrentStream() &&
              confirmsAssistantRunAdmission({
                requestedSessionId: sessionId,
                eventSessionId: runStartedData?.session_id,
                isResume,
              })
            ) {
              runStartNotified = true;
              params.onRunStarted?.(sessionId);
            }
            if (runStartedData?.task_id) {
              activeTaskIdRef.current = runStartedData.task_id;
              if (cancelRequestedRef.current) {
                requestTaskCancellation(runStartedData.task_id, streamAbortController);
              }
            }
            if (!workingMemory) {
              setWorkingMemory({
                goal: "",
                tasks: [],
                collectedInfo: [],
                notes: [],
                runId: runStartedData?.run_id,
              });
            }
            setShowTaskPanel(true);
            updateAssistantMessage((m) => ({
              ...m,
              processSummary: {
                ...initProcessSummary(
                  runStartedData?.run_id,
                  runStartedData?.timestamp ?? now,
                ),
                reasoning: runStartedData?.reasoning ?? undefined,
                modelId: runStartedData?.model_id,
                temperature: runStartedData?.temperature,
                runtimeThreadId: runStartedData?.thread_id,
              },
            }));
            await persistAssistantRunId(runStartedData?.run_id);
            if (!isCurrentStream()) return;
            break;

          case SSEEventType.RUN_FINISHED:
            // Agent execution completed
            // Working memory stays visible for user reference
            settleRunTerminal("succeeded", now, {
              runId:
                typeof (event.data as { run_id?: unknown } | null)?.run_id === "string"
                  ? (event.data as { run_id: string }).run_id
                  : undefined,
            });
            break;

          case SSEEventType.RUN_ERROR:
            // A user-requested stop is transported as run_error with a
            // cancelled terminal envelope. Keep it distinct from failures.
            const runErrorData = event.data as {
              error?: string;
              message?: string;
              run_id?: string;
              status?: string;
              error_code?: string;
              terminal_envelope?: { status?: string; exit_reason?: string };
            };
            const runWasCancelled =
              runErrorData.status === "cancelled" ||
              runErrorData.terminal_envelope?.status === "cancelled";
            if (runErrorData.terminal_envelope?.exit_reason === "side_effect_unknown") {
              updateAssistantMessage((message) => ({ ...message, outcomeUncertain: true }));
            }
            settleRunTerminal(runWasCancelled ? "cancelled" : "failed", now, {
              error:
                runErrorData.error ||
                runErrorData.message ||
                (runWasCancelled ? "cancelled" : "assistant_run_failed"),
              runId: runErrorData.run_id,
              showInterruptionNotice:
                runWasCancelled ||
                isExpectedApprovalRejection(
                  runErrorData as unknown as Record<string, unknown>,
                  approvalRejected,
                )
                  ? false
                  : undefined,
              terminalReason: runWasCancelled &&
                runErrorData.error_code === "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_ORPHANED"
                  ? "runtime_restart_interrupted" : undefined,
            });
            await persistAssistantRunId(runErrorData.run_id);
            if (!isCurrentStream()) return;
            break;

          case SSEEventType.CANCELLED:
            settleRunTerminal("cancelled", now, { error: "cancelled" });
            break;

          // === AG-UI Step Events (Manus-style) ===
          case SSEEventType.STEP_STARTED:
            // Agent-first workflows may do tool planning before text generation.
            // Treat the first visible step as first response to avoid misleading TTFT.
            markFirstResponse(now);
            const stepStartData = event.data as {
              step_id: string;
              title: string;
              description?: string;
              icon?: string;
              timestamp: number;
            };
            const sanitizedStepTitle = sanitizeProgressLabel(stepStartData.title);
            const isNoiseStep = isZeroToolNoise(stepStartData.title);
            const stepTitle = sanitizedStepTitle || stepStartData.title || stepStartData.step_id;
            // Add new step to working memory tasks
            setWorkingMemory((prev) => {
              if (isNoiseStep) return prev;
              if (!prev) {
                return {
                  goal: "",
                  tasks: [{
                    id: stepStartData.step_id,
                    description: stepTitle,
                    status: "in_progress",
                    icon: stepStartData.icon,
                    startTime: stepStartData.timestamp,
                  }],
                  collectedInfo: [],
                  notes: [],
                };
              }
              // Check if task already exists (from TASK_PLANNING)
              const existingTask = prev.tasks.find((t) => t.id === stepStartData.step_id);
              if (existingTask) {
                return {
                  ...prev,
                  tasks: prev.tasks.map((t) =>
                    t.id === stepStartData.step_id
                      ? {
                          ...t,
                          description: stepTitle,
                          status: "in_progress",
                          icon: stepStartData.icon,
                          startTime: stepStartData.timestamp,
                        }
                      : t
                  ),
                };
              }
              // Add new task
              return {
                ...prev,
                tasks: [...prev.tasks, {
                  id: stepStartData.step_id,
                  description: stepTitle,
                  status: "in_progress",
                  icon: stepStartData.icon,
                  startTime: stepStartData.timestamp,
                }],
              };
            });
            setShowTaskPanel(true);
            updateAssistantMessage((m) => {
              const prev = m.processSummary ?? initProcessSummary(undefined, now);
              if (isNoiseStep) {
                return {
                  ...m,
                  processSummary: {
                    ...prev,
                    currentStep: undefined,
                  },
                };
              }
              return {
                ...m,
                processSummary: {
                  ...prev,
                  currentStep: stepTitle,
                  steps: upsertStep(prev.steps, {
                    id: stepStartData.step_id,
                    title: stepTitle,
                    description: stepStartData.description,
                    status: "running",
                    startedAt: stepStartData.timestamp ?? now,
                  }),
                },
              };
            });
            break;

          case SSEEventType.STEP_FINISHED:
            const stepFinishData = event.data as {
              step_id: string;
              status: "completed" | "failed" | "skipped";
              result?: string;
              error?: string;
              duration_ms?: number;
              timestamp: number;
            };
            setWorkingMemory((prev) => prev ? {
              ...prev,
              tasks: prev.tasks.map((t) =>
                t.id === stepFinishData.step_id
                  ? {
                      ...t,
                      status: stepFinishData.status,
                      result: stepFinishData.result,
                      error: stepFinishData.error,
                      durationMs: stepFinishData.duration_ms,
                      endTime: stepFinishData.timestamp,
                    }
                  : t
              ),
            } : null);
            updateAssistantMessage((m) => {
              const prev = m.processSummary ?? initProcessSummary(undefined, now);
              const status: ProcessStepItem["status"] =
                stepFinishData.status === "failed" ? "failed" : "completed";
              const existingStep = prev.steps.find((s) => s.id === stepFinishData.step_id);
              if (!existingStep) return m;
              return {
                ...m,
                processSummary: {
                  ...prev,
                  steps: upsertStep(prev.steps, {
                    id: stepFinishData.step_id,
                    title: existingStep.title,
                    status,
                    finishedAt: stepFinishData.timestamp ?? now,
                    durationMs: stepFinishData.duration_ms,
                    error: stepFinishData.error,
                  }),
                },
              };
            });
            break;

          // === AG-UI Tool Call Events ===
          case SSEEventType.TOOL_CALL_START:
            markFirstResponse(now);
            const toolStartData = event.data as {
              tool_call_id: string;
              tool_name: string;
              arguments?: Record<string, unknown>;
              step_id?: string;
              timestamp: number;
            };
            // Update the parent step with sub-task info (if step_id exists)
            if (toolStartData.step_id) {
              setWorkingMemory((prev) => prev ? {
                ...prev,
                tasks: prev.tasks.map((t) =>
                  t.id === toolStartData.step_id
                    ? {
                        ...t,
                        currentTool: toolStartData.tool_name,
                        subTasks: [...(t.subTasks || []), {
                          id: toolStartData.tool_call_id,
                          name: toolStartData.tool_name,
                          status: "running",
                          startTime: toolStartData.timestamp,
                        }],
                      }
                    : t
                ),
              } : null);
            }
            updateAssistantMessage((m) => {
              const prev = m.processSummary ?? initProcessSummary(undefined, now);
              const existing = prev.tools.find((tool) => tool.id === toolStartData.tool_call_id);
              const awaitingApproval = existing?.status === "approval_required";
              return {
                ...m,
                processSummary: {
                  ...prev,
                  tools: upsertTool(prev.tools, {
                    id: toolStartData.tool_call_id,
                    name: existing?.name || toolStartData.tool_name,
                    status: awaitingApproval ? "approval_required" : "running",
                    startedAt: existing?.startedAt ?? toolStartData.timestamp ?? now,
                    approvalId: existing?.approvalId,
                    summary:
                      existing?.summary ||
                      summarizeToolArguments(toolStartData.arguments),
                  }),
                },
              };
            });
            break;

          case SSEEventType.TOOL_CALL_END:
            const toolEndData = event.data as {
              tool_call_id: string;
              status?: string;
              error?: unknown;
              timestamp: number;
            };
            const toolEndStatus = toolEndData.status?.trim().toLowerCase();
            const toolEndFailed =
              toolEndStatus === "cancelled" ||
              toolEndStatus === "not_executed" ||
              toolEndStatus === "error" ||
              toolEndStatus === "failed";
            setWorkingMemory((prev) => prev ? {
              ...prev,
              tasks: prev.tasks.map((t) => ({
                ...t,
                subTasks: (t.subTasks || []).map((st) =>
                  st.id === toolEndData.tool_call_id
                    ? {
                        ...st,
                        status: toolEndFailed ? "failed" : "completed",
                        endTime: toolEndData.timestamp,
                      }
                    : st
                ),
              })),
            } : null);
            updateAssistantMessage((m) => {
              const prev = m.processSummary ?? initProcessSummary(undefined, now);
              const existing = prev.tools.find((tool) => tool.id === toolEndData.tool_call_id);
              if (!existing) return m;
              if (existing.status === "approval_required" && !toolEndFailed) return m;
              return {
                ...m,
                processSummary: {
                  ...prev,
                  tools: upsertTool(prev.tools, {
                    id: existing.id,
                    name: existing.name,
                    status:
                      existing.status === "completed" || existing.status === "error"
                        ? existing.status
                        : (toolEndFailed ? "error" : "completed"),
                    finishedAt: toolEndData.timestamp ?? now,
                    durationMs:
                      existing.startedAt != null
                        ? (toolEndData.timestamp ?? now) - existing.startedAt
                        : existing.durationMs,
                    error: toolEndFailed
                      ? summarizeToolResult(toolEndData.error) || toolEndStatus
                      : existing.error,
                  }),
                },
              };
            });
            break;

          case SSEEventType.TOOL_CALL_RESULT:
            const toolResultData = event.data as {
              tool_call_id: string;
              tool_name?: string;
              result?: unknown;
              result_preview?: unknown;
              status?: string;
              success?: boolean;
              error?: unknown;
              result_count?: number;  // From backend metadata.total_results
              duration_ms?: number;
              timestamp: number;
            };
            const toolResultStatus = toolResultData.status?.trim().toLowerCase();
            const toolResultHasError =
              typeof toolResultData.error === "string"
                ? toolResultData.error.trim().length > 0
                : toolResultData.error != null;
            const toolResultSucceeded =
              !toolResultHasError &&
              toolResultData.success !== false &&
              (toolResultData.success === true ||
                toolResultStatus === "completed" ||
                toolResultStatus === "succeeded" ||
                toolResultStatus === "success");
            const toolResultValue =
              toolResultData.result ?? toolResultData.result_preview;
            // Update workingMemory (existing logic)
            setWorkingMemory((prev) => prev ? {
              ...prev,
              tasks: prev.tasks.map((t) => ({
                ...t,
                subTasks: (t.subTasks || []).map((st) =>
                  st.id === toolResultData.tool_call_id
                    ? {
                        ...st,
                        status: toolResultSucceeded ? "completed" : "failed",
                        result: toolResultValue,
                        durationMs: toolResultData.duration_ms,
                      }
                    : st
                ),
              })),
            } : null);
            // Update searchStatus when KB/Web search tool completes
            // Use tool_name from result data directly (more reliable)
            {
              const toolName = toolResultData.tool_name;
              const isKbTool = toolName === "search_knowledge_base" || toolName === "search_kb";
              const isWebTool = toolName === "search_web" || toolName === "web_search";
              if (isKbTool || isWebTool) {
                const statusType = isKbTool ? "kb" : "web";
                setMessages((prev) =>
                  prev.map((m) =>
                    m.id === assistantMessage.id && m.searchStatus
                      ? {
                          ...m,
                          searchStatus: m.searchStatus.map((s) =>
                            s.type === statusType
                              ? {
                                  ...s,
                                  state: toolResultSucceeded ? "completed" : "error",
                                  resultCount: toolResultData.result_count ?? s.resultCount,
                                  durationMs: toolResultData.duration_ms,
                                }
                              : s
                          ),
                        }
                      : m
                  )
                );
              }
            }
            updateAssistantMessage((m) => {
              const prev = m.processSummary ?? initProcessSummary(undefined, now);
              const existing = prev.tools.find((tool) => tool.id === toolResultData.tool_call_id);
              return {
                ...m,
                processSummary: {
                  ...prev,
                  tools: upsertTool(prev.tools, {
                    id: toolResultData.tool_call_id,
                    name: existing?.name || toolResultData.tool_name || "tool",
                    status: toolResultSucceeded ? "completed" : "error",
                    finishedAt: toolResultData.timestamp ?? now,
                    durationMs: toolResultData.duration_ms,
                    summary: summarizeToolResult(toolResultValue),
                    error: toolResultSucceeded
                      ? undefined
                      : summarizeToolResult(toolResultData.error) ??
                        summarizeToolResult(toolResultValue),
                  }),
                },
              };
            });
            break;

          // === Custom File Events ===
          case SSEEventType.FILE_CREATING:
            const fileCreatingData = event.data as {
              step_id?: string;
              filename: string;
              type: string;
              timestamp: number;
            };
            // Show file creation progress
            if (fileCreatingData.step_id) {
              setWorkingMemory((prev) => prev ? {
                ...prev,
                tasks: prev.tasks.map((t) =>
                  t.id === fileCreatingData.step_id
                    ? { ...t, currentFile: fileCreatingData.filename, fileStatus: "creating" }
                    : t
                ),
              } : null);
            }
            break;

          case SSEEventType.FILE_CREATED:
            const fileCreatedData = event.data as {
              step_id?: string;
              filename: string;
              type: string;
              artifact_id?: string;
              download_url?: string;
              timestamp: number;
            };
            if (fileCreatedData.step_id) {
              setWorkingMemory((prev) => prev ? {
                ...prev,
                tasks: prev.tasks.map((t) =>
                  t.id === fileCreatedData.step_id
                    ? {
                        ...t,
                        fileStatus: "completed",
                        createdFile: {
                          filename: fileCreatedData.filename,
                          type: fileCreatedData.type,
                          artifactId: fileCreatedData.artifact_id,
                          downloadUrl: fileCreatedData.download_url,
                        },
                      }
                    : t
                ),
              } : null);
            }
            break;

          // === Custom Search Events ===
          case SSEEventType.SEARCH_STARTED:
            const searchStartData = event.data as {
              step_id?: string;
              search_id: string;
              query: string;
              source: "kb" | "web" | "file";
              timestamp: number;
            };
            if (searchStartData.step_id) {
              setWorkingMemory((prev) => prev ? {
                ...prev,
                tasks: prev.tasks.map((t) =>
                  t.id === searchStartData.step_id
                    ? {
                        ...t,
                        searchStatus: "searching",
                        searchQuery: searchStartData.query,
                        searchSource: searchStartData.source,
                      }
                    : t
                ),
              } : null);
            }
            break;

          case SSEEventType.SEARCH_PROGRESS:
            const searchProgressData = event.data as {
              search_id: string;
              progress: number;
              results_found?: number;
              timestamp: number;
            };
            // Update search progress (if we can find the matching step)
            setWorkingMemory((prev) => prev ? {
              ...prev,
              tasks: prev.tasks.map((t) =>
                t.searchStatus === "searching"
                  ? {
                      ...t,
                      searchProgress: searchProgressData.progress,
                      searchResultsFound: searchProgressData.results_found,
                    }
                  : t
              ),
            } : null);
            break;

          case SSEEventType.SEARCH_COMPLETED:
            const searchCompletedData = event.data as {
              search_id: string;
              results_count: number;
              duration_ms: number;
              timestamp: number;
            };
            setWorkingMemory((prev) => prev ? {
              ...prev,
              tasks: prev.tasks.map((t) =>
                t.searchStatus === "searching"
                  ? {
                      ...t,
                      searchStatus: "completed",
                      searchResultsCount: searchCompletedData.results_count,
                      searchDurationMs: searchCompletedData.duration_ms,
                    }
                  : t
              ),
            } : null);
            break;

          // === Manus-style Slide Outline Event ===
          case SSEEventType.OUTLINE_READY:
            const outlineData = event.data as OutlineReadyEventData;
            // Add slide outline to the current in-progress task (if any)
            // This enables SlideOutlinePreview display in AgentTaskTimeline
            setWorkingMemory((prev) => {
              if (!prev) {
                // Create working memory with outline task
                return {
                  goal: t("assistant.outline.goal", { format: outlineData.format.toUpperCase() }),
                  tasks: [{
                    id: `outline-${Date.now()}`,
                    description: t("assistant.outline.taskCreate", { title: outlineData.outline.title }),
                    status: "completed",
                    icon: outlineData.format === "pptx" ? "ppt" : "doc",
                    slideOutline: outlineData.outline,
                  }],
                  collectedInfo: [],
                  notes: [],
                };
              }
              // Find the current in_progress task and attach the outline
              const hasInProgressTask = prev.tasks.some((t) => t.status === "in_progress");
              if (hasInProgressTask) {
                return {
                  ...prev,
                  tasks: prev.tasks.map((t) =>
                    t.status === "in_progress"
                      ? { ...t, slideOutline: outlineData.outline }
                      : t
                  ),
                };
              }
              // No in-progress task, add a new completed outline task
              return {
                ...prev,
                tasks: [...prev.tasks, {
                  id: `outline-${Date.now()}`,
                  description: t("assistant.outline.taskSummary", { title: outlineData.outline.title }),
                  status: "completed",
                  icon: outlineData.format === "pptx" ? "ppt" : "doc",
                  slideOutline: outlineData.outline,
                }],
              };
            });
            setShowTaskPanel(true);
            break;

          // === Legacy Events ===
          case SSEEventType.PLAN_UPDATE: {
            const planData = (event.data || {}) as Record<string, unknown>;
            const rawPlan = Array.isArray(planData.plan) ? planData.plan : [];
            const planTasks = rawPlan.flatMap((raw, index) => {
              if (!raw || typeof raw !== "object") return [];
              const item = raw as Record<string, unknown>;
              const description =
                typeof item.step === "string" ? item.step.trim() : "";
              if (!description) return [];
              const rawStatus = item.status;
              let status: "pending" | "in_progress" | "completed" = "pending";
              if (rawStatus === "completed") {
                status = "completed";
              } else if (rawStatus === "inProgress" || rawStatus === "in_progress") {
                status = "in_progress";
              }
              const id =
                typeof item.id === "string" && item.id.trim()
                  ? item.id
                  : `plan-${String(planData.item_id ?? "turn")}-${index}`;
              return [{
                id,
                description,
                status,
              }];
            });
            const delta = typeof planData.delta === "string" ? planData.delta.trim() : "";
            const explanation =
              typeof planData.explanation === "string"
                ? planData.explanation
                : undefined;
            if (planTasks.length > 0) {
              setWorkingMemory((prev) => ({
                goal: explanation ?? prev?.goal ?? "",
                tasks: planTasks,
                collectedInfo: prev?.collectedInfo || [],
                notes: prev?.notes || [],
              }));
              setShowTaskPanel(true);
            }
            updateAssistantMessage((m) => {
              const prev = m.processSummary ?? initProcessSummary(undefined, now);
              const steps = planTasks.length > 0
                ? planTasks.reduce(
                    (current, task) => {
                      let status: ProcessStepStatus = "pending";
                      if (task.status === "in_progress") {
                        status = "running";
                      } else if (task.status === "completed") {
                        status = "completed";
                      }
                      return upsertStep(current, {
                        id: task.id,
                        title: sanitizeProgressLabel(task.description) || task.id,
                        status,
                      });
                    },
                    prev.steps,
                  )
                : prev.steps;
              return {
                ...m,
                processSummary: {
                  ...prev,
                  collapsed: false,
                  currentStep: delta || explanation || prev.currentStep,
                  steps,
                },
              };
            });
            break;
          }

          case SSEEventType.TASK_PLANNING:
            const planData = event.data as TaskPlanningEventData;
            setWorkingMemory({
              goal: planData.goal,
              tasks: planData.tasks.map(t => ({ id: t.id, description: t.description, status: "pending" })),
              collectedInfo: [],
              notes: []
            });
            updateAssistantMessage((m) => {
              const prev = m.processSummary ?? initProcessSummary(undefined, now);
              return {
                ...m,
                processSummary: {
                  ...prev,
                  collapsed: false,
                  currentStep: planData.goal || prev.currentStep,
                  steps: planData.tasks.reduce(
                    (steps, task) =>
                      upsertStep(steps, {
                        id: task.id,
                        title: sanitizeProgressLabel(task.description) || task.id,
                        description: task.type,
                        status: "pending",
                      }),
                    prev.steps
                  ),
                },
              };
            });
            setShowTaskPanel(true);
            break;

          case SSEEventType.MEMORY_LOADED:
            updateAssistantMessage((m) => {
              const prev = m.processSummary ?? initProcessSummary(undefined, now);
              return {
                ...m,
                processSummary: {
                  ...prev,
                  collapsed: false,
                  steps: upsertStep(prev.steps, {
                    id: `memory-loaded-${m.id}`,
                    title: t("assistant.activity.memoryLoaded", {
                      defaultValue: "Memory loaded",
                    }),
                    description: t("assistant.activity.memoryLoadedBody", {
                      defaultValue: "Session memory is available to this turn.",
                    }),
                    status: "completed",
                  }),
                },
              };
            });
            break;

          case SSEEventType.WORKING_MEMORY_UPDATE:
            const memData = event.data as WorkingMemoryUpdateEventData;
            setWorkingMemory({
              goal: memData.goal,
              tasks: memData.tasks,
              collectedInfo: memData.collected_info,
              notes: memData.notes
            });
            updateAssistantMessage((m) => {
              const prev = m.processSummary ?? initProcessSummary(undefined, now);
              const inProgressTask = memData.tasks.find((task) => task.status === "in_progress");
              return {
                ...m,
                processSummary: {
                  ...prev,
                  collapsed: false,
                  currentStep:
                    sanitizeProgressLabel(inProgressTask?.description) ||
                    memData.goal ||
                    prev.currentStep,
                  steps: memData.tasks.reduce(
                    (steps, task) =>
                      upsertStep(steps, {
                        id: task.id,
                        title: sanitizeProgressLabel(task.description) || task.id,
                        status: taskStatusToProcessStatus(task.status),
                        error: task.error,
                        description: task.result,
                      }),
                    prev.steps
                  ),
                },
              };
            });
            setShowTaskPanel(true);
            break;

          // Code/Document execution events
          case SSEEventType.CODE_EXECUTION_START:
          case SSEEventType.IMAGE_GENERATION_START:
            if (event.data) {
              const startData = event.data as { execution_id?: string; title?: string; code?: string };
              setCodeExecution({
                isExecuting: true,
                executionId: startData.execution_id ?? null,
                code: startData.code ?? null,
                output: "Processing...\n",
                executionTimeMs: null,
                status: "running",
                outputFiles: [],
              });
              setShowArtifacts(true);
            }
            break;

          case SSEEventType.DOCUMENT_GENERATION_START:
            if (event.data) {
              const startData = event.data as { execution_id?: string; title?: string; code?: string };
              setCodeExecution({
                isExecuting: true,
                executionId: startData.execution_id ?? null,
                code: startData.code ?? null,
                output: "Processing...\n",
                executionTimeMs: null,
                status: "running",
                outputFiles: [],
              });
            }
            break;

          case SSEEventType.CODE_EXECUTION_OUTPUT:
            if (typeof event.data === "string") {
              setCodeExecution((prev) => ({
                ...prev,
                output: prev.output + event.data,
              }));
            }
            break;

          case SSEEventType.CODE_EXECUTION_RESULT:
          case SSEEventType.DOCUMENT_GENERATION_RESULT:
          case SSEEventType.IMAGE_GENERATION_RESULT:
            if (event.data) {
              const resultData = event.data as {
                success?: boolean;
                error?: string;
                output?: string;
                result?: string;
                duration_ms?: number;
                output_files?: Array<{
                  filename: string;
                  content_base64: string;
                  mime_type: string | null;
                  size_bytes: number;
                  artifact_id?: string;
                  download_url?: string;
                }>;
              };
              setCodeExecution((prev) => ({
                ...prev,
                isExecuting: false,
                status: resultData.success ? "success" : "error",
                output: resultData.output || resultData.result || (resultData.error ? `Error: ${resultData.error}` : prev.output),
                executionTimeMs: resultData.duration_ms ?? null,
                outputFiles: resultData.output_files || [],
              }));
            }
            break;

          case SSEEventType.ARTIFACT_CREATED:
            if (event.data) {
              const artifactData = event.data as {
                artifact_id: string;
                type?: string;
                artifact_type?: string;
                format?: string;
                title?: string;
                filename?: string;
                mime_type?: string;
                size_bytes?: number;
                ready?: boolean;
                source?: string;
                download_url?: string;
                download_path?: string;
              };
              if (!artifactData.artifact_id) break;
              const format =
                artifactData.format ||
                artifactData.filename?.match(/\.([a-z0-9]+)$/i)?.[1]?.toLowerCase() ||
                (artifactData.mime_type?.includes("wordprocessingml") ? "docx" : "file");
              const artifactType =
                artifactData.type ||
                artifactData.artifact_type ||
                (["doc", "docx", "md", "pdf", "ppt", "pptx", "txt", "xlsx"].includes(format)
                  ? "document"
                  : "file");
              const artifactTitle = artifactData.title || artifactData.filename || "Artifact";
              // Prefer presigned download_url (no auth required)
              const downloadUrl =
                artifactData.download_url ||
                artifactData.download_path ||
                getArtifactDownloadUrl(artifactData.artifact_id);

              // Add to artifacts panel
              setArtifacts((prev) => {
                if (prev.some((artifact) => artifact.id === artifactData.artifact_id)) {
                  return prev;
                }
                return [
                  ...prev,
                  {
                    id: artifactData.artifact_id,
                    type: artifactType as any,
                    format,
                    title: artifactTitle,
                    url: downloadUrl,
                    filename: artifactData.filename,
                    mimeType: artifactData.mime_type,
                    sizeBytes: artifactData.size_bytes,
                    ready: artifactData.ready ?? (artifactData.size_bytes === undefined || artifactData.size_bytes > 0),
                    source: artifactData.source as any,
                    createdAt: new Date(),
                  },
                ];
              });
              if (
                artifactType === "image" ||
                artifactData.mime_type?.startsWith("image/")
              ) {
                setShowArtifacts(true);
              }

              // Inline success cards only represent downloadable artifacts.
              if (artifactData.ready === false || artifactData.size_bytes === 0) break;
              // Also add to current message's generatedArtifacts for inline display
              const generatedArtifact: GeneratedArtifact = {
                id: artifactData.artifact_id,
                type: artifactType as GeneratedArtifact["type"],
                format,
                title: artifactTitle,
                url: downloadUrl,
                filename: artifactData.filename,
                mimeType: artifactData.mime_type,
                sizeBytes: artifactData.size_bytes,
              };
              setMessages((prev) =>
                prev.map((m) => {
                  if (m.id !== assistantMessage.id) return m;
                  const nextGeneratedArtifacts = [...(m.generatedArtifacts || [])];
                  if (!nextGeneratedArtifacts.some((artifact) => artifact.id === generatedArtifact.id)) {
                    nextGeneratedArtifacts.push(generatedArtifact);
                  }
                  const artifactIds = new Set(m._artifactIds ?? []);
                  artifactIds.add(generatedArtifact.id);
                  return {
                    ...m,
                    _artifactIds: [...artifactIds],
                    generatedArtifacts: nextGeneratedArtifacts,
                  };
                })
              );
            }
            break;

          case SSEEventType.QUIZ_STATUS:
            // Show quiz generation progress as search status
            if (event.data && typeof event.data === "object") {
              const statusMsg = (event.data as Record<string, unknown>).message as string || "";
              updateSearchStatus("kb", { state: "searching", resultCount: undefined });
              setMessages((prev) =>
                prev.map((m) =>
                  m.id === assistantMessage.id
                    ? { ...m, content: statusMsg, isStreaming: true }
                    : m
                )
              );
            }
            break;

          case SSEEventType.QUIZ_READY:
            if (event.data && typeof event.data === "object") {
              const quizPayload = event.data as Record<string, unknown>;
              setMessages((prev) =>
                prev.map((m) =>
                  m.id === assistantMessage.id
                    ? { ...m, quizData: quizPayload as any }
                    : m
                )
              );
            }
            break;

          case SSEEventType.QUIZ_ERROR:
            if (event.data && typeof event.data === "object") {
              const errMsg = (event.data as Record<string, unknown>).message as string || "Quiz generation failed";
              setMessages((prev) =>
                prev.map((m) =>
                  m.id === assistantMessage.id
                    ? { ...m, content: errMsg, isStreaming: false }
                    : m
                )
              );
            }
            break;

          // Note: TASK_PLANNING and WORKING_MEMORY_UPDATE are handled above (lines 552-571)
          // Do not duplicate handlers here

          case "usage":
            if (event.data && typeof event.data === "object") {
              streamTurnState = applyUsageToTurnState(
                streamTurnState,
                event.data as Record<string, unknown>
              );
              syncTurnStateToMessage();
            }
            break;
            
          case "done":
            if (event.data && typeof event.data === "object") {
              streamTurnState = applyUsageToTurnState(
                streamTurnState,
                event.data as Record<string, unknown>
              );
            }
            // `done` closes model transport output only. The canonical AgentLoop
            // can still fail during durable finalization, so only run_finished
            // may mark the turn successful.
            syncTurnStateToMessage();
            break;
            
          case "error":
            const errData = event.data as any;
            const streamErrorMessage =
              errData?.message || "Unknown error";
            settleRunTerminal("failed", now, { error: streamErrorMessage });
            break;
        }
      }

      if (!isCurrentStream()) {
        closeStreamTrace("cancelled", { reason: "session_epoch_changed" });
        return;
      }
      const streamFinishedAtMs = Date.now();
      if (approvalHoldActive() && !cancelRequestedRef.current && !streamAbortController.signal.aborted) {
        updateAssistantMessage((message) => ({
          ...message,
          isStreaming: true,
          status: "streaming",
          processSummary: message.processSummary
            ? {
                ...message.processSummary,
                status: "blocked",
                collapsed: false,
              }
            : message.processSummary,
        }));
        closeStreamTrace("completed", { reason: "awaiting_approval" });
      } else if (cancelRequestedRef.current || streamAbortController.signal.aborted) {
        settleRunTerminal("cancelled", streamFinishedAtMs, { error: "cancelled" });
      } else if (
        streamTurnState.status === "idle" ||
        streamTurnState.status === "streaming"
      ) {
        settleRunTerminal("failed", streamFinishedAtMs, {
          error: "stream_ended_without_terminal",
        });
      }

      // Cancel pending RAF and flush before final update
      if (syncRafId !== null) { cancelAnimationFrame(syncRafId); syncRafId = null; }
      flushTurnStateToMessage();
      activityQueue.flushNow();

      // The append-only Runtime history is authoritative at turn terminal.
      // Reconcile once after success so a transient SSE delivery gap around
      // approval/tool execution cannot leave the final answer or artifact
      // link missing until the user manually reloads the conversation.
      if (terminalLatch.current() === "succeeded" && isCurrentStream()) {
        try {
          // Distinguish "no artifacts" from "the artifact fetch failed".
          // Collapsing both to [] let a transient failure clear a panel that
          // the stream had just correctly populated.
          const [historyResponse, artifactFetch] = await Promise.all([
            getAssistantSessionHistory(sessionId, 200),
            getSessionArtifacts(sessionId).then(
              (items) => ({ ok: true, items }),
              () => ({ ok: false, items: [] as ArtifactInfo[] }),
            ),
          ]);
          const sessionArtifacts = artifactFetch.items;
          if (!isCurrentStream()) return;
          const restoredMessages = historyResponse.messages.map((message, index) =>
            restoreMessageMetadata(message, index, sessionId)
          );
          const restoredAssistant = [...restoredMessages]
            .reverse()
            .find((message) => message.role === "assistant");
          const hydrationBase =
            restoredAssistant ??
            messagesRef.current.find((message) => message.id === assistantMessage.id) ??
            assistantMessage;
          if (restoredAssistant?.content.trim()) {
            streamTurnState = {
              ...streamTurnState,
              content: restoredAssistant.content,
            };
            // The quiz capability moved into the Rust worker, whose compat
            // `tool_call_result` carries only the arguments and which emits no
            // `quiz:ready` event, so the card has no live source. The
            // reconciled history does carry `quiz_id` (recovered from the
            // capability ledger) — hydrate the card from it.
            const restoredQuizId = (restoredAssistant as { _quizId?: string })._quizId;
            if (restoredQuizId) {
              void getQuiz(restoredQuizId)
                .then((quiz) => {
                  if (!isCurrentStream()) return;
                  updateAssistantMessage((message) => ({ ...message, quizData: quiz }));
                })
                .catch(() => undefined);
            }
          }
          const hydrated = hydrateMessageArtifacts([hydrationBase], sessionArtifacts)[0];
          updateAssistantMessage((message) => ({
            ...message,
            generatedArtifacts:
              hydrated?.generatedArtifacts ?? message.generatedArtifacts,
            _artifactIds: hydrated?._artifactIds ?? message._artifactIds,
          }));
          if (artifactFetch.ok) {
            setArtifacts(sessionArtifacts.map(toArtifact));
            setCodeExecution((previous) => ({
              ...previous,
              outputFiles: buildLatestRunOutputFilesFromArtifacts(
                hydrated ? [hydrated] : [hydrationBase],
                sessionArtifacts,
              ),
            }));
          }
        } catch {
          console.warn("Assistant terminal history reconciliation failed");
        }
      }

      if (approvalHoldActive() && !cancelRequestedRef.current && !streamAbortController.signal.aborted) {
        return;
      }

      // Final update
      const finishedAtMs = Date.now();
      streamTurnState = completeStreamTurn(streamTurnState, finishedAtMs);
      content = streamTurnState.content;
      firstTokenMs = streamTurnState.firstTokenMs ?? firstTokenMs;
      durationMs = streamTurnState.durationMs ?? durationMs;
      usage = mergeUsageWithTurnState(usage, streamTurnState);
      const processSummaryStatus =
        streamTurnState.status === "failed"
          ? "failed"
          : streamTurnState.status === "cancelled"
            ? "cancelled"
            : "succeeded";
      setMessages(prev => prev.map(m => m.id === assistantMessage.id ? {
        ...m,
        content,
        parts: buildTextParts(assistantMessage.id, content, createdAt),
        contexts,
        webSearchResults,
        usage,
        durationMs,
        firstTokenMs,
        firstTextTokenMs,
        isStreaming: false,
        isThinkingStreaming: false,
        thinkingContent:
          accumulatedThinkingContent.trim() || m.thinkingContent,
        streamingThinkingContent: undefined,
        status:
          streamTurnState.status === "failed" || streamTurnState.status === "cancelled"
            ? streamTurnState.status
            : "completed",
        toolCalls: mapStreamToolCallsToAssistant(streamTurnState),
        processSummary: finalizeProcessSummary(
          m.processSummary,
          processSummaryStatus,
          finishedAtMs,
          true
        ),
      } : m));
      const finalOutcome =
        streamTurnState.status === "failed"
          ? "failed"
          : streamTurnState.status === "cancelled"
            ? "cancelled"
            : "completed";
      closeStreamTrace(finalOutcome, {
        durationMs,
        firstTokenMs,
        firstTextTokenMs,
        totalTokens:
          typeof usage?.total_tokens === "number" ? usage.total_tokens : undefined,
        toolCalls: streamTurnState.toolCalls.length,
        ...(streamTurnState.error ? { error: streamTurnState.error } : {}),
      });

    } catch (error: any) {
      if (syncRafId !== null) { cancelAnimationFrame(syncRafId); syncRafId = null; }
      flushTurnStateToMessage();
      activityQueue.flushNow();
      if (!isCurrentStream()) {
        closeStreamTrace("cancelled", { reason: "session_epoch_changed" });
        return;
      }
      const userCancelled =
        cancelRequestedRef.current ||
        (error.name === "AbortError" && streamAbortController.signal.aborted === true);
      if (approvalHoldActive() && !userCancelled) {
        updateAssistantMessage((message) => ({
          ...message,
          isStreaming: true,
          status: "streaming",
          processSummary: message.processSummary
            ? {
                ...message.processSummary,
                status: "blocked",
                collapsed: false,
              }
            : message.processSummary,
        }));
        closeStreamTrace("completed", { reason: "awaiting_approval" });
      } else if (!userCancelled) {
        const finishedAtMs = Date.now();
        const sourceRevoked = error.response?.data?.detail?.code === "ASSISTANT_SOURCE_ACCESS_REVOKED";
        const needsNewThread = error.response?.data?.detail?.code ===
          "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_THREAD_RECREATE_REQUIRED";
        const attachmentUnavailable = error.response?.data?.detail?.code === "ATTACHMENT_UNAVAILABLE";
        const startFailureMessage = sourceRevoked
          ? t("assistant.sourceNeedsNewConversation", "Earlier knowledge sources are unavailable. Start a new conversation to continue.")
          : needsNewThread
          ? t("assistant.modelNeedsNewConversation", "This model uses a different tool configuration. Start a new conversation to continue.")
          : attachmentUnavailable
          ? t("assistant.selectedAttachmentUnavailable", "A selected attachment is unavailable. Remove it and upload the file again, then send manually. No run was started.")
          : undefined;
        if (startFailureMessage) {
          setModelRecreateNeeded(needsNewThread);
          setSourceRecreateNeeded(sourceRevoked);
          streamTurnState = { ...streamTurnState, content: startFailureMessage };
        }
        const accepted = settleRunTerminal("failed", finishedAtMs, {
          error: startFailureMessage || error.message || "Unknown error",
          ...(startFailureMessage ? { showInterruptionNotice: false } : {}),
        });
        if (accepted) {
          setMessages(prev => prev.map(m => m.id === assistantMessage.id ? {
            ...m,
            content: streamTurnState.content,
            parts: buildTextParts(
              assistantMessage.id,
              streamTurnState.content,
              createdAt
            ),
            isStreaming: false,
            isThinkingStreaming: false,
            thinkingContent:
              accumulatedThinkingContent.trim() || m.thinkingContent,
            streamingThinkingContent: undefined,
            status: "failed",
            firstTokenMs: streamTurnState.firstTokenMs ?? firstTokenMs,
            firstTextTokenMs,
            toolCalls: mapStreamToolCallsToAssistant(streamTurnState),
            processSummary: finalizeProcessSummary(
              m.processSummary,
              "failed",
              finishedAtMs
            ),
          } : m));
        }
      } else {
        const finishedAtMs = Date.now();
        const accepted = settleRunTerminal("cancelled", finishedAtMs, {
          error: "cancelled",
        });
        if (accepted) {
          const cancelledContent = streamTurnState.content || content || "(Cancelled)";
          setMessages(prev => prev.map(m => m.id === assistantMessage.id ? {
            ...m,
            content: cancelledContent,
            parts: buildTextParts(assistantMessage.id, cancelledContent, createdAt),
            isStreaming: false,
            isThinkingStreaming: false,
            thinkingContent:
              accumulatedThinkingContent.trim() || m.thinkingContent,
            streamingThinkingContent: undefined,
            status: "cancelled",
            firstTokenMs: streamTurnState.firstTokenMs ?? firstTokenMs,
            firstTextTokenMs,
            toolCalls: mapStreamToolCallsToAssistant(streamTurnState),
            processSummary: finalizeProcessSummary(
              m.processSummary,
              "cancelled",
              finishedAtMs,
              true
            ),
          } : m));
        }
      }
      const caughtOutcome = terminalLatch.current();
      if (caughtOutcome === "failed") {
        closeStreamTrace("failed", {
          error: streamTurnState.error || error.message,
        });
      } else if (caughtOutcome === "cancelled") {
        closeStreamTrace("cancelled", { reason: "abort_signal" });
      } else if (caughtOutcome === "succeeded") {
        closeStreamTrace("completed");
      }
    } finally {
      activityQueue.flushNow();
      if (terminalLatch.current() === "cancelled" && isCurrentStream()) {
        await reconcileCancellationReceipt();
      }
      if (isCurrentStream()) {
        refreshSessionsInBackground();
      }
      if (!streamTraceClosed) {
        closeStreamTrace("cancelled", { reason: "finalized_without_outcome" });
      }
      // The consumer is gone by the time ``finally`` runs, so its transport
      // locks must be released. A live stream clears ``approvalHoldOpen`` on
      // APPROVAL_RESULT before reaching here; an active hold therefore means
      // the stream died mid-park. Keep the server-run lock closed so another
      // turn cannot race the pending decision, while releasing the dead
      // AbortController/send task. The pending card and its
      // runtimeThreadId/approvalId remain actionable, and the message id tells
      // the decision handler to reconcile from durable history. Only a
      // confirmed terminal projection releases the server-run lock.
      if (isCurrentStream()) {
        if (approvalHoldActive()) {
          deadApprovalMessageIdsRef.current.add(assistantMessage.id);
          setServerRunBlocking(true);
        }
        setIsStreaming(false);
        clearCancelFallback();
        if (abortControllerRef.current === streamAbortController) {
          abortControllerRef.current = null;
        }
        activeTaskIdRef.current = null;
        cancelApiTaskIdRef.current = null;
        cancelRequestedRef.current = false;
        sendInFlightRef.current = false;
      }
    }
  }, [activeSessionId, clearCancelFallback, messages, requestTaskCancellation, retireArtifactRestore, setActiveSessionId, setArtifacts, setAssistantLocalTitles, setShowArtifacts, t, workingMemory]);

  const reconcileDeadApprovalRun = useCallback(
    async (runId?: string, runtimeThreadId?: string, sessionIdHint?: string | null) => {
      try {
        const result = await loadDeadApprovalReconciliation(
          runId,
          runtimeThreadId,
          sessionIdHint,
        );
        if (!result) {
          console.warn("Approval decision did not reach a terminal run state");
          return;
        }
        if (result.sessionId && activeSessionIdRef.current === result.sessionId) {
          const restored = hydrateMessageArtifacts(
            result.historyMessages.map((message, index) =>
              restoreMessageMetadata(message, index, result.sessionId as string),
            ),
            result.sessionArtifacts,
          );
          setMessages(restored);
          setArtifacts(result.sessionArtifacts.map(toArtifact));
          void hydrateQuizData(restored, setMessages);
          setServerRunBlocking(false);
        }
      } catch {
        console.warn("Reconcile after dead-stream approval decision failed");
      }
    },
    [setArtifacts],
  );

  const handleToolApproval = useCallback(
    async (
      messageId: string,
      toolId: string,
      approvalId: string,
      approved: boolean,
    ) => {
      // Native Runtime V2 keeps the SSE cursor open while waiting for an
      // approval. V1 still guards against a duplicate resume while another
      // send is active, but the V2 decision must be allowed through.
      if (!approvalId) throw new Error("Approval request is unavailable");
      const messageSnapshot = messagesRef.current;
      const targetIndex = messageSnapshot.findIndex((message) => message.id === messageId);
      const target = targetIndex >= 0 ? messageSnapshot[targetIndex] : undefined;
      const runId = target?.processSummary?.runId;
      const runtimeThreadId = target?.processSummary?.runtimeThreadId;
      let resumeMessageContent = "";
      if (targetIndex > 0) {
        for (let index = targetIndex - 1; index >= 0; index -= 1) {
          if (messageSnapshot[index]?.role === "user") {
            resumeMessageContent = messageSnapshot[index].content;
            break;
          }
        }
      }

      if (!runId && !runtimeThreadId) {
        throw new Error("Approval run is unavailable");
      }

      try {
        if (runtimeThreadId) {
          // Native V2 approvals are consumed by the Runtime and wake the
          // existing turn; starting a second turn would duplicate execution.
          const { decideAgentRuntimeApproval } = await import("@/api/agentThreads");
          await decideAgentRuntimeApproval(runtimeThreadId, approvalId, approved);
        } else {
          await approveToolCall(approvalId, { approved });
        }
      } catch (error) {
        // The approval card must refresh authoritative state before offering a
        // new manual decision. A fulfilled callback would falsely latch success.
        console.warn("Assistant tool approval submission failed");
        throw error;
      }
      const wasDeadApproval = deadApprovalMessageIdsRef.current.delete(messageId);
      const runtimeNeedsReconciliation = Boolean(
        runtimeThreadId && (!abortControllerRef.current || wasDeadApproval),
      );
      setServerRunBlocking(runtimeNeedsReconciliation);

      setMessages((prev) => {
        return prev.map((message) => {
          if (message.id !== messageId || !message.processSummary) return message;
          const tools = message.processSummary.tools.map((tool) =>
            tool.id === toolId
              ? {
                  ...tool,
                  status: approved ? ("running" as const) : ("error" as const),
                  summary: approved
                    ? tool.summary
                    : tool.summary || "Approval rejected",
                }
              : tool,
          );
          return {
            ...message,
            processSummary: {
              ...message.processSummary,
              tools,
            },
          };
        });
      });

      if (!approved) {
        // A rejected approval ends the turn; reconcile the dead-stream case so
        // the rejection is reflected instead of leaving a parked "blocked" card.
        if (runtimeNeedsReconciliation) {
          void reconcileDeadApprovalRun(
            runId,
            runtimeThreadId,
            activeSessionIdRef.current,
          );
        }
        return;
      }

      if (runtimeThreadId) {
        // A live V2 stream keeps consuming after the decision and renders the
        // continued turn itself. Only the dead-stream case — flagged by the
        // consumer's ``finally`` — needs a client-side reconcile.
        if (runtimeNeedsReconciliation) {
          void reconcileDeadApprovalRun(
            runId,
            runtimeThreadId,
            activeSessionIdRef.current,
          );
        }
        return;
      }

      // The early guard above guarantees runId when no live runtime thread exists;
      // restate it so the narrowing survives the closure boundary.
      if (!runId) {
        return;
      }

      try {
        const resumePlan = await prepareAssistantRunResume(runId, {
          approval_id: approvalId,
        });
        if (resumePlan.resume.status !== "ready") {
          console.warn("Approval resume not ready", resumePlan.resume.reason);
          setMessages((prev) =>
            prev.map((message) => {
              if (message.id !== messageId || !message.processSummary) return message;
              return {
                ...message,
                processSummary: {
                  ...message.processSummary,
                  tools: message.processSummary.tools.map((tool) =>
                    tool.id === toolId
                      ? {
                          ...tool,
                          status: "error" as const,
                          summary:
                            tool.summary ||
                            resumePlan.resume.reason ||
                            "Resume not ready",
                        }
                      : tool,
                  ),
                },
              };
            }),
          );
          return;
        }
      } catch (error) {
        console.warn("Resume probe after approval failed", error);
        setMessages((prev) =>
          prev.map((message) => {
            if (message.id !== messageId || !message.processSummary) return message;
            return {
              ...message,
              processSummary: {
                ...message.processSummary,
                tools: message.processSummary.tools.map((tool) =>
                  tool.id === toolId
                    ? {
                        ...tool,
                        status: "approval_required" as const,
                        summary: tool.summary || "Resume not ready",
                      }
                    : tool,
                ),
              },
            };
          }),
        );
        return;
      }

      const streamConfig = lastStreamConfigRef.current;
      if (!streamConfig) {
        console.warn("Approval resume skipped: stream config unavailable");
        setMessages((prev) =>
          prev.map((message) => {
            if (message.id !== messageId || !message.processSummary) return message;
            return {
              ...message,
              processSummary: {
                ...message.processSummary,
                tools: message.processSummary.tools.map((tool) =>
                  tool.id === toolId
                    ? {
                        ...tool,
                        status: "approval_required" as const,
                        summary: tool.summary || "Resume not ready",
                      }
                    : tool,
                ),
              },
            };
          }),
        );
        return;
      }

      await sendMessage({
        messageContent: resumeMessageContent || "Continue",
        filePaths: [],
        attachments: [],
        config: streamConfig.config,
        selectedDatasets: streamConfig.selectedDatasets,
        models: streamConfig.models,
        datasets: streamConfig.datasets,
        resumeRunId: runId,
        resumeApprovalId: approvalId,
        targetAssistantMessageId: messageId,
      });
    },
    [sendMessage, reconcileDeadApprovalRun],
  );

  return {
    sessions,
    setSessions,
    activeSessionId,
    setActiveSessionId,
    activeSessionConfig: lastStreamConfigRef.current?.config,
    messages,
    setMessages,
    isStreaming,
    isRecoveringImage,
    hasActiveRun: isStreaming || Boolean(observedRun),
    modelRecreateNeeded,
    sourceRecreateNeeded,
    isComposerBlocked: isStreaming || serverRunBlocking || isRecoveringImage,
    sessionsLoading,
    historyRestoreState,
    historyRestoreError,
    handleNewChat,
    handleSelectSession,
    handleDeleteSession,
    sendMessage,
    stopStreaming,
    handleToolApproval,
    // Artifacts & Agent
    artifacts,
    artifactLoadError,
    refreshArtifacts,
    artifactRefreshStatus,
    canRefreshArtifacts,
    setArtifacts,
    showArtifacts,
    setShowArtifacts,
    workingMemory,
    showTaskPanel,
    codeExecution,
    setCodeExecution
  };
}
