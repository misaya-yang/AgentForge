import { motion } from "framer-motion";
import { lazy, memo, Suspense, useState, forwardRef } from "react";
import { useTranslation } from "react-i18next";
import { cn, getSafePlaygroundAssistantContent } from "@/lib/utils";
import type { ChatMessage } from "@/components/ChatWindow";
import { MessageAvatar } from "./MessageAvatar";
import { StatsBadge } from "./StatsBadge";
import { TimelineSection } from "./TimelineSection";
import { ArtifactsSection } from "./ArtifactsSection";
import { WorkflowBeat } from "./WorkflowBeat";
import { Copy, Share2, RefreshCw, Check } from "lucide-react";
import { messageContainmentStyle } from "@/features/chat/messageRenderPerformance";

const SdkRunControls = lazy(async () => {
  const module = await import("./SdkRunControls");
  return { default: module.SdkRunControls };
});

const SdkToolActivity = lazy(async () => {
  const module = await import("./SdkToolActivity");
  return { default: module.SdkToolActivity };
});

const StreamOutput = lazy(async () => {
  const module = await import("@/components/StreamOutput");
  return { default: module.StreamOutput };
});

/**
 * Remove internal "Process (brief)" execution traces from assistant output.
 */
function stripProcessSectionForDisplay(text: string): string {
  if (!text || typeof text !== "string") return text;

  const processBlockRe =
    /(?:^|\n)(?:\s*---\s*\n)?\s*Process\s*\(\s*brief\s*\)\s*:[\s\S]*?Actions\s*&\s*observations\s*:\s*(?:\n+\s*[•\-*]?\s*Action:[^\n]*)+/gi;

  const withoutProcess = text.replace(processBlockRe, "\n");
  const removedProcess = withoutProcess !== text;

  let stripped = withoutProcess;
  if (removedProcess) {
    stripped = stripped.replace(/(?:^|\n)\s*---\s*(?=\n|$)/g, "\n");
  }
  stripped = stripped.replace(/\n{3,}/g, "\n\n").trim();

  return stripped;
}

export interface ChatMessageItemProps {
  message: ChatMessage;
  showToolCalls: boolean;
  toolCallsMode?: "full" | "collapsed" | "hidden";
  toolCallsDefaultOpen?: boolean;
  showTimeline?: boolean;
  showThinkingIndicator?: boolean;
  index: number;
  onShare?: () => void;
  onRegenerate?: () => void;
}

export const ChatMessageItem = memo(
  forwardRef<HTMLDivElement, ChatMessageItemProps>(
    function ChatMessageItem(
      {
        message,
        showToolCalls,
        toolCallsMode = "full",
        showTimeline = true,
        index,
        onShare,
        onRegenerate,
      },
      ref
    ) {
      const { t } = useTranslation();
      const isUser = message.role === "user";
      const hasToolCalls = (message.toolCalls?.length ?? 0) > 0;
      const hasTimeline =
        showTimeline &&
        !isUser &&
        message.timeline &&
        message.timeline.steps.length > 0;
      const hasArtifacts =
        !isUser && message.artifacts && message.artifacts.length > 0;
      const assistantDisplayContent = isUser
        ? message.content
        : getSafePlaygroundAssistantContent(
            stripProcessSectionForDisplay(message.content || ""),
            t
          );
      const hasVisibleAssistantText =
        assistantDisplayContent.trim().length > 0;

      const isStreaming = !!message.isStreaming;
      const toolCallsVisible =
        showToolCalls && toolCallsMode !== "hidden" && !isUser;
      // Workflow beat is shown when this is an assistant turn that has either
      // tool activity or is actively thinking with no text yet. We skip it
      // when the AG-UI timeline is present — that panel owns the workflow view.
      const showWorkflowBeat =
        !isUser &&
        !hasTimeline &&
        !message.sdkToolActivity?.length &&
        toolCallsVisible &&
        (hasToolCalls || (isStreaming && !hasVisibleAssistantText));

      const [isTimelineExpanded, setIsTimelineExpanded] = useState(
        message.timeline?.status === "running"
      );

      return (
        <motion.div
          ref={ref}
          data-message-role={message.role}
          initial={{ opacity: 0, y: 16 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{
            duration: 0.35,
            ease: "easeOut",
            delay: index * 0.04,
          }}
          style={messageContainmentStyle(isStreaming)}
          className={cn(
            "flex w-full gap-4",
            isUser ? "flex-row-reverse" : "flex-row"
          )}
        >
          {/* Avatar */}
          <MessageAvatar isUser={isUser} />

          {/* Content Container */}
          <div
            className={cn(
              "flex flex-col gap-1.5 min-w-0",
              isUser
                ? "w-fit max-w-[70%] items-end"
                : "w-full max-w-[85%] min-w-[280px] sm:min-w-[360px] items-start"
            )}
          >
            {/* Name Label */}
            <span
              className={cn(
                "text-xs font-medium px-1",
                isUser
                  ? "text-emerald-600 dark:text-emerald-400"
                  : "text-blue-600 dark:text-blue-400"
              )}
            >
              {isUser
                ? t("playground.you", "You")
                : t("playground.assistant", "AI Assistant")}
            </span>

            {/* Message Bubble */}
            <div
              data-message-surface={isUser ? "user" : "assistant"}
              role={
                !isUser && message.status === "failed"
                  ? "alert"
                  : undefined
              }
              aria-live={
                !isUser && message.status === "failed"
                  ? "assertive"
                  : undefined
              }
              className={cn(
                "relative min-w-0 transition-all duration-200",
                isUser
                  ? [
                      "px-4 py-2.5",
                      "bg-linear-to-br from-emerald-500 via-emerald-500 to-teal-600",
                      "text-white text-[15px] leading-relaxed",
                      "rounded-2xl rounded-tr-sm",
                      "shadow-md shadow-emerald-500/20",
                    ]
                  : [
                      "w-full",
                      "space-y-3",
                      message.status === "failed"
                        ? "rounded-2xl rounded-tl-sm border border-rose-500/25 bg-rose-500/8 px-4 py-3"
                        : "px-1 py-1",
                    ]
              )}
            >
              {/* AG-UI Timeline Section */}
              {hasTimeline && message.timeline && (
                <div data-message-supplemental="timeline">
                  <TimelineSection
                    timeline={message.timeline}
                    isExpanded={isTimelineExpanded}
                    onToggle={() =>
                      setIsTimelineExpanded(!isTimelineExpanded)
                    }
                  />
                </div>
              )}

              {/* Workflow beat — one collapsible summary row for the whole
                  tool-call cluster in this turn. Replaces the old expanded
                  thinking panel + separate running status bar. */}
              {showWorkflowBeat && (
                <WorkflowBeat
                  toolCalls={message.toolCalls ?? []}
                  isRunning={isStreaming}
                  hasNoVisibleText={!hasVisibleAssistantText}
                />
              )}

              {!isUser && !!message.sdkToolActivity?.length && <Suspense fallback={null}><SdkToolActivity
                items={message.sdkToolActivity} active={isStreaming} authorityScope={message.sdkAuthorityScope} /></Suspense>}

              {!isUser && message.sdkRun && <Suspense fallback={null}><SdkRunControls
                reference={message.sdkRun} authorityScope={message.sdkAuthorityScope}
                interrupted={!isStreaming && (message.status === "failed" || message.status === "cancelled")} /></Suspense>}

              {/* Message Content */}
              {(isUser
                ? message.content
                : hasVisibleAssistantText) ? (
                <div
                  className={cn(
                    "leading-[1.75]",
                    isUser
                      ? "text-white text-[15px]"
                      : "max-w-none px-1 text-[15px] text-slate-800 dark:text-zinc-100"
                  )}
                  data-message-text={
                    isUser ? undefined : "true"
                  }
                >
                  {isUser ? (
                    <div className="whitespace-pre-wrap">
                      {message.content}
                    </div>
                  ) : (
                    <Suspense fallback={<div className="whitespace-pre-wrap">{assistantDisplayContent}</div>}>
                      <StreamOutput
                        text={assistantDisplayContent}
                        isStreaming={isStreaming}
                      />
                    </Suspense>
                  )}
                </div>
              ) : null}

              {!isUser && message.status === "completed" && message.completionConfirmed &&
                !isStreaming && !message.isThinking && !message.content.trim() &&
                !hasToolCalls && !message.sdkToolActivity?.length &&
                !message.timeline?.steps.length && !message.artifacts?.length && (
                <p role="status" data-message-outcome="empty"
                  className="px-1 text-sm text-slate-500 dark:text-zinc-400">
                  {t("playground.emptyResponse", "Completed with no text to display.")}
                </p>
              )}

              {/* Artifacts Section */}
              {hasArtifacts && message.artifacts && (
                <div data-message-supplemental="artifacts">
                  <ArtifactsSection
                    artifacts={message.artifacts}
                  />
                </div>
              )}
            </div>

            {/* Stats (assistant messages only, after content loaded) */}
            {!isUser &&
              message.stats &&
              !message.isThinking &&
              (message.content || message.stats.tokenUsageSource !== undefined) && (
                <StatsBadge stats={message.stats} />
              )}

            {/* Action buttons (assistant messages, after completion) */}
            {!isUser && !isStreaming && hasVisibleAssistantText && (
              <MessageActions
                content={assistantDisplayContent}
                onShare={onShare}
                onRegenerate={onRegenerate}
              />
            )}
          </div>
        </motion.div>
      );
    }
  )
);

ChatMessageItem.displayName = "ChatMessageItem";

// ---------------------------------------------------------------------------
// Action buttons: Copy, Share, Regenerate
// ---------------------------------------------------------------------------

function MessageActions({
  content,
  onShare,
  onRegenerate,
}: {
  content: string;
  onShare?: () => void;
  onRegenerate?: () => void;
}) {
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(content);
    } catch {
      // Fallback for HTTP (no clipboard API)
      const textarea = document.createElement("textarea");
      textarea.value = content;
      document.body.appendChild(textarea);
      textarea.select();
      document.execCommand("copy");
      document.body.removeChild(textarea);
    }
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <div className="flex items-center gap-1 mt-1">
      {/* Copy */}
      <button
        type="button"
        onClick={handleCopy}
        className="p-1.5 rounded-md text-slate-400 hover:text-slate-600 hover:bg-slate-100 dark:hover:text-slate-300 dark:hover:bg-white/6 transition-colors"
        title="Copy"
      >
        {copied ? (
          <Check className="h-3.5 w-3.5 text-emerald-500" />
        ) : (
          <Copy className="h-3.5 w-3.5" />
        )}
      </button>

      {/* Share */}
      {onShare && (
        <button
          type="button"
          onClick={onShare}
          className="p-1.5 rounded-md text-slate-400 hover:text-slate-600 hover:bg-slate-100 dark:hover:text-slate-300 dark:hover:bg-white/6 transition-colors"
          title="Share conversation"
        >
          <Share2 className="h-3.5 w-3.5" />
        </button>
      )}

      {/* Regenerate */}
      {onRegenerate && (
        <button
          type="button"
          onClick={onRegenerate}
          className="p-1.5 rounded-md text-slate-400 hover:text-slate-600 hover:bg-slate-100 dark:hover:text-slate-300 dark:hover:bg-white/6 transition-colors"
          title="Regenerate response"
        >
          <RefreshCw className="h-3.5 w-3.5" />
        </button>
      )}
    </div>
  );
}
