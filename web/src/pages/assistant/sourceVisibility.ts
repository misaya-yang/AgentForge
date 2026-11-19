import type { ChatMessage } from "./types";

export function redactRestrictedSourceMessages(messages: ChatMessage[], restrictedRuns: Set<string>): ChatMessage[] {
  return messages.map((message) => {
    const runId = message.processSummary?.runId ?? message.runtimeRunId;
    return runId && restrictedRuns.has(runId) ? redactSourceMessage(message) : message;
  });
}

/** Keep task identity and decisions while removing unavailable source content. */
export function redactSourceMessage(message: ChatMessage): ChatMessage {
  if (message.role !== "assistant") return message;
  const summary = message.processSummary;
  const redacted: ChatMessage & { _quizId?: string } = {
    ...message, sourceAccessRevoked: true, content: "", parts: [], meta: undefined,
    thinkingContent: "", streamingThinkingContent: "", contexts: [],
    ragCitations: [], webSearchResults: [], toolCalls: [], toolResults: [],
    quizData: undefined, quizResult: undefined, generatedArtifacts: [],
    _artifactIds: [], citations: [], ragEvaluation: undefined,
    searchStatus: [], activeSubAgents: [], agentPhase: undefined,
    attachments: [], imageGenerationPrompt: undefined,
    processSummary: summary ? {
      collapsed: summary.collapsed, runId: summary.runId,
      runtimeThreadId: summary.runtimeThreadId, status: summary.status,
      startedAt: summary.startedAt, totalDurationMs: summary.totalDurationMs,
      terminalReason: summary.terminalReason, steps: [],
      tools: summary.tools.map((tool) => ({
        id: tool.id, name: tool.name, status: tool.status, approvalId: tool.approvalId,
        startedAt: tool.startedAt, finishedAt: tool.finishedAt, durationMs: tool.durationMs,
      })),
    } : undefined,
  };
  delete redacted._quizId;
  return redacted;
}
