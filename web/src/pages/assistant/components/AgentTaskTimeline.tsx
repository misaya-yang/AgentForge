/**
 * Agent Task Timeline Component
 *
 * Manus-style task execution display with:
 * - Vertical timeline layout
 * - Collapsible task steps with expand/collapse
 * - Real-time sub-task status (searching, thinking, etc.)
 * - Smooth animations and transitions
 */

import { useState, useEffect } from "react";
import { motion, AnimatePresence } from "framer-motion";
import {
  ChevronDown,
  Circle,
  Loader2,
  CheckCircle2,
  AlertCircle,
  Search,
  FileText,
  Brain,
  Sparkles,
  Globe,
  Database,
  Code,
  Image,
  Presentation,
  File,
  Clock,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { SlideOutlinePreview } from "./SlideOutlinePreview";
import type { SlideOutline } from "../types";
import { useTranslation } from "react-i18next";

// =============================================================================
// Types
// =============================================================================

export type TaskStatus = "pending" | "in_progress" | "completed" | "failed";

export type TaskIconType = "search" | "web" | "kb" | "code" | "image" | "doc" | "ppt" | "file" | "brain";

export interface SubTask {
  id: string;
  label: string;
  status: TaskStatus;
  icon?: TaskIconType;
  durationMs?: number;
}

export interface AgentTask {
  id: string;
  title: string;
  description?: string;
  status: TaskStatus;
  icon?: TaskIconType;
  subTasks?: SubTask[];
  result?: string;
  error?: string;
  startTime?: Date | number;
  endTime?: Date | number;
  durationMs?: number;
  // Manus-style slide outline preview
  slideOutline?: SlideOutline;
}

export interface AgentTaskTimelineProps {
  goal?: string;
  tasks: AgentTask[];
  isThinking?: boolean;
  thinkingMessage?: string;
  className?: string;
}

// =============================================================================
// Sub-components
// =============================================================================

const TaskIcon = ({ icon, className }: { icon?: TaskIconType; className?: string }) => {
  const iconClass = cn("h-3.5 w-3.5", className);
  switch (icon) {
    case "search":
      return <Search className={iconClass} />;
    case "web":
      return <Globe className={iconClass} />;
    case "kb":
      return <Database className={iconClass} />;
    case "code":
      return <Code className={iconClass} />;
    case "image":
      return <Image className={iconClass} />;
    case "doc":
      return <FileText className={iconClass} />;
    case "ppt":
      return <Presentation className={iconClass} />;
    case "file":
      return <File className={iconClass} />;
    case "brain":
      return <Brain className={iconClass} />;
    default:
      return <Circle className={cn(iconClass, "scale-75")} />;
  }
};

// Helper to format duration
const formatDuration = (ms?: number): string => {
  if (!ms) return "";
  if (ms < 1000) return `${ms}ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = Math.floor(seconds % 60);
  return `${minutes}m ${remainingSeconds}s`;
};

const StatusIndicator = ({ status }: { status: TaskStatus }) => {
  switch (status) {
    case "pending":
      return (
        <div className="w-6 h-6 rounded-full bg-slate-200 dark:bg-slate-700 flex items-center justify-center">
          <Circle className="h-3 w-3 text-slate-400" />
        </div>
      );
    case "in_progress":
      return (
        <div className="w-6 h-6 rounded-full bg-blue-100 dark:bg-blue-900/50 flex items-center justify-center">
          <Loader2 className="h-3.5 w-3.5 text-blue-600 dark:text-blue-400 animate-spin" />
        </div>
      );
    case "completed":
      return (
        <div className="w-6 h-6 rounded-full bg-emerald-100 dark:bg-emerald-900/50 flex items-center justify-center">
          <CheckCircle2 className="h-3.5 w-3.5 text-emerald-600 dark:text-emerald-400" />
        </div>
      );
    case "failed":
      return (
        <div className="w-6 h-6 rounded-full bg-red-100 dark:bg-red-900/50 flex items-center justify-center">
          <AlertCircle className="h-3.5 w-3.5 text-red-600 dark:text-red-400" />
        </div>
      );
  }
};

// =============================================================================
// Task Item Component
// =============================================================================

function TaskTimelineItem({ task, isLast }: { task: AgentTask; isLast: boolean }) {
  const [isExpanded, setIsExpanded] = useState(task.status === "in_progress");

  // Auto-expand when task starts
  useEffect(() => {
    if (task.status === "in_progress") {
      setIsExpanded(true);
    }
  }, [task.status]);

  const hasContent = task.subTasks?.length || task.description || task.result || task.error;

  return (
    <div className="relative">
      {/* Timeline connector line */}
      {!isLast && (
        <div
          className={cn(
            "absolute left-3 top-8 w-0.5 h-[calc(100%-16px)]",
            task.status === "completed"
              ? "bg-emerald-300 dark:bg-emerald-700"
              : task.status === "failed"
              ? "bg-red-300 dark:bg-red-700"
              : "bg-slate-200 dark:bg-slate-700"
          )}
        />
      )}

      {/* Task header */}
      <div
        className={cn(
          "flex items-start gap-3 cursor-pointer group",
          hasContent && "hover:bg-slate-50 dark:hover:bg-slate-800/50 -mx-2 px-2 py-1 rounded-lg transition-colors"
        )}
        onClick={() => hasContent && setIsExpanded(!isExpanded)}
      >
        {/* Status indicator */}
        <StatusIndicator status={task.status} />

        {/* Title, icon, duration and expand icon */}
        <div className="flex-1 min-w-0 pt-0.5">
          <div className="flex items-center gap-2">
            {/* Task icon */}
            {task.icon && (
              <div className={cn(
                "shrink-0",
                task.status === "completed" && "text-emerald-600 dark:text-emerald-400",
                task.status === "failed" && "text-red-600 dark:text-red-400",
                task.status === "in_progress" && "text-blue-600 dark:text-blue-400",
                task.status === "pending" && "text-slate-400"
              )}>
                <TaskIcon icon={task.icon} />
              </div>
            )}
            <span
              className={cn(
                "text-sm font-medium",
                task.status === "completed" && "text-slate-600 dark:text-slate-400",
                task.status === "failed" && "text-red-700 dark:text-red-300",
                task.status === "in_progress" && "text-blue-700 dark:text-blue-300",
                task.status === "pending" && "text-slate-500 dark:text-slate-400"
              )}
            >
              {task.title}
            </span>
            {/* Duration badge for completed tasks */}
            {task.durationMs && task.status === "completed" && (
              <span className="text-[10px] text-slate-400 dark:text-slate-500 flex items-center gap-0.5 bg-slate-100 dark:bg-slate-800 px-1.5 py-0.5 rounded">
                <Clock className="h-2.5 w-2.5" />
                {formatDuration(task.durationMs)}
              </span>
            )}
            {hasContent && (
              <motion.div
                animate={{ rotate: isExpanded ? 0 : -90 }}
                transition={{ duration: 0.15 }}
                className="text-slate-400 ml-auto"
              >
                <ChevronDown className="h-4 w-4" />
              </motion.div>
            )}
          </div>
        </div>
      </div>

      {/* Expandable content */}
      <AnimatePresence>
        {isExpanded && hasContent && (
          <motion.div
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.2, ease: "easeInOut" }}
            className="overflow-hidden"
          >
            <div className="ml-9 mt-2 mb-3 space-y-2">
              {/* Description */}
              {task.description && (
                <p className="text-xs text-slate-500 dark:text-slate-400">
                  {task.description}
                </p>
              )}

              {/* Sub-tasks */}
              {task.subTasks?.map((subTask, idx) => (
                <motion.div
                  key={subTask.id}
                  initial={{ opacity: 0, x: -10 }}
                  animate={{ opacity: 1, x: 0 }}
                  transition={{ delay: idx * 0.05 }}
                  className={cn(
                    "flex items-center gap-2 text-xs py-1.5 px-3 rounded-lg",
                    subTask.status === "in_progress" &&
                      "bg-blue-50 dark:bg-blue-900/30 text-blue-700 dark:text-blue-300",
                    subTask.status === "completed" &&
                      "bg-emerald-50 dark:bg-emerald-900/30 text-emerald-700 dark:text-emerald-300",
                    subTask.status === "failed" &&
                      "bg-red-50 dark:bg-red-900/30 text-red-700 dark:text-red-300",
                    subTask.status === "pending" &&
                      "bg-slate-50 dark:bg-slate-800/50 text-slate-500 dark:text-slate-400"
                  )}
                >
                  {subTask.status === "in_progress" ? (
                    <Loader2 className="h-3 w-3 animate-spin" />
                  ) : (
                    <TaskIcon icon={subTask.icon} />
                  )}
                  <span className="flex-1">{subTask.label}</span>
                  {subTask.durationMs && subTask.status === "completed" && (
                    <span className="text-[10px] opacity-60 flex items-center gap-0.5">
                      <Clock className="h-2.5 w-2.5" />
                      {formatDuration(subTask.durationMs)}
                    </span>
                  )}
                </motion.div>
              ))}

              {/* Slide outline preview (Manus-style) */}
              {task.slideOutline && (
                <SlideOutlinePreview
                  outline={task.slideOutline}
                  isGenerating={task.status === "in_progress"}
                  className="mt-2"
                />
              )}

              {/* Result */}
              {task.result && task.status === "completed" && !task.slideOutline && (
                <div className="text-xs text-emerald-600 dark:text-emerald-400 bg-emerald-50 dark:bg-emerald-900/20 p-2 rounded-lg">
                  {task.result}
                </div>
              )}

              {/* Error */}
              {task.error && task.status === "failed" && (
                <div className="text-xs text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/20 p-2 rounded-lg">
                  {task.error}
                </div>
              )}
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

// =============================================================================
// Thinking Indicator Component
// =============================================================================

function ThinkingIndicator({ message }: { message?: string }) {
  const { t } = useTranslation();
  return (
    <motion.div
      initial={{ opacity: 0, y: 10 }}
      animate={{ opacity: 1, y: 0 }}
      exit={{ opacity: 0, y: -10 }}
      className="flex items-center gap-3 py-2"
    >
      <div className="w-6 h-6 rounded-full bg-primary flex items-center justify-center">
        <Brain className="h-3.5 w-3.5 text-white animate-pulse" />
      </div>
      <div className="flex items-center gap-2">
        <span className="text-sm text-violet-700 dark:text-violet-300 font-medium">
          {message || t("assistant.thinking")}
        </span>
        <div className="flex gap-1">
          {[0, 1, 2].map((i) => (
            <motion.div
              key={i}
              className="w-1.5 h-1.5 rounded-full bg-violet-500"
              animate={{
                scale: [1, 1.3, 1],
                opacity: [0.5, 1, 0.5],
              }}
              transition={{
                duration: 1,
                repeat: Infinity,
                delay: i * 0.2,
              }}
            />
          ))}
        </div>
      </div>
    </motion.div>
  );
}

// =============================================================================
// Main Component
// =============================================================================

export function AgentTaskTimeline({
  goal,
  tasks,
  isThinking,
  thinkingMessage,
  className,
}: AgentTaskTimelineProps) {
  const { t } = useTranslation();
  // Defensive check for tasks array
  const safeTasks = Array.isArray(tasks) ? tasks : [];

  if (safeTasks.length === 0 && !isThinking) {
    return null;
  }

  const completedCount = safeTasks.filter((t) => t.status === "completed").length;
  const totalCount = safeTasks.length;

  return (
    <div
      className={cn(
        "bg-white dark:bg-slate-900 rounded-2xl border border-slate-200 dark:border-slate-700 overflow-hidden",
        className
      )}
    >
      {/* Header */}
      <div className="px-4 py-3 bg-linear-to-r from-slate-50 to-slate-100/50 dark:from-slate-800 dark:to-slate-800/50 border-b border-slate-200 dark:border-slate-700">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <Sparkles className="h-4 w-4 text-amber-500" />
            <span className="text-sm font-medium text-slate-700 dark:text-slate-200">
              {goal || t("assistant.taskGoalFallback")}
            </span>
          </div>
          {totalCount > 0 && (
            <span className="text-xs text-slate-500 dark:text-slate-400">
              {t("assistant.taskCompleted", { completed: completedCount, total: totalCount })}
            </span>
          )}
        </div>
      </div>

      {/* Timeline */}
      <div className="p-4 space-y-3">
        {safeTasks.map((task, index) => (
          <TaskTimelineItem
            key={task.id}
            task={task}
            isLast={index === safeTasks.length - 1 && !isThinking}
          />
        ))}

        {/* Thinking indicator at bottom */}
        <AnimatePresence>
          {isThinking && <ThinkingIndicator message={thinkingMessage} />}
        </AnimatePresence>
      </div>
    </div>
  );
}

export default AgentTaskTimeline;
