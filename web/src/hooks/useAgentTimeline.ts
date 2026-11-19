/**
 * useAgentTimeline - Hook for managing agent timeline state from AG-UI events
 *
 * This hook parses AG-UI protocol SSE events and maintains a real-time
 * timeline state that can be rendered by the AgentTimeline component.
 *
 * Usage:
 *   const { state, processEvent, reset } = useAgentTimeline();
 *
 *   // Process incoming SSE events
 *   eventSource.onmessage = (event) => {
 *     processEvent(JSON.parse(event.data));
 *   };
 *
 *   // Render timeline
 *   <AgentTimeline state={state} />
 */

import { useCallback, useReducer } from "react";
import i18n from "@/i18n";
import type {
  TimelineState,
  TimelineStep,
  TimelineArtifact,
  ToolCallInfo,
  StepType,
} from "@/components/agent/AgentTimeline";
import type { AGUIEvent, AGUIEventType } from "@/lib/sse";

// Re-export types for consumers
export type { AGUIEvent, AGUIEventType };

/** Timeline reducer action types */
type TimelineAction =
  | { type: "RUN_STARTED"; payload: AGUIEvent }
  | { type: "RUN_FINISHED"; payload: AGUIEvent }
  | { type: "RUN_ERROR"; payload: AGUIEvent }
  | { type: "STEP_STARTED"; payload: AGUIEvent }
  | { type: "STEP_FINISHED"; payload: AGUIEvent }
  | { type: "TOOL_CALL_START"; payload: AGUIEvent }
  | { type: "TOOL_CALL_RESULT"; payload: AGUIEvent }
  | { type: "TOOL_CALL_END"; payload: AGUIEvent }
  | { type: "ARTIFACT_CREATED"; payload: AGUIEvent }
  | { type: "FILE_CREATED"; payload: AGUIEvent }
  | { type: "STATUS_UPDATE"; payload: AGUIEvent }
  | { type: "SEARCH_STARTED"; payload: AGUIEvent }
  | { type: "SEARCH_COMPLETED"; payload: AGUIEvent }
  | { type: "CODE_EXECUTION_START"; payload: AGUIEvent }
  | { type: "CODE_EXECUTION_RESULT"; payload: AGUIEvent }
  | { type: "IMAGE_GENERATION_START"; payload: AGUIEvent }
  | { type: "IMAGE_GENERATION_RESULT"; payload: AGUIEvent }
  | { type: "DOCUMENT_GENERATION_START"; payload: AGUIEvent }
  | { type: "DOCUMENT_GENERATION_RESULT"; payload: AGUIEvent }
  | { type: "OUTLINE_READY"; payload: AGUIEvent }
  // Phase 1 Optimization: Phase tracking
  | { type: "PHASE_STARTED"; payload: AGUIEvent }
  | { type: "PHASE_COMPLETED"; payload: AGUIEvent }
  | { type: "CANCELLED"; payload: AGUIEvent }
  | { type: "RESET" };

// ============================================================================
// Initial State
// ============================================================================

const initialState: TimelineState = {
  runId: "",
  status: "idle",
  steps: [],
  currentStepId: undefined,
  startTime: undefined,
  endTime: undefined,
  metadata: undefined,
  // Phase 1 Optimization: Phase progress
  phaseProgress: undefined,
};

// ============================================================================
// Helper Functions
// ============================================================================

function mapStepType(type: string): StepType {
  const typeMap: Record<string, StepType> = {
    planning: "planning",
    analysis: "analysis",
    outline: "outline",
    generating: "generating",
    validating: "validating",
    repairing: "repairing",
    tool_call: "tool_call",
    search: "search",
    code_execution: "code_execution",
    image_generation: "image_generation",
    document_generation: "document_generation",
    complete: "complete",
  };
  return typeMap[type] || "planning";
}

/** Safely convert AG-UI timestamp to milliseconds */
function toMs(timestamp: number | undefined): number {
  if (!timestamp) return Date.now();
  // If timestamp looks like seconds (< year 2100 in seconds), convert to ms
  // Otherwise assume it's already in milliseconds
  return timestamp < 4102444800 ? timestamp * 1000 : timestamp;
}

function updateStepStatus(
  steps: TimelineStep[],
  stepId: string,
  updates: Partial<TimelineStep>
): TimelineStep[] {
  return steps.map((step) => {
    if (step.id === stepId) {
      return { ...step, ...updates };
    }
    if (step.children) {
      return {
        ...step,
        children: updateStepStatus(step.children, stepId, updates),
      };
    }
    return step;
  });
}

function addToolCallToStep(
  steps: TimelineStep[],
  stepId: string,
  toolCall: ToolCallInfo
): TimelineStep[] {
  return steps.map((step) => {
    if (step.id === stepId) {
      return {
        ...step,
        toolCalls: [...(step.toolCalls || []), toolCall],
      };
    }
    if (step.children) {
      return {
        ...step,
        children: addToolCallToStep(step.children, stepId, toolCall),
      };
    }
    return step;
  });
}

function updateToolCallInStep(
  steps: TimelineStep[],
  stepId: string,
  toolCallId: string,
  updates: Partial<ToolCallInfo>
): TimelineStep[] {
  return steps.map((step) => {
    if (step.id === stepId && step.toolCalls) {
      return {
        ...step,
        toolCalls: step.toolCalls.map((tc) =>
          tc.id === toolCallId ? { ...tc, ...updates } : tc
        ),
      };
    }
    if (step.children) {
      return {
        ...step,
        children: updateToolCallInStep(step.children, stepId, toolCallId, updates),
      };
    }
    return step;
  });
}

function addArtifactToStep(
  steps: TimelineStep[],
  stepId: string,
  artifact: TimelineArtifact
): TimelineStep[] {
  return steps.map((step) => {
    if (step.id === stepId) {
      return {
        ...step,
        artifacts: [...(step.artifacts || []), artifact],
      };
    }
    if (step.children) {
      return {
        ...step,
        children: addArtifactToStep(step.children, stepId, artifact),
      };
    }
    return step;
  });
}

// ============================================================================
// Reducer
// ============================================================================

function timelineReducer(
  state: TimelineState,
  action: TimelineAction
): TimelineState {
  switch (action.type) {
    case "RUN_STARTED": {
      const { run_id, thread_id, metadata, timestamp } = action.payload;
      return {
        ...initialState,
        runId: run_id as string,
        status: "running",
        startTime: toMs(timestamp),
        metadata: {
          ...(metadata as Record<string, unknown>),
          threadId: thread_id,
        },
      };
    }

    case "RUN_FINISHED": {
      const { timestamp, metadata } = action.payload;
      return {
        ...state,
        status: "completed",
        endTime: toMs(timestamp),
        currentStepId: undefined,
        metadata: {
          ...state.metadata,
          ...(metadata as Record<string, unknown>),
        },
      };
    }

    case "RUN_ERROR": {
      const { timestamp, metadata } = action.payload;
      return {
        ...state,
        status: "error",
        endTime: toMs(timestamp),
        currentStepId: undefined,
        metadata: {
          ...state.metadata,
          ...(metadata as Record<string, unknown>),
        },
      };
    }

    case "STEP_STARTED": {
      const { step_id, step_name, step_type, parent_step_id, metadata, timestamp } =
        action.payload;
      const newStep: TimelineStep = {
        id: step_id as string,
        type: mapStepType(step_type as string),
        name: step_name as string,
        status: "running",
        startTime: toMs(timestamp),
        metadata: metadata as Record<string, unknown>,
      };

      // If there's a parent step, add as child
      if (parent_step_id) {
        return {
          ...state,
          currentStepId: step_id as string,
          steps: state.steps.map((step) => {
            if (step.id === parent_step_id) {
              return {
                ...step,
                children: [...(step.children || []), newStep],
              };
            }
            return step;
          }),
        };
      }

      return {
        ...state,
        currentStepId: step_id as string,
        steps: [...state.steps, newStep],
      };
    }

    case "STEP_FINISHED": {
      const { step_id, timestamp, metadata } = action.payload;
      const stepId = (step_id as string) || state.currentStepId;
      if (!stepId) return state;

      return {
        ...state,
        steps: updateStepStatus(state.steps, stepId, {
          status: "completed",
          endTime: toMs(timestamp),
          metadata: {
            ...(state.steps.find((s) => s.id === stepId)?.metadata || {}),
            ...(metadata as Record<string, unknown>),
          },
        }),
      };
    }

    case "TOOL_CALL_START": {
      const { tool_call_id, tool_name, timestamp } = action.payload;
      const toolCall: ToolCallInfo = {
        id: tool_call_id as string,
        name: tool_name as string,
        status: "running",
      };

      // Add to current step
      const stepId = state.currentStepId;
      if (!stepId) {
        // Create a tool call step
        const newStep: TimelineStep = {
          id: `step-tc-${tool_call_id}`,
          type: "tool_call",
          name: tool_name as string,
          status: "running",
          startTime: toMs(timestamp),
          toolCalls: [toolCall],
        };
        return {
          ...state,
          currentStepId: newStep.id,
          steps: [...state.steps, newStep],
        };
      }

      return {
        ...state,
        steps: addToolCallToStep(state.steps, stepId, toolCall),
      };
    }

    case "TOOL_CALL_RESULT":
    case "TOOL_CALL_END": {
      const { tool_call_id, result, error, status } = action.payload;
      const stepId = state.currentStepId || `step-tc-${tool_call_id}`;

      return {
        ...state,
        steps: updateToolCallInStep(state.steps, stepId, tool_call_id as string, {
          result: result ? JSON.stringify(result) : undefined,
          status: (error ? "error" : (status as ToolCallInfo["status"]) || "completed"),
          durationMs: undefined, // Would need start time tracking
        }),
      };
    }

    case "ARTIFACT_CREATED":
    case "FILE_CREATED": {
      const { artifact_id, artifact_type, name, file_name, file_id, url, mime_type, size } =
        action.payload;
      const artifact: TimelineArtifact = {
        id: (artifact_id || file_id) as string,
        type: (artifact_type as TimelineArtifact["type"]) || "file",
        name: (name || file_name) as string,
        url: url as string,
        mimeType: mime_type as string,
        size: size as number,
      };

      const stepId = state.currentStepId;
      if (!stepId) {
        return state;
      }

      return {
        ...state,
        steps: addArtifactToStep(state.steps, stepId, artifact),
      };
    }

    case "STATUS_UPDATE": {
      const { status, message, phase, progress } = action.payload;
      const stepId = state.currentStepId;
      if (!stepId) return state;

      return {
        ...state,
        steps: updateStepStatus(state.steps, stepId, {
          message: message as string,
          progress: progress as number,
          metadata: {
            ...(state.steps.find((s) => s.id === stepId)?.metadata || {}),
            phase,
            status,
          },
        }),
      };
    }

    case "SEARCH_STARTED": {
      const { query, search_type, timestamp } = action.payload;
      const newStep: TimelineStep = {
        id: `search-${Date.now()}`,
        type: "search",
        name: i18n.t("agent.status.searchQuery", { query: (query as string).slice(0, 30) }),
        status: "running",
        startTime: toMs(timestamp),
        message: query as string,
        metadata: { searchType: search_type },
      };
      return {
        ...state,
        currentStepId: newStep.id,
        steps: [...state.steps, newStep],
      };
    }

    case "SEARCH_COMPLETED": {
      const { result_count, timestamp } = action.payload;
      const stepId = state.currentStepId;
      if (!stepId) return state;

      return {
        ...state,
        steps: updateStepStatus(state.steps, stepId, {
          status: "completed",
          endTime: toMs(timestamp),
          message: i18n.t("agent.status.searchResults", { count: result_count }),
        }),
      };
    }

    case "CODE_EXECUTION_START": {
      const { language, timestamp } = action.payload;
      const newStep: TimelineStep = {
        id: `code-${Date.now()}`,
        type: "code_execution",
        name: i18n.t("agent.status.codeExecution", { language: language || "Python" }),
        status: "running",
        startTime: toMs(timestamp),
        metadata: { language },
      };
      return {
        ...state,
        currentStepId: newStep.id,
        steps: [...state.steps, newStep],
      };
    }

    case "CODE_EXECUTION_RESULT": {
      const { output, success, error, timestamp } = action.payload;
      const stepId = state.currentStepId;
      if (!stepId) return state;

      return {
        ...state,
        steps: updateStepStatus(state.steps, stepId, {
          status: success ? "completed" : "error",
          endTime: toMs(timestamp),
          message: error as string || (output as string)?.slice(0, 100),
        }),
      };
    }

    case "IMAGE_GENERATION_START": {
      const { prompt, timestamp } = action.payload;
      const newStep: TimelineStep = {
        id: `img-${Date.now()}`,
        type: "image_generation",
        name: i18n.t("agent.status.generateImage"),
        status: "running",
        startTime: toMs(timestamp),
        message: (prompt as string)?.slice(0, 50),
      };
      return {
        ...state,
        currentStepId: newStep.id,
        steps: [...state.steps, newStep],
      };
    }

    case "IMAGE_GENERATION_RESULT": {
      const { url, prompt, timestamp } = action.payload;
      const stepId = state.currentStepId;
      if (!stepId) return state;

      const artifact: TimelineArtifact = {
        id: `img-artifact-${Date.now()}`,
        type: "image",
        name: (prompt as string)?.slice(0, 20) || i18n.t("agent.artifact.generatedImage"),
        url: url as string,
      };

      return {
        ...state,
        steps: addArtifactToStep(
          updateStepStatus(state.steps, stepId, {
            status: "completed",
            endTime: toMs(timestamp),
          }),
          stepId,
          artifact
        ),
      };
    }

    case "DOCUMENT_GENERATION_START": {
      const { doc_type, title, timestamp } = action.payload;
      const newStep: TimelineStep = {
        id: `doc-${Date.now()}`,
        type: "document_generation",
        name: i18n.t("agent.status.generateDocument", { type: (doc_type as string)?.toUpperCase() }),
        status: "running",
        startTime: toMs(timestamp),
        message: title as string,
      };
      return {
        ...state,
        currentStepId: newStep.id,
        steps: [...state.steps, newStep],
      };
    }

    case "DOCUMENT_GENERATION_RESULT": {
      const { doc_type, title, url, file_id, timestamp } = action.payload;
      const stepId = state.currentStepId;
      if (!stepId) return state;

      const artifact: TimelineArtifact = {
        id: (file_id as string) || `doc-artifact-${Date.now()}`,
        type: "document",
        name: (title as string) || i18n.t("agent.artifact.document"),
        url: url as string,
        mimeType: (doc_type as string) === "docx"
          ? "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
          : undefined,
      };

      return {
        ...state,
        steps: addArtifactToStep(
          updateStepStatus(state.steps, stepId, {
            status: "completed",
            endTime: toMs(timestamp),
          }),
          stepId,
          artifact
        ),
      };
    }

    case "OUTLINE_READY": {
      const { title, sections } = action.payload;
      const stepId = state.currentStepId;
      if (!stepId) return state;

      return {
        ...state,
        steps: updateStepStatus(state.steps, stepId, {
          message: i18n.t("agent.status.outlineReady", {
            title: title || i18n.t("agent.status.unknown"),
            count: (sections as string[])?.length || 0,
          }),
          metadata: {
            ...(state.steps.find((s) => s.id === stepId)?.metadata || {}),
            outline: { title, sections },
          },
        }),
      };
    }

    // Phase 1 Optimization: Phase tracking events
    case "PHASE_STARTED": {
      const { phase_index, total_phases, phase_name, display_name } = action.payload;
      return {
        ...state,
        phaseProgress: {
          currentPhase: phase_index as number,
          totalPhases: total_phases as number,
          phaseName: phase_name as string,
          displayName: display_name as string,
          status: "running",
        },
      };
    }

    case "PHASE_COMPLETED": {
      const { duration_ms } = action.payload;
      if (!state.phaseProgress) return state;
      return {
        ...state,
        phaseProgress: {
          ...state.phaseProgress,
          status: "completed",
          durationMs: duration_ms as number,
        },
      };
    }

    case "CANCELLED": {
      return {
        ...state,
        status: "cancelled" as TimelineState["status"],
        endTime: Date.now(),
      };
    }

    case "RESET":
      return initialState;

    default:
      return state;
  }
}

// ============================================================================
// Hook
// ============================================================================

export function useAgentTimeline() {
  const [state, dispatch] = useReducer(timelineReducer, initialState);

  const processEvent = useCallback((event: AGUIEvent) => {
    const eventType = event.event;

    switch (eventType) {
      case "run_started":
        dispatch({ type: "RUN_STARTED", payload: event });
        break;
      case "run_finished":
        dispatch({ type: "RUN_FINISHED", payload: event });
        break;
      case "run_error":
        dispatch({ type: "RUN_ERROR", payload: event });
        break;
      case "step_started":
        dispatch({ type: "STEP_STARTED", payload: event });
        break;
      case "step_finished":
        dispatch({ type: "STEP_FINISHED", payload: event });
        break;
      case "tool_call_start":
        dispatch({ type: "TOOL_CALL_START", payload: event });
        break;
      case "tool_call_result":
        dispatch({ type: "TOOL_CALL_RESULT", payload: event });
        break;
      case "tool_call_end":
        dispatch({ type: "TOOL_CALL_END", payload: event });
        break;
      case "artifact_created":
        dispatch({ type: "ARTIFACT_CREATED", payload: event });
        break;
      case "file_created":
        dispatch({ type: "FILE_CREATED", payload: event });
        break;
      case "status":
        dispatch({ type: "STATUS_UPDATE", payload: event });
        break;
      case "search_started":
        dispatch({ type: "SEARCH_STARTED", payload: event });
        break;
      case "search_completed":
        dispatch({ type: "SEARCH_COMPLETED", payload: event });
        break;
      case "code_execution_start":
        dispatch({ type: "CODE_EXECUTION_START", payload: event });
        break;
      case "code_execution_result":
        dispatch({ type: "CODE_EXECUTION_RESULT", payload: event });
        break;
      case "image_generation_start":
        dispatch({ type: "IMAGE_GENERATION_START", payload: event });
        break;
      case "image_generation_result":
        dispatch({ type: "IMAGE_GENERATION_RESULT", payload: event });
        break;
      case "document_generation_start":
        dispatch({ type: "DOCUMENT_GENERATION_START", payload: event });
        break;
      case "document_generation_result":
        dispatch({ type: "DOCUMENT_GENERATION_RESULT", payload: event });
        break;
      case "outline_ready":
        dispatch({ type: "OUTLINE_READY", payload: event });
        break;
      // Phase 1 Optimization: Phase tracking events
      case "phase_started":
        dispatch({ type: "PHASE_STARTED", payload: event });
        break;
      case "phase_completed":
        dispatch({ type: "PHASE_COMPLETED", payload: event });
        break;
      case "cancelled":
        dispatch({ type: "CANCELLED", payload: event });
        break;
      // Ignore text events - they don't affect timeline
      case "text_message_start":
      case "text_message_content":
      case "text_message_end":
      case "text_delta":
      case "tool_call_args":
      case "stream_end":
      case "state_snapshot":
      case "state_delta":
      case "messages_snapshot":
      case "custom_event":
      case "raw_event":
        // These don't affect the timeline state
        break;
      default:
        console.debug(`Unhandled AG-UI event: ${eventType}`);
    }
  }, []);

  const reset = useCallback(() => {
    dispatch({ type: "RESET" });
  }, []);

  return {
    state,
    processEvent,
    reset,
  };
}

export default useAgentTimeline;
