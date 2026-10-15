import {
  getAssistantRunStatus,
  getSessionArtifacts,
  getSessionHistory,
  type ArtifactInfo,
  type AssistantMessage,
} from "@/api/assistant";
import { getAgentRuntimeV2RunSnapshot } from "@/api/agentThreads";

const TERMINAL_RUN_STATUSES = new Set(["succeeded", "failed", "cancelled"]);

export interface DeadApprovalReconciliation {
  sessionId?: string;
  historyMessages: AssistantMessage[];
  sessionArtifacts: ArtifactInfo[];
}

/** Consume durable Runtime events, then load the terminal session projection. */
export async function loadDeadApprovalReconciliation(
  runId?: string,
  runtimeThreadId?: string,
  sessionIdHint?: string | null,
): Promise<DeadApprovalReconciliation | null> {
  let sessionId = sessionIdHint || undefined;
  if (!runId) return null;
  let terminalConfirmed = false;
  for (let attempt = 0; attempt < 40; attempt += 1) {
    // Finalizing the Gateway run ledger is coupled to consuming the durable
    // per-turn cursor. A restored approval no longer has a live consumer.
    if (runtimeThreadId) {
      await getAgentRuntimeV2RunSnapshot(runtimeThreadId, runId);
    }
    const { run } = await getAssistantRunStatus(runId);
    const reportedSessionId = String(run.session_id || "").trim();
    sessionId = reportedSessionId || sessionId;
    if (TERMINAL_RUN_STATUSES.has(run.status || "")) {
      terminalConfirmed = true;
      break;
    }
    await new Promise((resolve) => setTimeout(resolve, 1500));
  }
  if (!terminalConfirmed) return null;
  if (!sessionId) {
    return { sessionId, historyMessages: [], sessionArtifacts: [] };
  }
  const [history, sessionArtifacts] = await Promise.all([
    getSessionHistory(sessionId, 200),
    getSessionArtifacts(sessionId).catch(() => []),
  ]);
  return { sessionId, historyMessages: history.messages, sessionArtifacts };
}
