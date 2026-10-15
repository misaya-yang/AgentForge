import type { AgentRuntimeSession } from "@/types/agents";
import type { ArtifactInfo, SessionHistoryMessage } from "@/api/assistant";

export interface PreviewLocator {
  session_id: string;
  agent_id: string;
  agent_version_id: string | null;
  draft_revision: number | null;
  channel: "preview";
}

export function previewLocatorKey(agentId: string): string {
  return `agent-studio-preview:${agentId}`;
}

export function readPreviewLocator(raw: string | null, agentId: string): PreviewLocator | null {
  if (!raw) return null;
  try {
    const value: unknown = JSON.parse(raw);
    if (!value || typeof value !== "object") return null;
    const session = value as Record<string, unknown>;
    const versionId = session.agent_version_id;
    const revision = session.draft_revision;
    if (
      session.agent_id !== agentId || session.channel !== "preview"
      || typeof session.session_id !== "string" || !session.session_id
      || !(versionId === null || (typeof versionId === "string" && versionId))
      || !(revision === null || (Number.isInteger(revision) && Number(revision) > 0))
      || (versionId === null) === (revision === null)
    ) return null;
    return session as unknown as PreviewLocator;
  } catch {
    return null;
  }
}

export function previewLocator(session: AgentRuntimeSession): PreviewLocator {
  return {
    session_id: session.session_id,
    agent_id: session.agent_id,
    agent_version_id: session.agent_version_id,
    draft_revision: session.draft_revision,
    channel: "preview",
  };
}

export function latestPreviewRunId(messages: SessionHistoryMessage[]): string | null {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const runId = messages[index].metadata?.runtime_run_id;
    if (typeof runId === "string" && runId) return runId;
  }
  return null;
}

export function previewHistoryMessages(messages: SessionHistoryMessage[]): Array<{
  id: string;
  role: "user" | "assistant";
  content: string;
}> {
  return messages.filter((message) => message.role === "user" || message.role === "assistant")
    .map((message, index) => ({
      id: `restored:${index}`,
      role: message.role as "user" | "assistant",
      content: message.content,
    }));
}

export function previewRunIsTerminal(status: string): boolean {
  return ["succeeded", "completed", "failed", "cancelled", "timeout"].includes(status);
}

export function previewArtifactCanDownload(
  artifact: Pick<ArtifactInfo, "ready" | "size_bytes" | "source">,
): boolean {
  return artifact.source !== "user" && artifact.ready === true && artifact.size_bytes > 0;
}

export async function downloadPreviewArtifact(
  artifact: Pick<ArtifactInfo, "artifact_id" | "filename" | "title" | "ready" | "size_bytes" | "source">,
  artifactUrl: (artifactId: string) => string,
  authenticatedDownload: (url: string, filename: string) => Promise<void>,
): Promise<void> {
  if (!previewArtifactCanDownload(artifact)) throw new Error("Artifact is not ready");
  await authenticatedDownload(artifactUrl(artifact.artifact_id), artifact.filename || artifact.title);
}

export function previewSessionActionsBlocked(state: {
  restoring: boolean;
  starting: boolean;
  sending: boolean;
  runUnsettled: boolean;
  decidingApproval: boolean;
  cancelling: boolean;
  refreshingRun: boolean;
}): boolean {
  return Object.values(state).some(Boolean);
}

export function previewCanStartCurrentDraft(state: {
  target: string;
  pinnedDraftRevision: number | null;
  currentDraftRevision: number;
  restoring: boolean;
  starting: boolean;
  sending: boolean;
  decidingApproval: boolean;
  cancelling: boolean;
  refreshingRun: boolean;
  approvalPending: boolean;
}): boolean {
  return state.pinnedDraftRevision !== null
    && state.pinnedDraftRevision < state.currentDraftRevision
    && state.target === `draft:${state.pinnedDraftRevision}`
    && !state.restoring
    && !state.starting
    && !state.sending
    && !state.decidingApproval
    && !state.cancelling
    && !state.refreshingRun
    && !state.approvalPending;
}

export type PreviewPollResult<TApproval> =
  | { kind: "terminal"; runId: string; status: string; messages: SessionHistoryMessage[] }
  | { kind: "approval"; runId: string; approval: TApproval }
  | { kind: "pending" | "unavailable"; runId: string | null };

export async function pollPreviewOutcome<TApproval>(options: {
  runId: string | null;
  readHistory: () => Promise<SessionHistoryMessage[]>;
  readRun: (runId: string) => Promise<{ status: string; approval: TApproval | null }>;
  isActive: () => boolean;
  wait: () => Promise<void>;
  onRunId: (runId: string) => void;
  attempts: number;
}): Promise<PreviewPollResult<TApproval>> {
  let runId = options.runId;
  let unavailable = false;
  for (let attempt = 0; attempt < options.attempts && options.isActive(); attempt += 1) {
    try {
      if (!runId) {
        runId = latestPreviewRunId(await options.readHistory());
        if (runId && options.isActive()) options.onRunId(runId);
      }
      if (runId && options.isActive()) {
        const result = await options.readRun(runId);
        if (!options.isActive()) break;
        if (result.approval) return { kind: "approval", runId, approval: result.approval };
        if (previewRunIsTerminal(result.status)) {
          const messages = await options.readHistory();
          if (!options.isActive()) break;
          return { kind: "terminal", runId, status: result.status, messages };
        }
      }
      unavailable = false;
    } catch {
      unavailable = true;
    }
    if (attempt + 1 < options.attempts && options.isActive()) await options.wait();
  }
  return { kind: unavailable ? "unavailable" : "pending", runId };
}
