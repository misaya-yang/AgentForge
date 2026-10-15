/**
 * WorkflowBeat - Claude.ai / Cursor-style single-line workflow summary.
 *
 * Collapsed state: one muted row like
 *   "Searched the web, read 3 files, ran 2 commands  >"
 *
 * Running state: spinner replaces chevron; subtle pulse on the label.
 * Expanded state: reveals per-tool ToolCallBlock cards + optional thinking text.
 *
 * Replaces the old "big thinking panel + separate status bar + stacked tool
 * cards" composition with a single dense, quiet row — matching what Claude
 * desktop, Cowork and Cursor do.
 */

import { useEffect, useRef, useState } from "react";
import { motion, AnimatePresence } from "framer-motion";
import { useTranslation } from "react-i18next";
import { ChevronRight, Loader2, Sparkles } from "lucide-react";
import { cn } from "@/lib/utils";
import { ToolCallBlock } from "@/components/ToolCallBlock";
import type { ToolCallWithResult } from "@/components/ChatWindow";

// ---------------------------------------------------------------------------
// Tool categorisation — maps tool names → a "verb" bucket for the summary
// ---------------------------------------------------------------------------

type Category =
  | "search_web"
  | "search_kb"
  | "read_file"
  | "write_file"
  | "run_code"
  | "create_image"
  | "create_document"
  | "fetch_url"
  | "memory"
  | "spawn"
  | "other";

function categorise(toolName: string): Category {
  const n = (toolName || "").toLowerCase();
  if (!n) return "other";
  if (n.includes("search_web") || n === "web_search" || n === "tavily" || n.includes("websearch"))
    return "search_web";
  if (n.includes("search_knowledge") || n.includes("kb_search") || n.includes("retrieve"))
    return "search_kb";
  if (n === "web_fetch" || n.includes("fetch_url") || n === "url_fetch") return "fetch_url";
  if (n === "fs_read" || n.includes("read_file") || n === "fs_glob" || n === "fs_grep")
    return "read_file";
  if (n === "fs_write" || n.includes("write_file") || n.includes("create_file"))
    return "write_file";
  if (n.includes("execute") || n.includes("run_code") || n.includes("python") || n.includes("bash"))
    return "run_code";
  if (n.includes("image")) return "create_image";
  if (n.includes("document") || n.includes("pptx") || n.includes("docx") || n.includes("generate_doc"))
    return "create_document";
  if (n.includes("memory")) return "memory";
  if (n.includes("spawn") || n.includes("subagent")) return "spawn";
  return "other";
}

/**
 * Build a human-readable summary from a flat list of tool calls.
 * Multiple categories are joined with commas.
 *   [search_web x1]           → "Searched the web"
 *   [search_web, read_file x3] → "Searched the web, read 3 files"
 *   [other x2]                → "Used 2 tools"
 */
function buildSummary(
  tools: ToolCallWithResult[],
  t: ReturnType<typeof useTranslation>["t"]
): string {
  if (!tools.length) return "";

  const buckets = new Map<Category, number>();
  for (const tc of tools) {
    const cat = categorise(tc.toolCall.name || "");
    buckets.set(cat, (buckets.get(cat) ?? 0) + 1);
  }

  const phrases: string[] = [];
  for (const [cat, count] of buckets) {
    // i18next auto-selects plural form from `count`. Default English falls
    // back via the `_other` key in locales; no local fallback needed.
    phrases.push(t(`playground.beat.${cat}`, { count } as Record<string, unknown>));
  }

  // Capitalise first letter of first phrase only, lowercase the rest.
  const joined = phrases.join(", ");
  return joined.charAt(0).toUpperCase() + joined.slice(1);
}

function uniqueToolNames(tools: ToolCallWithResult[]): string[] {
  return Array.from(
    new Set(
      tools
        .map((tc) => tc.toolCall.name?.trim())
        .filter((name): name is string => Boolean(name))
    )
  );
}

// ---------------------------------------------------------------------------
// ElapsedTimer — small live counter shown in the row while streaming
// ---------------------------------------------------------------------------

function ElapsedTimer() {
  const [elapsed, setElapsed] = useState(0);
  const startRef = useRef<number | null>(null);
  useEffect(() => {
    startRef.current = performance.now();
    const id = window.setInterval(() => {
      if (startRef.current != null) {
        setElapsed((performance.now() - startRef.current) / 1000);
      }
    }, 500);
    return () => window.clearInterval(id);
  }, []);
  return (
    <span className="tabular-nums font-mono text-[10px] text-slate-400 dark:text-slate-500">
      {Math.floor(elapsed)}s
    </span>
  );
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

export interface WorkflowBeatProps {
  /** Tool calls accumulated in this beat. Empty = pure thinking beat. */
  toolCalls: ToolCallWithResult[];
  /** True while the assistant is still streaming / tools still running. */
  isRunning: boolean;
  /**
   * True when this message has NO assistant text yet (so we may want to
   * render a "Thinking…" label instead of a tool summary).
   */
  hasNoVisibleText: boolean;
  /** Initial expanded state (default: collapsed). */
  defaultExpanded?: boolean;
}

export function WorkflowBeat({
  toolCalls,
  isRunning,
  hasNoVisibleText,
  defaultExpanded = false,
}: WorkflowBeatProps) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(defaultExpanded);

  const hasTools = toolCalls.length > 0;
  const toolsRunning = toolCalls.some((tc) => tc.toolCall.status === "running");

  // Summary text: tools beat out plain thinking.
  const summary = (() => {
    if (hasTools) return buildSummary(toolCalls, t);
    if (isRunning && hasNoVisibleText)
      return t("playground.beat.thinking", "Thinking…");
    // Finished, no tools, has text → nothing to show
    return "";
  })();

  if (!summary) return null;

  // While running without any tool calls yet, this is effectively a small
  // inline "Thinking…" hint — keep it quiet, don't show an elapsed counter
  // past the beat (it's noisy). Show the counter only when running.
  const showTimer = isRunning && (hasTools || hasNoVisibleText);
  const showSpinner = isRunning && (toolsRunning || !hasTools);
  const toolNames = hasTools ? uniqueToolNames(toolCalls) : [];

  return (
    <div data-message-supplemental="workflow-beat" className="w-full">
      <button
        type="button"
        onClick={() => hasTools && setExpanded((v) => !v)}
        disabled={!hasTools}
        aria-expanded={hasTools ? expanded : undefined}
        className={cn(
          "group flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left",
          "transition-colors duration-150",
          hasTools
            ? "cursor-pointer hover:bg-slate-100/60 dark:hover:bg-white/4"
            : "cursor-default",
          "focus:outline-hidden focus-visible:ring-2 focus-visible:ring-slate-400/30"
        )}
      >
        {/* Left icon — spinner while running, else sparkle */}
        {showSpinner ? (
          <Loader2 className="h-3.5 w-3.5 shrink-0 text-slate-400 dark:text-slate-500 animate-spin" />
        ) : (
          <Sparkles
            className={cn(
              "h-3.5 w-3.5 shrink-0",
              isRunning
                ? "text-slate-400 dark:text-slate-500 animate-pulse"
                : "text-slate-400 dark:text-slate-500"
            )}
          />
        )}

        {/* Summary label — muted, single line, truncates gracefully */}
        <span
          className={cn(
            "flex-1 min-w-0 truncate text-[13px]",
            "text-slate-500 dark:text-slate-400",
            isRunning && "opacity-90"
          )}
        >
          {summary}
        </span>

        {toolNames.length > 0 && (
          <span className="max-w-[45%] shrink-0 truncate rounded-md bg-slate-100 px-1.5 py-0.5 font-mono text-[10px] text-slate-500 dark:bg-white/6 dark:text-slate-400">
            {toolNames.join(", ")}
          </span>
        )}

        {/* Elapsed timer (running only) */}
        {showTimer && <ElapsedTimer />}

        {/* Chevron — only if there's detail to expand to */}
        {hasTools && (
          <motion.div
            animate={{ rotate: expanded ? 90 : 0 }}
            transition={{ duration: 0.15, ease: "easeOut" }}
            className="shrink-0"
          >
            <ChevronRight className="h-3.5 w-3.5 text-slate-400 dark:text-slate-500" />
          </motion.div>
        )}
      </button>

      {/* Expanded detail */}
      <AnimatePresence initial={false}>
        {expanded && hasTools && (
          <motion.div
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.22, ease: [0.25, 0.1, 0.25, 1] }}
            className="overflow-hidden"
          >
            <div className="pl-5 pt-1.5 space-y-2">
              {toolCalls.map((tc, idx) => (
                <ToolCallBlock
                  key={tc.toolCall.tool_call_id || idx}
                  toolCall={tc.toolCall}
                  result={tc.result}
                  argsText={tc.argsText}
                  argsValid={tc.argsValid}
                  stepNumber={toolCalls.length > 1 ? idx + 1 : undefined}
                />
              ))}
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

export default WorkflowBeat;
