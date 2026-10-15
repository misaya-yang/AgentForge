/**
 * Chat Message Component
 *
 * Renders a single message in the chat. Reasoning, tool calls, search status
 * and the thinking stream are consolidated into a single Claude.ai-style
 * "Activity" chip that opens a side panel (ActivityPanel) — replacing the
 * previous stacked ProcessSummaryBar / ToolCallsDisplay / ThinkingPanel /
 * SearchStatusDisplay / WebSearchDisplay / Thought Process block.
 */

import { lazy, memo, Suspense, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { motion } from "framer-motion";
import { liveThinkingLabel } from "../thinkingPreview";
import {
  Clock,
  MessageSquare,
  FileText,
  Image as ImageIcon,
  Zap,
  Brain,
  PenTool,
  Cog,
  Eye,
  ListTodo,
  CheckCircle2,
  Download,
  ExternalLink,
  Network,
  Copy,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { ContextDisplay } from "./ContextDisplay";
import { CitationDisplay } from "./CitationDisplay";
import type { ChatMessage as ChatMessageType, AgentPhaseStatus } from "../types";
import { QuizCard } from "./Quiz";
import { ActivityPill } from "./ActivityPill";
import { useRightPanel } from "./rightPanelContext";
import { messageContainmentStyle } from "@/features/chat/messageRenderPerformance";
import { assistantOutcome } from "../assistantOutcome";
import { hasTerminalActivity } from "./activityTerminalState";
import { artifactAccessErrorKey, downloadAssistantArtifact, openAssistantArtifact } from "@/lib/authenticatedDownload";
import { toast } from "@/hooks/use-toast";

interface ChatMessageProps {
  message: ChatMessageType;
  sessionId?: string | null;
  onToolApproval?: (
    messageId: string,
    toolId: string,
    approvalId: string,
    approved: boolean,
  ) => void | Promise<void>;
}

const ASSISTANT_UI_V2 = import.meta.env.VITE_ASSISTANT_UI_V2 !== "false";
const ToolApprovalCard = lazy(() => import("./ToolApprovalCard"));
const StreamOutput = lazy(async () => {
  const module = await import("@/components/StreamOutput");
  return { default: module.StreamOutput };
});
const DocumentPreview = lazy(async () => {
  const module = await import("./DocumentPreview");
  return { default: module.DocumentPreview };
});

const ImageTaskReceipt = lazy(() => import("./ImageTaskReceipt"));

function InlineArtifactCard({
  artifact,
}: {
  artifact: NonNullable<ChatMessageType["generatedArtifacts"]>[number];
}) {
  const { t } = useTranslation();
  const hasUrl = Boolean(artifact.url);
  const format = (artifact.format || "file").toUpperCase();
  const title = artifact.title || artifact.filename || "Artifact";
  const meta = [format, artifact.sizeBytes ? `${Math.round(artifact.sizeBytes / 1024)} KB` : null]
    .filter(Boolean)
    .join(" · ");

  // Only show "Open in new tab" for browser-previewable formats
  const previewable = /^(png|jpg|jpeg|gif|webp|svg|pdf|md|txt|html|json|csv)$/i.test(artifact.format || "");

  return (
    <div className="rounded-xl border border-[hsl(var(--assistant-border-soft))] bg-[hsl(var(--assistant-chip-bg))]/60 p-3">
      <div className="flex items-center gap-2">
        <FileText className="h-4 w-4 text-[hsl(var(--assistant-accent))]" />
        <div className="min-w-0 flex-1">
          <p className="text-sm font-medium text-[hsl(var(--assistant-text-primary))] truncate">{title}</p>
          <p className="text-[11px] text-[hsl(var(--assistant-text-secondary))]">{meta}</p>
        </div>
        {hasUrl && (
          <>
            {previewable && (
              <button
                type="button"
                onClick={() => void openAssistantArtifact(artifact.url).catch((error) => toast.error(t("artifact.errors.previewTitle"), t(artifactAccessErrorKey(error, "preview"))))}
                className="inline-flex items-center gap-1 rounded-md border border-[hsl(var(--assistant-border-soft))] px-2 py-1 text-[11px] text-[hsl(var(--assistant-text-secondary))] hover:text-[hsl(var(--assistant-text-primary))]"
              >
                <ExternalLink className="h-3 w-3" />
                {t("common.openInNewTab", "新标签页打开")}
              </button>
            )}
            <button
              type="button"
              onClick={() => void downloadAssistantArtifact(artifact.url, artifact.filename || title).catch((error) => toast.error(t("artifact.errors.downloadTitle"), t(artifactAccessErrorKey(error))))}
              className="inline-flex items-center gap-1 rounded-md border border-[hsl(var(--assistant-border-soft))] px-2 py-1 text-[11px] text-[hsl(var(--assistant-text-secondary))] hover:text-[hsl(var(--assistant-text-primary))]"
            >
              <Download className="h-3 w-3" />
              {t("artifact.download", "下载")}
            </button>
          </>
        )}
      </div>
    </div>
  );
}

/** Agent phase display - Manus style compact indicator */
function AgentPhaseDisplay({ phase }: { phase: AgentPhaseStatus }) {
  const { t } = useTranslation();

  const phaseConfig: Record<
    string,
    { icon: React.ReactNode; color: string; bgColor: string; label: string }
  > = {
    analyzing: {
      icon: <Brain className="h-3 w-3" />,
      color: "text-blue-600 dark:text-blue-400",
      bgColor: "bg-blue-100 dark:bg-blue-900/40",
      label: t("assistant.phase.analyzing"),
    },
    thinking: {
      icon: <Brain className="h-3 w-3" />,
      color: "text-violet-600 dark:text-violet-400",
      bgColor: "bg-violet-100 dark:bg-violet-900/40",
      label: t("assistant.phase.thinking"),
    },
    planning: {
      icon: <ListTodo className="h-3 w-3" />,
      color: "text-amber-600 dark:text-amber-400",
      bgColor: "bg-amber-100 dark:bg-amber-900/40",
      label: t("assistant.phase.planning"),
    },
    executing: {
      icon: <Cog className="h-3 w-3" />,
      color: "text-emerald-600 dark:text-emerald-400",
      bgColor: "bg-emerald-100 dark:bg-emerald-900/40",
      label: t("assistant.phase.executing"),
    },
    observing: {
      icon: <Eye className="h-3 w-3" />,
      color: "text-cyan-600 dark:text-cyan-400",
      bgColor: "bg-cyan-100 dark:bg-cyan-900/40",
      label: t("assistant.phase.observing"),
    },
    writing: {
      icon: <PenTool className="h-3 w-3" />,
      color: "text-pink-600 dark:text-pink-400",
      bgColor: "bg-pink-100 dark:bg-pink-900/40",
      label: t("assistant.phase.writing"),
    },
    completing: {
      icon: <CheckCircle2 className="h-3 w-3" />,
      color: "text-emerald-600 dark:text-emerald-400",
      bgColor: "bg-emerald-100 dark:bg-emerald-900/40",
      label: t("assistant.phase.completing"),
    },
  };

  const config = phaseConfig[phase.phase] || phaseConfig.thinking;

  return (
    <motion.div
      initial={{ opacity: 0, y: 4 }}
      animate={{ opacity: 1, y: 0 }}
      exit={{ opacity: 0, y: -4 }}
      className="flex items-center gap-2 mt-2"
    >
      <div className={cn("flex items-center justify-center w-5 h-5 rounded-md", config.bgColor)}>
        <motion.div
          animate={phase.phase === "executing" ? { rotate: 360 } : {}}
          transition={{ duration: 2, repeat: Infinity, ease: "linear" }}
          className={config.color}
        >
          {config.icon}
        </motion.div>
      </div>
      <span className={cn("text-xs font-medium", config.color)}>
        {phase.message || config.label}
      </span>
      <div className="flex gap-0.5">
        {[0, 1, 2].map((i) => (
          <motion.span
            key={i}
            className={cn("w-1 h-1 rounded-full", config.color.replace("text-", "bg-"))}
            animate={{ opacity: [0.3, 1, 0.3] }}
            transition={{ duration: 1, repeat: Infinity, delay: i * 0.15 }}
          />
        ))}
      </div>
    </motion.div>
  );
}

/** Stats badge — single hairline text line of muted metadata.
 *
 * Replaces the old 3-4 colored pill stack ("18.49s", "TTFT: 5491ms",
 * "1.2K tokens", "cached") which was loud and feels dated. Metadata
 * belongs in the margin, not the spotlight — per project design brief
 * ("composure at rest, drama on change"). Duration already appears in
 * the Activity pill; this line keeps the secondary numbers accessible
 * without competing for attention.
 */
function StatsBadge({ message }: { message: ChatMessageType }) {
  const { t } = useTranslation();
  if (!message.usage && !message.durationMs) return null;

  const totalTokens =
    (message.usage?.input_tokens || 0) + (message.usage?.output_tokens || 0);

  const parts: string[] = [];
  if (message.durationMs != null) {
    parts.push(`${(message.durationMs / 1000).toFixed(2)}s`);
  }
  if (message.firstTokenMs != null) {
    parts.push(`${t("playground.stats.ttft", "TTFT")} ${(message.firstTokenMs / 1000).toFixed(2)}s`);
  }
  if (
    message.firstTextTokenMs != null &&
    message.firstTextTokenMs !== message.firstTokenMs
  ) {
    parts.push(
      `${t("playground.stats.firstText", "text")} ${(message.firstTextTokenMs / 1000).toFixed(2)}s`
    );
  }
  if (totalTokens > 0) {
    const compact =
      totalTokens >= 1000 ? `${(totalTokens / 1000).toFixed(1)}k` : String(totalTokens);
    parts.push(`${compact} ${t("assistant.tokens", "tokens")}`);
  }
  if (message.cacheMetrics && message.cacheMetrics.cache_hit_rate > 0) {
    parts.push(`${Math.round(message.cacheMetrics.cache_hit_rate * 100)}% cached`);
  }

  if (parts.length === 0) return null;

  return (
    <motion.div
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      transition={{ delay: 0.2 }}
      className="mt-2 text-[11px] font-mono tabular-nums text-[hsl(var(--assistant-text-tertiary))]"
    >
      {parts.join(" · ")}
    </motion.div>
  );
}

// Silence unused-import warnings now that pills are gone.
void Clock;
void Zap;
void MessageSquare;

/** Attachments display in message */
function AttachmentsDisplay({
  attachments,
  useV2,
}: {
  attachments: ChatMessageType["attachments"];
  useV2: boolean;
}) {
  if (!attachments || attachments.length === 0) return null;

  return (
    <div className="flex flex-wrap gap-2 mb-3">
      {attachments.map((att, idx) => (
        <div
          key={idx}
          className={
            useV2
              ? "flex items-center gap-1.5 px-2.5 py-1.5 rounded-lg text-xs border border-[hsl(var(--assistant-border-soft))] bg-[hsl(var(--assistant-chip-bg))]/80 text-[hsl(var(--assistant-text-secondary))]"
              : "flex items-center gap-1.5 px-2.5 py-1.5 rounded-lg bg-white/20 backdrop-blur-xs text-xs text-white/90 border border-white/20"
          }
        >
          {att.type === "image" ? (
            <ImageIcon className="h-3.5 w-3.5" />
          ) : (
            <FileText className="h-3.5 w-3.5" />
          )}
          <span className="truncate max-w-[100px]">
            {att.filename || "Attachment"}
          </span>
        </div>
      ))}
    </div>
  );
}

/** Compact image generation placeholder */
function ImageGeneratingPlaceholder({ prompt }: { prompt?: string }) {
  const { t } = useTranslation();

  return (
    <div className="space-y-2">
      <motion.div
        initial={{ opacity: 0, scale: 0.98 }}
        animate={{ opacity: 1, scale: 1 }}
        className="relative w-[200px] h-[200px] rounded-xl overflow-hidden bg-linear-to-br from-slate-100 to-slate-200 dark:from-slate-800 dark:to-slate-700"
      >
        <motion.div
          className="absolute inset-0 bg-linear-to-r from-transparent via-white/20 to-transparent dark:via-white/5"
          animate={{ x: ["-100%", "100%"] }}
          transition={{ duration: 1.5, repeat: Infinity, ease: "linear" }}
        />
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-3">
          <motion.div
            animate={{ scale: [1, 1.05, 1] }}
            transition={{ duration: 2, repeat: Infinity, ease: "easeInOut" }}
            className="p-3 rounded-lg bg-pink-500/10"
          >
            <ImageIcon className="h-8 w-8 text-pink-500" />
          </motion.div>
          <div className="flex items-center gap-1.5">
            <span className="text-xs font-medium text-slate-500 dark:text-slate-400">
              {t("assistant.creatingImage")}
            </span>
            <div className="flex gap-0.5">
              {[0, 1, 2].map((i) => (
                <motion.div
                  key={i}
                  className="w-1 h-1 rounded-full bg-pink-500"
                  animate={{ opacity: [0.3, 1, 0.3] }}
                  transition={{ duration: 1, repeat: Infinity, delay: i * 0.15 }}
                />
              ))}
            </div>
          </div>
        </div>
      </motion.div>
      {prompt && (
        <p className="text-[11px] text-slate-400 dark:text-slate-500 truncate max-w-[200px]">
          "{prompt.length > 40 ? prompt.slice(0, 40) + "..." : prompt}"
        </p>
      )}
    </div>
  );
}

function formatDurationLabel(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return "";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(seconds >= 10 ? 0 : 1)}s`;
  const m = Math.floor(seconds / 60);
  const s = Math.round(seconds % 60);
  return `${m}m ${s}s`;
}

export const ChatMessage = memo(function ChatMessage({
  message,
  sessionId,
  onToolApproval,
}: ChatMessageProps) {
  const { t } = useTranslation();
  const isUser = message.role === "user";
  const runActive = Boolean(message.isStreaming || message.isGeneratingImage || message.processSummary?.status === "running");
  const { openActivity, openSubagents } = useRightPanel();
  const outcome = isUser ? undefined : assistantOutcome(message);
  const [diagnosticCopied, setDiagnosticCopied] = useState(false);

  // Live-elapsed ticker for the pill subtitle while streaming. Kept local
  // to the pill so the panel doesn't re-render every 500ms.
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!runActive) return;
    const id = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(id);
  }, [runActive]);

  const timelineStepCount = isUser
    ? 0
    : (message.processSummary?.steps.length || 0) +
      (message.processSummary?.tools.length || 0) +
      (message.activeSubAgents?.length || 0);
  const totalDurationMs = message.processSummary?.totalDurationMs || message.durationMs || 0;
  const standaloneArtifacts = (message.generatedArtifacts || []).filter((artifact) => {
    const url = artifact.url?.trim();
    return !url || !message.content.includes(url);
  });

  // A turn paused on a tool approval deliberately keeps `isStreaming` true so
  // the SSE turn stays open, but everything it produced before the pause is
  // already final. Gating that output on `!isStreaming` alone hid artifacts
  // and citations for the whole time the operator was deciding.
  const awaitingApproval = message.processSummary?.status === "blocked";
  const outputSettled = !runActive || awaitingApproval;

  // Always surface the activity entry point on assistant turns. Earlier we
  // gated on `timelineSteps.length > 0 || isStreaming`, but messages reloaded
  // from history often have empty toolCalls/thinkingContent (the streaming
  // deltas don't round-trip through persistence). When `hasActivity` went
  // false the pill disappeared and users lost access to the drawer entirely.
  // Broad gate + graceful empty state in the drawer is the safer default.
  const hasActivity = !isUser;

  // Show a "thinking" 3-dot placeholder only if nothing else (no activity, no content)
  // has landed yet — i.e. we literally have no signal to show.
  const showTypingDots =
    !isUser &&
    runActive &&
    !message.content &&
    timelineStepCount === 0;

  return (
    <motion.div
      initial={{ opacity: 0, y: 8 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: 0.3, ease: [0.25, 0.1, 0.25, 1] }}
      style={messageContainmentStyle(runActive)}
      className={cn("group w-full", isUser ? "flex justify-end" : "")}
    >
      {isUser ? (
        <div className="max-w-[75%]">
          <AttachmentsDisplay attachments={message.attachments} useV2={ASSISTANT_UI_V2} />
          <div
            className={cn(
              "rounded-3xl px-5 py-3",
              ASSISTANT_UI_V2
                ? "bg-[hsl(var(--assistant-user-bubble))] text-[hsl(var(--assistant-text-primary))]"
                : "bg-primary text-white shadow-xs shadow-primary/15",
            )}
          >
            <div className="whitespace-pre-wrap leading-relaxed text-[15px]">
              {message.content}
            </div>
          </div>
        </div>
      ) : (
        /* Assistant message — clean, no bubble, full width */
        <div className="w-full space-y-3 assistant-copy pl-1">
          {/* ActivityPill — inline affordance that opens the right-side
              Activity drawer. The drawer itself is mounted at page level
              (see AssistantPage) as a sibling to ArtifactsPanel, with a
              mutex enforced via RightPanelContext. */}
          {hasActivity && (() => {
            const createdMs = message.createdAt
              ? new Date(message.createdAt).getTime()
              : undefined;
            const liveElapsed =
              runActive && createdMs ? Math.max(0, now - createdMs) : 0;
            const effectiveMs = runActive
              ? Math.max(totalDurationMs, liveElapsed)
              : totalDurationMs;
            const durationLabel = formatDurationLabel(effectiveMs);
            const thinkingLabel = t("playground.activity.thinking", {
              defaultValue: "Thinking",
            });
            const label = runActive
              ? liveThinkingLabel(message.streamingThinkingContent, thinkingLabel)
              : t("playground.activity.title", { defaultValue: "Activity" });
            return (
              <ActivityPill
                steps={timelineStepCount}
                durationLabel={durationLabel}
                running={runActive}
                onOpen={() => openActivity(message.id)}
                variant="pill"
                label={label}
              />
            );
          })()}

          {!isUser && !hasTerminalActivity(message) && onToolApproval && (
            <Suspense fallback={null}>
              {(message.processSummary?.tools ?? [])
                .filter((tool) => tool.status === "approval_required" && tool.approvalId)
                .map((tool) => (
                  <ToolApprovalCard
                    key={tool.id}
                    tool={tool}
                    sourceAccessRevoked={message.sourceAccessRevoked}
                    runtimeThreadId={message.processSummary?.runtimeThreadId}
                    onApprove={() =>
                      onToolApproval(message.id, tool.id, tool.approvalId as string, true)
                    }
                    onReject={() =>
                      onToolApproval(message.id, tool.id, tool.approvalId as string, false)
                    }
                  />
                ))}
            </Suspense>
          )}

          {/* Typing dots: only if we have no signal at all yet */}
          {showTypingDots && (
            <motion.div
              initial={{ opacity: 0 }}
              animate={{ opacity: 1 }}
              className="flex items-center gap-2.5 py-2"
            >
              <div className="flex gap-1">
                {[0, 1, 2].map((i) => (
                  <motion.div
                    key={i}
                    className="w-1.5 h-1.5 rounded-full bg-[hsl(var(--assistant-accent))]"
                    animate={{ opacity: [0.3, 1, 0.3] }}
                    transition={{
                      duration: 1.4,
                      repeat: Infinity,
                      delay: i * 0.2,
                      ease: "easeInOut",
                    }}
                  />
                ))}
              </div>
            </motion.div>
          )}

          {/* Context display — still shown inline (not noise; it's citation source detail) */}
          {message.contexts && message.contexts.length > 0 && (
            <ContextDisplay contexts={message.contexts} />
          )}

          {/* Compact launcher; child details live in the shared right-side lane. */}
          {message.activeSubAgents && message.activeSubAgents.length > 0 && (
            <button
              type="button"
              onClick={() => openSubagents(message.id)}
              className="mb-3 inline-flex items-center gap-2 rounded-full border border-[hsl(var(--assistant-border-soft))] bg-[hsl(var(--assistant-surface-soft))] px-3 py-1.5 text-[12px] text-[hsl(var(--assistant-text-secondary))] transition-colors hover:text-[hsl(var(--assistant-text-primary))] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[hsl(var(--assistant-accent))]"
              aria-label={`Open ${message.activeSubAgents.length} sub-agents`}
            >
              <Network className="h-3.5 w-3.5 text-[hsl(var(--assistant-accent))]" aria-hidden="true" />
              <span>{message.activeSubAgents.length} sub-agents</span>
              <span className="font-mono text-[10px] tabular-nums text-[hsl(var(--assistant-text-tertiary))]">
                {message.activeSubAgents.filter((agent) => agent.status === "running").length} active
              </span>
            </button>
          )}

          {/* Message content */}
          {message.isGeneratingImage ? (
            <ImageGeneratingPlaceholder prompt={message.imageGenerationPrompt} />
          ) : message.content.trim() ? (
            <div className="text-[hsl(var(--assistant-text-primary))] text-[15px] leading-[1.75]">
              <Suspense fallback={<div className="whitespace-pre-wrap">{message.content}</div>}>
                <StreamOutput
                  text={message.content}
                  isStreaming={runActive}
                  artifacts={message.generatedArtifacts}
                  artifactIds={message.sourceAccessRevoked ? undefined : message._artifactIds}
                />
              </Suspense>
            </div>
          ) : null}

          {message.sourceAccessRevoked && (
            <div role="status" className="rounded-lg border border-amber-300/60 bg-amber-50/70 px-3 py-2 text-sm text-amber-950 dark:border-amber-700/60 dark:bg-amber-950/30 dark:text-amber-100">
              <p className="font-medium">{t("assistant.outcome.source_revoked.title")}</p>
              <p className="mt-1 text-xs">{t("assistant.outcome.source_revoked.next")}</p>
            </div>
          )}
          {outcome && (
            <div
              role="status"
              aria-live="polite"
              className={cn(
                "rounded-lg border px-3 py-2 text-sm",
                outcome.kind === "failed" || outcome.kind === "unknown" || outcome.kind === "restart_interrupted" || outcome.kind === "source_revoked"
                  ? "border-amber-300/60 bg-amber-50/70 text-amber-950 dark:border-amber-700/60 dark:bg-amber-950/30 dark:text-amber-100"
                  : "border-[hsl(var(--assistant-border-soft))] bg-[hsl(var(--assistant-surface-soft))]/50 text-[hsl(var(--assistant-text-secondary))]",
              )}
            >
              <p className="font-medium">
                {t(`assistant.outcome.${outcome.kind}.title`)}
              </p>
              {(outcome.kind === "failed" || outcome.kind === "cancelled" || outcome.kind === "restart_interrupted" || outcome.kind === "source_revoked" ||
                outcome.kind === "unknown" || outcome.kind === "empty") && (
                <p className="mt-1 text-xs leading-relaxed">
                  {t(`assistant.outcome.${outcome.kind}.next`)}
                </p>
              )}
              {outcome.kind === "unknown" && !message.sourceAccessRevoked && sessionId && outcome.diagnosticId && /^imt_[0-9a-f]{20}$/i.test(outcome.diagnosticId) && (
                <Suspense fallback={null}>
                  <ImageTaskReceipt key={JSON.stringify([message.id, sessionId, outcome.diagnosticId, message.imageTurnId])}
                    taskId={outcome.diagnosticId} sessionId={sessionId} turnId={message.imageTurnId} />
                </Suspense>
              )}
              {outcome.diagnosticId && (outcome.kind === "failed" || outcome.kind === "unknown" || outcome.kind === "restart_interrupted") && (
                <div className="mt-2 flex flex-wrap items-center gap-2 text-xs">
                  <span>{t("assistant.outcome.diagnosticId")}: <code>{outcome.diagnosticId}</code></span>
                  <button
                    type="button"
                    className="inline-flex items-center gap-1 rounded border border-current/30 px-1.5 py-0.5 hover:bg-black/5 dark:hover:bg-white/10"
                    aria-label={t("assistant.outcome.copyDiagnosticId")}
                    onClick={() => {
                      void navigator.clipboard.writeText(outcome.diagnosticId!).then(
                        () => setDiagnosticCopied(true),
                        () => setDiagnosticCopied(false),
                      );
                    }}
                  >
                    <Copy className="h-3 w-3" aria-hidden="true" />
                    {diagnosticCopied
                      ? t("assistant.outcome.copied")
                      : t("assistant.outcome.copyDiagnosticId")}
                  </button>
                </div>
              )}
            </div>
          )}

          {/* Agent phase display */}
          {runActive && message.agentPhase && (
            <AgentPhaseDisplay phase={message.agentPhase} />
          )}

          {/* Citation display */}
          {outputSettled && message.ragCitations && message.ragCitations.length > 0 && (
            <CitationDisplay
              citations={message.ragCitations}
              evaluation={message.ragEvaluation}
            />
          )}

          {/* Generated artifacts */}
          {outputSettled &&
            standaloneArtifacts.length > 0 &&
            (ASSISTANT_UI_V2 ? (
              <div className="mt-4 space-y-2">
                {standaloneArtifacts.map((artifact) => (
                  <InlineArtifactCard key={artifact.id} artifact={artifact} />
                ))}
              </div>
            ) : (
              <div className="mt-4 space-y-3">
                {standaloneArtifacts.map((artifact) => (
                  <Suspense key={artifact.id} fallback={null}>
                    <DocumentPreview
                      title={artifact.title || artifact.filename || "Document"}
                      content={artifact.content || ""}
                      format={
                        artifact.format === "md" || artifact.format === "markdown"
                          ? "markdown"
                          : "text"
                      }
                      downloadUrl={artifact.url}
                      defaultExpanded={false}
                      maxHeight={300}
                    />
                  </Suspense>
                ))}
              </div>
            ))}

          {/* Quiz card */}
          {message.quizData && (
            <div className="mt-4">
              <QuizCard quizData={message.quizData} existingResult={message.quizResult} />
            </div>
          )}

          {/* Stats */}
          {!runActive && <StatsBadge message={message} />}
        </div>
      )}
    </motion.div>
  );
});
