/**
 * AgentTimeline - Manus-style task execution timeline
 *
 * A collapsible timeline component that visualizes the agent's execution
 * steps, tool calls, and artifacts in real-time. Inspired by Manus.im's
 * clean, interactive execution visualization.
 *
 * Features:
 * - Real-time step tracking with progress indicators
 * - Collapsible step details
 * - Clickable artifacts and tool results
 * - Smooth animations and transitions
 */

import * as React from "react";
import { motion, AnimatePresence } from "framer-motion";
import {
  ChevronDown,
  CheckCircle2,
  Circle,
  Loader2,
  AlertCircle,
  FileText,
  Search,
  Code2,
  Image as ImageIcon,
  Wrench,
  Brain,
  ListChecks,
  Sparkles,
  Clock,
  XCircle,
  StopCircle,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { formatDuration } from "@/lib/format";
import { useTranslation } from "react-i18next";

// ============================================================================
// Types
// ============================================================================

export type StepStatus = "pending" | "running" | "completed" | "error";

export type StepType =
  | "planning"
  | "analysis"
  | "outline"
  | "generating"
  | "validating"
  | "repairing"
  | "tool_call"
  | "search"
  | "code_execution"
  | "image_generation"
  | "document_generation"
  | "complete";

export interface TimelineArtifact {
  id: string;
  type: "file" | "document" | "image" | "code";
  name: string;
  url?: string;
  mimeType?: string;
  size?: number;
}

export interface ToolCallInfo {
  id: string;
  name: string;
  arguments?: string;
  result?: string;
  status: "pending" | "running" | "completed" | "error";
  durationMs?: number;
}

export interface TimelineStep {
  id: string;
  type: StepType;
  name: string;
  status: StepStatus;
  message?: string;
  progress?: number;
  startTime?: number;
  endTime?: number;
  children?: TimelineStep[];
  artifacts?: TimelineArtifact[];
  toolCalls?: ToolCallInfo[];
  metadata?: Record<string, unknown>;
}

/**
 * Phase progress for 8-step AgentLoop visualization.
 */
export interface PhaseProgress {
  currentPhase: number;     // 1-8
  totalPhases: number;      // 8
  phaseName: string;        // e.g., "memory_loading"
  displayName: string;      // Chinese display name
  status: "pending" | "running" | "completed";
  durationMs?: number;
}

export interface TimelineState {
  runId: string;
  status: "idle" | "running" | "completed" | "error" | "cancelled";
  steps: TimelineStep[];
  currentStepId?: string;
  startTime?: number;
  endTime?: number;
  metadata?: Record<string, unknown>;
  // Phase 1 Optimization: Phase progress
  phaseProgress?: PhaseProgress;
}

interface AgentTimelineProps {
  state: TimelineState;
  onArtifactClick?: (artifact: TimelineArtifact) => void;
  onStepClick?: (step: TimelineStep) => void;
  onCancel?: () => void;  // Phase 1 Optimization: Cancel callback
  isCancellable?: boolean;  // Phase 1 Optimization: Can be cancelled
  className?: string;
  defaultExpanded?: boolean;
  compact?: boolean;
}

// ============================================================================
// Helpers
// ============================================================================

const stepTypeIcons: Record<StepType, React.ElementType> = {
  planning: Brain,
  analysis: Search,
  outline: ListChecks,
  generating: Sparkles,
  validating: CheckCircle2,
  repairing: Wrench,
  tool_call: Wrench,
  search: Search,
  code_execution: Code2,
  image_generation: ImageIcon,
  document_generation: FileText,
  complete: CheckCircle2,
};

const stepTypeColors: Record<StepType, string> = {
  planning: "text-violet-500",
  analysis: "text-blue-500",
  outline: "text-indigo-500",
  generating: "text-emerald-500",
  validating: "text-amber-500",
  repairing: "text-orange-500",
  tool_call: "text-purple-500",
  search: "text-cyan-500",
  code_execution: "text-lime-500",
  image_generation: "text-pink-500",
  document_generation: "text-blue-500",
  complete: "text-green-500",
};


function getStepDuration(step: TimelineStep): number | undefined {
  if (step.startTime && step.endTime) {
    return step.endTime - step.startTime;
  }
  return undefined;
}

// ============================================================================
// Sub-components
// ============================================================================

/** Status indicator dot with animation */
function StatusIndicator({ status }: { status: StepStatus }) {
  if (status === "running") {
    return (
      <motion.div
        animate={{ scale: [1, 1.2, 1] }}
        transition={{ repeat: Infinity, duration: 1.5 }}
        className="relative flex items-center justify-center"
      >
        <Loader2 className="h-4 w-4 animate-spin text-blue-500" />
      </motion.div>
    );
  }

  if (status === "completed") {
    return (
      <motion.div
        initial={{ scale: 0 }}
        animate={{ scale: 1 }}
        className="flex items-center justify-center"
      >
        <CheckCircle2 className="h-4 w-4 text-green-500" />
      </motion.div>
    );
  }

  if (status === "error") {
    return (
      <motion.div
        initial={{ scale: 0 }}
        animate={{ scale: 1 }}
        className="flex items-center justify-center"
      >
        <AlertCircle className="h-4 w-4 text-red-500" />
      </motion.div>
    );
  }

  // pending
  return (
    <div className="flex items-center justify-center">
      <Circle className="h-4 w-4 text-slate-300 dark:text-slate-600" />
    </div>
  );
}

/** Progress bar for step progress */
function StepProgress({ progress }: { progress: number }) {
  return (
    <div className="w-full h-1 bg-slate-100 dark:bg-slate-800 rounded-full overflow-hidden">
      <motion.div
        initial={{ width: 0 }}
        animate={{ width: `${Math.min(100, Math.max(0, progress * 100))}%` }}
        className="h-full bg-linear-to-r from-blue-500 to-violet-500 rounded-full"
        transition={{ duration: 0.3 }}
      />
    </div>
  );
}

/**
 * Phase 1 Optimization: 8-step AgentLoop progress bar
 */
function PhaseProgressBar({ progress }: { progress: PhaseProgress }) {
  const { currentPhase, totalPhases, displayName, status } = progress;

  return (
    <div className="w-full space-y-1">
      {/* Phase label */}
      <div className="flex items-center justify-between text-xs">
        <span className="text-slate-600 dark:text-slate-400">
          {displayName}
        </span>
        <span className="text-slate-500 dark:text-slate-500">
          {currentPhase}/{totalPhases}
        </span>
      </div>
      {/* Progress segments */}
      <div className="flex gap-0.5 h-1.5">
        {Array.from({ length: totalPhases }, (_, i) => {
          const phaseIndex = i + 1;
          const isCompleted = phaseIndex < currentPhase;
          const isCurrent = phaseIndex === currentPhase;
          const isPending = phaseIndex > currentPhase;

          return (
            <motion.div
              key={phaseIndex}
              className={cn(
                "flex-1 rounded-full transition-colors duration-300",
                isCompleted && "bg-green-500",
                isCurrent && status === "running" && "bg-blue-500",
                isCurrent && status === "completed" && "bg-green-500",
                isPending && "bg-slate-200 dark:bg-slate-700"
              )}
              initial={false}
              animate={{
                scale: isCurrent && status === "running" ? [1, 1.05, 1] : 1,
              }}
              transition={{
                repeat: isCurrent && status === "running" ? Infinity : 0,
                duration: 1,
              }}
            />
          );
        })}
      </div>
    </div>
  );
}

/** Artifact chip - clickable */
function ArtifactChip({
  artifact,
  onClick,
}: {
  artifact: TimelineArtifact;
  onClick?: () => void;
}) {
  const icon =
    artifact.type === "image" ? (
      <ImageIcon className="h-3 w-3" />
    ) : artifact.type === "code" ? (
      <Code2 className="h-3 w-3" />
    ) : (
      <FileText className="h-3 w-3" />
    );

  return (
    <motion.button
      initial={{ opacity: 0, scale: 0.9 }}
      animate={{ opacity: 1, scale: 1 }}
      onClick={onClick}
      className={cn(
        "inline-flex items-center gap-1.5 px-2.5 py-1 rounded-lg text-xs font-medium",
        "bg-slate-100 dark:bg-slate-800 hover:bg-slate-200 dark:hover:bg-slate-700",
        "text-slate-700 dark:text-slate-300 transition-colors",
        onClick && "cursor-pointer hover:ring-2 hover:ring-blue-500/30"
      )}
    >
      {icon}
      <span className="truncate max-w-[120px]">{artifact.name}</span>
    </motion.button>
  );
}

/** Tool call mini-card */
function ToolCallChip({
  toolCall,
  onClick,
}: {
  toolCall: ToolCallInfo;
  onClick?: () => void;
}) {
  const statusColors = {
    pending: "bg-slate-100 dark:bg-slate-800 text-slate-500",
    running: "bg-blue-50 dark:bg-blue-900/30 text-blue-600 dark:text-blue-400",
    completed: "bg-green-50 dark:bg-green-900/30 text-green-600 dark:text-green-400",
    error: "bg-red-50 dark:bg-red-900/30 text-red-600 dark:text-red-400",
  };

  return (
    <motion.button
      initial={{ opacity: 0, x: -10 }}
      animate={{ opacity: 1, x: 0 }}
      onClick={onClick}
      className={cn(
        "inline-flex items-center gap-1.5 px-2.5 py-1 rounded-lg text-xs font-medium",
        statusColors[toolCall.status],
        "transition-all duration-200",
        onClick && "cursor-pointer hover:ring-2 hover:ring-purple-500/30"
      )}
    >
      <Wrench className="h-3 w-3" />
      <span>{toolCall.name}</span>
      {toolCall.status === "running" && (
        <Loader2 className="h-3 w-3 animate-spin" />
      )}
      {toolCall.durationMs && toolCall.status === "completed" && (
        <span className="text-[10px] opacity-70">
          {formatDuration(toolCall.durationMs)}
        </span>
      )}
    </motion.button>
  );
}

/** Single timeline step */
function TimelineStepItem({
  step,
  isLast,
  onArtifactClick,
  onStepClick,
  defaultExpanded,
  depth = 0,
}: {
  step: TimelineStep;
  isLast: boolean;
  onArtifactClick?: (artifact: TimelineArtifact) => void;
  onStepClick?: (step: TimelineStep) => void;
  defaultExpanded?: boolean;
  depth?: number;
}) {
  const [isExpanded, setIsExpanded] = React.useState(
    defaultExpanded ?? step.status === "running"
  );

  const hasDetails =
    (step.children && step.children.length > 0) ||
    (step.artifacts && step.artifacts.length > 0) ||
    (step.toolCalls && step.toolCalls.length > 0) ||
    step.message;

  const Icon = stepTypeIcons[step.type] || Circle;
  const colorClass = stepTypeColors[step.type] || "text-slate-500";
  const duration = getStepDuration(step);

  // Auto-expand when running
  React.useEffect(() => {
    if (step.status === "running") {
      setIsExpanded(true);
    }
  }, [step.status]);

  return (
    <div className={cn("relative", depth > 0 && "ml-6")}>
      {/* Connector line */}
      {!isLast && (
        <div
          className={cn(
            "absolute left-2 top-6 w-px bg-slate-200 dark:bg-slate-700",
            step.status === "completed"
              ? "bg-green-200 dark:bg-green-800"
              : step.status === "running"
                ? "bg-blue-200 dark:bg-blue-800"
                : ""
          )}
          style={{ bottom: "-0.5rem" }}
        />
      )}

      {/* Step header */}
      <div
        className={cn(
          "flex items-start gap-3 py-2 rounded-lg transition-colors",
          hasDetails && "cursor-pointer hover:bg-slate-50 dark:hover:bg-slate-800/50"
        )}
        onClick={() => {
          if (hasDetails) setIsExpanded(!isExpanded);
          onStepClick?.(step);
        }}
      >
        {/* Status indicator */}
        <div className="mt-0.5">
          <StatusIndicator status={step.status} />
        </div>

        {/* Content */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            {/* Step icon */}
            <Icon className={cn("h-4 w-4", colorClass)} />

            {/* Step name */}
            <span
              className={cn(
                "text-sm font-medium",
                step.status === "completed"
                  ? "text-slate-700 dark:text-slate-300"
                  : step.status === "running"
                    ? "text-slate-900 dark:text-slate-100"
                    : "text-slate-500 dark:text-slate-400"
              )}
            >
              {step.name}
            </span>

            {/* Duration */}
            {duration && step.status === "completed" && (
              <span className="text-[10px] text-slate-400 dark:text-slate-500">
                {formatDuration(duration)}
              </span>
            )}

            {/* Expand/collapse */}
            {hasDetails && (
              <motion.div
                animate={{ rotate: isExpanded ? 180 : 0 }}
                transition={{ duration: 0.2 }}
              >
                <ChevronDown className="h-3.5 w-3.5 text-slate-400" />
              </motion.div>
            )}
          </div>

          {/* Progress bar */}
          {step.status === "running" && step.progress !== undefined && (
            <div className="mt-2">
              <StepProgress progress={step.progress} />
            </div>
          )}
        </div>
      </div>

      {/* Expanded details */}
      <AnimatePresence>
        {isExpanded && hasDetails && (
          <motion.div
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.2 }}
            className="overflow-hidden"
          >
            <div className="ml-7 pl-3 border-l-2 border-slate-200 dark:border-slate-700 space-y-2 pb-2">
              {/* Message */}
              {step.message && (
                <p className="text-xs text-slate-500 dark:text-slate-400">
                  {step.message}
                </p>
              )}

              {/* Tool calls */}
              {step.toolCalls && step.toolCalls.length > 0 && (
                <div className="flex flex-wrap gap-1.5">
                  {step.toolCalls.map((tc) => (
                    <ToolCallChip key={tc.id} toolCall={tc} />
                  ))}
                </div>
              )}

              {/* Artifacts */}
              {step.artifacts && step.artifacts.length > 0 && (
                <div className="flex flex-wrap gap-1.5">
                  {step.artifacts.map((artifact) => (
                    <ArtifactChip
                      key={artifact.id}
                      artifact={artifact}
                      onClick={() => onArtifactClick?.(artifact)}
                    />
                  ))}
                </div>
              )}

              {/* Child steps */}
              {step.children && step.children.length > 0 && (
                <div className="mt-2">
                  {step.children.map((child, idx) => (
                    <TimelineStepItem
                      key={child.id}
                      step={child}
                      isLast={idx === step.children!.length - 1}
                      onArtifactClick={onArtifactClick}
                      onStepClick={onStepClick}
                      depth={depth + 1}
                    />
                  ))}
                </div>
              )}
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

// ============================================================================
// Main Component
// ============================================================================

export function AgentTimeline({
  state,
  onArtifactClick,
  onStepClick,
  onCancel,
  isCancellable = false,
  className,
  defaultExpanded = false,
  compact = false,
}: AgentTimelineProps) {
  const { t } = useTranslation();
  const [isCollapsed, setIsCollapsed] = React.useState(false);
  const [isCancelling, setIsCancelling] = React.useState(false);

  const totalDuration =
    state.startTime && state.endTime
      ? state.endTime - state.startTime
      : undefined;

  const completedSteps = state.steps.filter((s) => s.status === "completed").length;
  const totalSteps = state.steps.length;

  // Handle cancel button click
  const handleCancel = async (e: React.MouseEvent) => {
    e.stopPropagation();
    if (onCancel && !isCancelling) {
      setIsCancelling(true);
      try {
        await onCancel();
      } finally {
        setIsCancelling(false);
      }
    }
  };

  if (state.steps.length === 0 && !state.phaseProgress) {
    return null;
  }

  return (
    <div
      className={cn(
        "rounded-xl border border-slate-200 dark:border-slate-800 bg-white dark:bg-slate-900 overflow-hidden",
        className
      )}
    >
      {/* Header */}
      <div
        className={cn(
          "flex items-center justify-between px-4 py-3 border-b border-slate-200 dark:border-slate-800",
          "cursor-pointer hover:bg-slate-50 dark:hover:bg-slate-800/50 transition-colors"
        )}
        onClick={() => setIsCollapsed(!isCollapsed)}
      >
        <div className="flex items-center gap-3">
          {/* Run status indicator */}
          <div className="flex items-center gap-2">
            {state.status === "running" ? (
              <Loader2 className="h-4 w-4 animate-spin text-blue-500" />
            ) : state.status === "completed" ? (
              <CheckCircle2 className="h-4 w-4 text-green-500" />
            ) : state.status === "error" ? (
              <AlertCircle className="h-4 w-4 text-red-500" />
            ) : state.status === "cancelled" ? (
              <XCircle className="h-4 w-4 text-orange-500" />
            ) : (
              <Clock className="h-4 w-4 text-slate-400" />
            )}

            <span className="text-sm font-semibold text-slate-900 dark:text-slate-100">
              {state.status === "running"
                ? t("agent.timeline.running", "Agent Running")
                : state.status === "completed"
                  ? t("agent.timeline.completed", "Agent Completed")
                  : state.status === "error"
                    ? t("agent.timeline.error", "Agent Error")
                    : state.status === "cancelled"
                      ? t("agent.timeline.cancelled", "Agent Cancelled")
                      : t("agent.timeline.idle", "Agent")}
            </span>
          </div>

          {/* Progress indicator */}
          {state.status === "running" && (
            <span className="text-xs text-slate-500">
              {completedSteps}/{totalSteps} {t("agent.timeline.steps", "steps")}
            </span>
          )}

          {/* Duration */}
          {totalDuration && state.status === "completed" && (
            <span className="text-xs text-slate-500">
              {formatDuration(totalDuration)}
            </span>
          )}
        </div>

        <div className="flex items-center gap-2">
          {/* Cancel button */}
          {isCancellable && state.status === "running" && (
            <motion.button
              initial={{ opacity: 0, scale: 0.9 }}
              animate={{ opacity: 1, scale: 1 }}
              onClick={handleCancel}
              disabled={isCancelling}
              className={cn(
                "flex items-center gap-1.5 px-2.5 py-1 rounded-lg text-xs font-medium",
                "bg-orange-50 dark:bg-orange-900/30 text-orange-600 dark:text-orange-400",
                "hover:bg-orange-100 dark:hover:bg-orange-900/50 transition-colors",
                "disabled:opacity-50 disabled:cursor-not-allowed"
              )}
            >
              {isCancelling ? (
                <Loader2 className="h-3 w-3 animate-spin" />
              ) : (
                <StopCircle className="h-3 w-3" />
              )}
              <span>{t("agent.timeline.cancel", "Cancel")}</span>
            </motion.button>
          )}

          {/* Collapse toggle */}
          <motion.div
            animate={{ rotate: isCollapsed ? -90 : 0 }}
            transition={{ duration: 0.2 }}
          >
            <ChevronDown className="h-4 w-4 text-slate-400" />
          </motion.div>
        </div>
      </div>

      {/* Phase Progress Bar */}
      {state.phaseProgress && state.status === "running" && (
        <div className="px-4 py-2 border-b border-slate-200 dark:border-slate-800 bg-slate-50 dark:bg-slate-800/50">
          <PhaseProgressBar progress={state.phaseProgress} />
        </div>
      )}

      {/* Timeline content */}
      <AnimatePresence>
        {!isCollapsed && (
          <motion.div
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.2 }}
          >
            <div className={cn("px-4 py-3", compact && "py-2")}>
              {state.steps.map((step, idx) => (
                <TimelineStepItem
                  key={step.id}
                  step={step}
                  isLast={idx === state.steps.length - 1}
                  onArtifactClick={onArtifactClick}
                  onStepClick={onStepClick}
                  defaultExpanded={defaultExpanded}
                />
              ))}
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

export default AgentTimeline;
