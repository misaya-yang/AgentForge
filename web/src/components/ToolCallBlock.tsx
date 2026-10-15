/**
 * ToolCallBlock - Claude Desktop-inspired tool call visualization
 *
 * Design principles:
 * - Left border accent colored by status (amber/blue/emerald/rose)
 * - Live elapsed time counter for running tools
 * - Shimmer progress bar for running state
 * - Auto-expand when running, smooth collapse/expand
 * - Professional, minimal chrome
 */

import { useState, useEffect, useRef, useCallback } from "react";
import { motion, AnimatePresence } from "framer-motion";
import { cn } from "@/lib/utils";
import type { ToolCall } from "@/types/gateway";
import { useTranslation } from "react-i18next";
import {
  CheckCircle2,
  Clock,
  Loader2,
  AlertCircle,
  ChevronRight,
  Terminal,
} from "lucide-react";

interface ToolCallBlockProps {
  toolCall: ToolCall;
  result?: string;
  argsText?: string;
  argsValid?: boolean;
  /** Optional step number for sequential display */
  stepNumber?: number;
}

// Status visual configs
const statusConfig = {
  pending: {
    label: "Waiting",
    labelKey: "playground.toolCall.pending",
    accent: "border-l-amber-400 dark:border-l-amber-500",
    iconBg: "bg-amber-100 dark:bg-amber-900/40",
    text: "text-amber-600 dark:text-amber-400",
    icon: Clock,
  },
  running: {
    label: "Running",
    labelKey: "playground.toolCall.running",
    accent: "border-l-blue-500 dark:border-l-blue-400",
    iconBg: "bg-blue-100 dark:bg-blue-900/40",
    text: "text-blue-600 dark:text-blue-400",
    icon: Loader2,
  },
  completed: {
    label: "Done",
    labelKey: "playground.toolCall.completed",
    accent: "border-l-emerald-500 dark:border-l-emerald-400",
    iconBg: "bg-emerald-100 dark:bg-emerald-900/40",
    text: "text-emerald-600 dark:text-emerald-400",
    icon: CheckCircle2,
  },
  error: {
    label: "Failed",
    labelKey: "playground.toolCall.error",
    accent: "border-l-rose-500 dark:border-l-rose-400",
    iconBg: "bg-rose-100 dark:bg-rose-900/40",
    text: "text-rose-600 dark:text-rose-400",
    icon: AlertCircle,
  },
} as const;

/** Live elapsed time counter for running tools */
function ElapsedTimer({ className }: { className?: string }) {
  const [elapsed, setElapsed] = useState(0);
  const startRef = useRef<number | null>(null);

  useEffect(() => {
    startRef.current = performance.now();
    const id = setInterval(() => {
      setElapsed((performance.now() - (startRef.current ?? performance.now())) / 1000);
    }, 100);
    return () => clearInterval(id);
  }, []);

  return (
    <span className={cn("tabular-nums font-mono text-[10px]", className)}>
      {elapsed.toFixed(1)}s
    </span>
  );
}

/** Format a tool name for display: snake_case -> readable */
function formatToolName(name: string): string {
  return name || "unknown_tool";
}

export function ToolCallBlock({
  toolCall,
  result,
  argsText,
  argsValid,
  stepNumber,
}: ToolCallBlockProps) {
  const { t } = useTranslation();
  const status = toolCall.status || "pending";
  const config = statusConfig[status] || statusConfig.pending;
  const StatusIcon = config.icon;
  const isRunning = status === "running";
  const [userExpanded, setUserExpanded] = useState<boolean | null>(null);
  const isExpanded = userExpanded ?? isRunning;

  // Parse arguments
  const rawArgs = argsText ?? toolCall.arguments ?? "";
  const hasValidArgs =
    argsValid ??
    (() => {
      if (!rawArgs) return false;
      try {
        JSON.parse(rawArgs);
        return true;
      } catch {
        return false;
      }
    })();

  const formatJson = useCallback((raw: string, valid: boolean): string => {
    if (!valid || !raw) return raw;
    try {
      return JSON.stringify(JSON.parse(raw), null, 2);
    } catch {
      return raw;
    }
  }, []);

  const formattedArgs = formatJson(rawArgs, hasValidArgs);

  // Parse result
  let formattedResult: string = result || "";
  if (result) {
    try {
      const parsed = JSON.parse(result);
      formattedResult =
        typeof parsed === "object"
          ? JSON.stringify(parsed, null, 2)
          : String(result);
    } catch {
      formattedResult = result;
    }
  }

  // Compact preview for collapsed state
  const argsPreview = (() => {
    if (!rawArgs) return "";
    try {
      const parsed = JSON.parse(rawArgs);
      // Show first key-value pair as preview
      const entries = Object.entries(parsed);
      if (entries.length === 0) return "{}";
      const [key, val] = entries[0];
      const valStr =
        typeof val === "string"
          ? `"${val.length > 40 ? val.slice(0, 40) + "..." : val}"`
          : JSON.stringify(val);
      const preview = `${key}: ${valStr}`;
      return entries.length > 1 ? `${preview}, ...` : preview;
    } catch {
      return rawArgs.length > 60 ? rawArgs.slice(0, 60) + "..." : rawArgs;
    }
  })();

  return (
    <motion.div
      layout="position"
      initial={{ opacity: 0, x: -8 }}
      animate={{ opacity: 1, x: 0 }}
      transition={{ duration: 0.25, ease: [0.25, 0.1, 0.25, 1] }}
      className={cn(
        "relative w-full rounded-lg overflow-hidden",
        "border border-slate-200/60 dark:border-slate-700/40",
        "border-l-[3px] transition-[border-color,box-shadow] duration-300",
        config.accent,
        isExpanded
          ? "shadow-xs dark:shadow-black/20"
          : "shadow-none"
      )}
    >
      {/* Shimmer progress bar for running state */}
      {isRunning && (
        <div className="absolute bottom-0 left-0 right-0 h-[2px] overflow-hidden bg-blue-100/50 dark:bg-blue-900/30">
          <div className="h-full bg-linear-to-r from-transparent via-blue-500/70 to-transparent animate-shimmer" />
        </div>
      )}

      {/* Compact Header — single line: chevron + icon + name + args + status */}
      <button
        onClick={() => setUserExpanded((current) => !(current ?? isRunning))}
        className={cn(
          "flex w-full items-center gap-1.5 px-2.5 py-1.5 text-left",
          "transition-colors duration-150",
          "bg-white/50 dark:bg-white/2",
          "hover:bg-slate-50/80 dark:hover:bg-white/4",
          "focus:outline-hidden focus-visible:ring-2 focus-visible:ring-blue-500/30 focus-visible:ring-inset"
        )}
        type="button"
      >
        <motion.div
          animate={{ rotate: isExpanded ? 90 : 0 }}
          transition={{ duration: 0.15, ease: "easeOut" }}
          className="shrink-0"
        >
          <ChevronRight className="h-2.5 w-2.5 text-slate-400 dark:text-slate-500" />
        </motion.div>

        <StatusIcon
          className={cn(
            "h-3 w-3 shrink-0",
            config.text,
            isRunning && "animate-spin"
          )}
        />

        {/* Tool name + inline args — all on one line */}
        <div className="flex-1 min-w-0 flex items-center gap-1.5 overflow-hidden">
          {stepNumber != null && (
            <span className="font-mono text-[10px] text-slate-400 dark:text-slate-500 shrink-0">
              #{stepNumber}
            </span>
          )}
          <span className="font-mono text-[11px] font-semibold text-slate-700 dark:text-slate-200 truncate shrink-0">
            {formatToolName(toolCall.name)}
          </span>
          {!isExpanded && argsPreview && (
            <span className="text-[10px] text-slate-400 dark:text-slate-500 font-mono truncate">
              {argsPreview}
            </span>
          )}
        </div>

        {/* Status badge */}
        <div className="flex items-center gap-1 shrink-0">
          {isRunning && <ElapsedTimer className={config.text} />}
          <span className={cn("text-[9px] font-semibold uppercase tracking-wider", config.text)}>
            {t(config.labelKey, config.label)}
          </span>
        </div>
      </button>

      {/* Expandable content */}
      <AnimatePresence initial={false}>
        {isExpanded && (
          <motion.div
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.2, ease: [0.25, 0.1, 0.25, 1] }}
            className="overflow-hidden"
          >
            <div className="border-t border-slate-200/50 dark:border-slate-700/30">
              {/* Arguments section */}
              {rawArgs && (
                <div className="px-3 py-2.5">
                  <div className="flex items-center gap-1.5 mb-1.5">
                    <Terminal className="h-2.5 w-2.5 text-slate-400 dark:text-slate-500" />
                    <span className="text-[9px] font-bold uppercase tracking-widest text-slate-400 dark:text-slate-500">
                      {hasValidArgs
                        ? t("playground.toolCall.arguments", "Arguments")
                        : t(
                            "playground.toolCall.argumentsPartial",
                            "Arguments (streaming...)"
                          )}
                    </span>
                    {!hasValidArgs && (
                      <span className="relative flex h-1.5 w-1.5">
                        <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-blue-400 opacity-75" />
                        <span className="relative inline-flex rounded-full h-1.5 w-1.5 bg-blue-500" />
                      </span>
                    )}
                  </div>
                  <pre
                    className={cn(
                      "max-h-40 overflow-auto rounded-md p-2.5",
                      "bg-slate-50 dark:bg-slate-900/60",
                      "text-[11px] font-mono text-slate-600 dark:text-slate-300 leading-relaxed",
                      "border border-slate-200/50 dark:border-slate-700/40",
                      "whitespace-pre-wrap wrap-break-word"
                    )}
                  >
                    {formattedArgs}
                  </pre>
                </div>
              )}

              {/* Result section */}
              {result && (
                <div
                  className={cn(
                    "px-3 py-2.5",
                    rawArgs &&
                      "border-t border-slate-200/30 dark:border-slate-700/20"
                  )}
                >
                  <div className="flex items-center gap-1.5 mb-1.5">
                    <CheckCircle2 className="h-2.5 w-2.5 text-emerald-500 dark:text-emerald-400" />
                    <span className="text-[9px] font-bold uppercase tracking-widest text-emerald-500 dark:text-emerald-400">
                      {t("playground.toolCall.result", "Result")}
                    </span>
                  </div>
                  <pre
                    className={cn(
                      "max-h-40 overflow-auto rounded-md p-2.5",
                      "bg-emerald-50/50 dark:bg-emerald-950/20",
                      "text-[11px] font-mono text-slate-600 dark:text-emerald-200 leading-relaxed",
                      "border border-emerald-200/40 dark:border-emerald-800/30",
                      "whitespace-pre-wrap wrap-break-word"
                    )}
                  >
                    {formattedResult}
                  </pre>
                </div>
              )}
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </motion.div>
  );
}
