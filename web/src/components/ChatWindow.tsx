import type { ReportedRouting } from "@/features/chat/stream/routingObservation";
import type { TextTokenEstimate, TokenProvenance } from "@/features/chat/stream/tokenObservation";
import type { SdkRunReference } from "@/features/chat/stream/sdkRun";
import type { SdkToolActivity } from "@/features/chat/stream/sdkLifecycle";
/**
 * ChatWindow - thin composition wrapper.
 *
 * All rendering logic lives in `@/components/chat/*`.
 * This file re-exports the public types and composes the message list.
 */
import { AnimatePresence } from "motion/react";
import { useTranslation } from "react-i18next";
import type { TimelineState } from "@/components/agent/AgentTimeline";
import type { ArtifactData } from "@/components/agent/ArtifactCard";
import type { ToolCall } from "@/types/gateway";
import { ChatMessageItem } from "@/components/chat/ChatMessageItem";

// ---------------------------------------------------------------------------
// Public types (kept here so existing imports don't break)
// ---------------------------------------------------------------------------

export interface ToolCallWithResult {
  toolCall: ToolCall;
  result?: string;
  argsText?: string;
  argsValid?: boolean;
}

export type ChatMessage = {
  id?: string;
  role: "user" | "assistant";
  content: string;
  createdAt?: string;
  parts?: Array<{ id: string; type: "text" | "tool_call" | "tool_result"; content: string; createdAt: string }>;
  status?: "idle" | "streaming" | "completed" | "cancelled" | "failed";
  /** Confirmed local terminal observation, not durable provider outcome. */
  completionConfirmed?: boolean;
  meta?: Record<string, unknown>;
  toolCalls?: ToolCallWithResult[];
  sdkToolActivity?: SdkToolActivity[];
  sdkAuthorityScope?: string;
  sdkRun?: SdkRunReference;
  isThinking?: boolean;
  isStreaming?: boolean;
  stats?: {
    reportedRouting?: ReportedRouting;
    durationMs?: number;
    inputTokens?: number;
    outputTokens?: number;
    totalTokens?: number;
    firstTokenMs?: number;
    tokenUsageSource?: "reported" | "unknown";
    textTokenEstimate?: TextTokenEstimate;
    tokenProvenance?: TokenProvenance;
  };
  /** AG-UI Protocol: Agent execution timeline */
  timeline?: TimelineState;
  /** AG-UI Protocol: Generated artifacts (documents, images, etc.) */
  artifacts?: ArtifactData[];
};

// ---------------------------------------------------------------------------
// ChatWindow component
// ---------------------------------------------------------------------------

export interface ChatWindowProps {
  messages: ChatMessage[];
  showToolCalls?: boolean;
  toolCallsMode?: "full" | "collapsed" | "hidden";
  toolCallsDefaultOpen?: boolean;
  /** Show AG-UI timeline in assistant messages (default: true) */
  showTimeline?: boolean;
  showThinkingIndicator?: boolean;
  onShare?: () => void;
  onRegenerate?: () => void;
}

export function ChatWindow({
  messages,
  showToolCalls = true,
  toolCallsMode = "full",
  toolCallsDefaultOpen = true,
  showTimeline = true,
  showThinkingIndicator = true,
  onShare,
  onRegenerate,
}: ChatWindowProps) {
  const { t } = useTranslation();
  return (
    <div
      className="mx-auto w-full max-w-4xl space-y-8 px-4 py-10"
      role="log"
      aria-live="polite"
      aria-relevant="additions text"
      aria-label={t("playground.chatLog", "Conversation log")}
    >
      <AnimatePresence mode="popLayout">
        {messages.map((message, i) => (
          <ChatMessageItem
            key={message.id || `${message.role}-${i}`}
            message={message}
            showToolCalls={showToolCalls}
            toolCallsMode={toolCallsMode}
            toolCallsDefaultOpen={toolCallsDefaultOpen}
            showTimeline={showTimeline}
            showThinkingIndicator={showThinkingIndicator}
            index={i}
            onShare={onShare}
            onRegenerate={onRegenerate}
          />
        ))}
      </AnimatePresence>
    </div>
  );
}
