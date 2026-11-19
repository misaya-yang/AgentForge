/**
 * Compact workbench row for one delegated agent.
 *
 * The collapsed row deliberately exposes only host-owned lifecycle metadata:
 * profile, safe tool/step label, status, and elapsed time. Expanding the row
 * reveals the assigned summary and terminal receipt, never chain-of-thought or
 * raw tool arguments.
 */
import { useId, useState } from "react";
import {
  Bot,
  ChevronDown,
  CircleAlert,
  CircleCheck,
  CircleStop,
  Clock3,
  ShieldQuestion,
} from "lucide-react";
import { formatDuration } from "@/lib/format";
import { cn } from "@/lib/utils";
import { safeSubAgentText } from "../subagentSafety";
import type { SubAgentState, SubAgentStatus } from "../types";

const GLYPH_COLORS = [
  "text-violet-500 bg-violet-500/12",
  "text-cyan-500 bg-cyan-500/12",
  "text-emerald-500 bg-emerald-500/12",
  "text-amber-500 bg-amber-500/12",
  "text-rose-500 bg-rose-500/12",
] as const;

const STATUS_META: Record<SubAgentStatus, {
  label: string;
  text: string;
  icon: typeof CircleCheck;
}> = {
  running: { label: "Running", text: "", icon: Clock3 },
  completed: { label: "Completed", text: "", icon: CircleCheck },
  failed: { label: "Failed", text: "", icon: CircleAlert },
  cancelled: { label: "Cancelled", text: "", icon: CircleStop },
  blocked: { label: "Blocked", text: "", icon: ShieldQuestion },
  partial: { label: "Partial", text: "", icon: CircleAlert },
};

function safeToolLabel(toolName: string): string {
  const words = safeSubAgentText(toolName, 120)
    .replace(/[^A-Za-z0-9\u4e00-\u9fff]+/g, " ")
    .trim()
    .slice(0, 80);
  return words || "external tool";
}

function hashIndex(value: string): number {
  let hash = 0;
  for (let index = 0; index < value.length; index += 1) {
    hash = ((hash << 5) - hash + value.charCodeAt(index)) | 0;
  }
  return Math.abs(hash) % GLYPH_COLORS.length;
}

function usageLabel(key: string): string {
  return key.replace(/_/g, " ");
}

function currentAction(subAgent: SubAgentState): string {
  const runningTool = [...subAgent.steps].reverse().find((step) => step.status === "running");
  if (runningTool) return `Using ${safeToolLabel(runningTool.toolName)}`;
  const step = safeSubAgentText(subAgent.currentStep, 120);
  if (subAgent.status === "running" && step) return step;
  if (subAgent.status === "blocked") return "Waiting for operator approval";
  if (subAgent.status === "cancelled") return "Cancelled by parent run";
  if (subAgent.status === "failed") return "Execution failed";
  if (subAgent.status === "partial") return "Completed with limitations";
  if (subAgent.status === "completed") return "Result delivered to parent";
  return step || "Waiting for progress";
}

function elapsedLabel(subAgent: SubAgentState, nowMs: number): string {
  const elapsed = subAgent.durationMs
    ?? (subAgent.startedAtMs ? Math.max(0, nowMs - subAgent.startedAtMs) : undefined);
  return elapsed === undefined ? "—" : formatDuration(elapsed);
}

export function SubAgentCard({
  subAgent,
  nowMs,
}: {
  subAgent: SubAgentState;
  nowMs: number;
}) {
  const [expanded, setExpanded] = useState(false);
  const detailsId = useId();
  const meta = STATUS_META[subAgent.status];
  const StatusIcon = meta.icon;
  const profileName = safeSubAgentText(
    subAgent.profileName || subAgent.profileId || `${subAgent.agentType} agent`,
    100,
  );
  const finishedTime = subAgent.finishedAtMs
    ? new Date(subAgent.finishedAtMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : undefined;
  const hasStructuredResult = subAgent.structuredResult !== undefined
    && subAgent.structuredResult !== null;

  return (
    <article
      className="border-b border-[hsl(var(--assistant-border-soft))] last:border-b-0"
      data-subagent-id={subAgent.agentId}
      data-status={subAgent.status}
    >
      <button
        type="button"
        onClick={() => setExpanded((current) => !current)}
        aria-expanded={expanded}
        aria-controls={detailsId}
        className="group flex w-full items-center gap-3 rounded-lg px-2 py-3 text-left hover:bg-[hsl(var(--assistant-surface-soft))] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[hsl(var(--assistant-accent))]"
      >
        <span
          className={cn(
            "flex h-8 w-8 shrink-0 items-center justify-center rounded-lg",
            GLYPH_COLORS[hashIndex(subAgent.agentId)],
          )}
          aria-hidden="true"
        >
          <Bot className="h-[18px] w-[18px]" strokeWidth={1.8} />
        </span>
        <span className="min-w-0 flex-1">
          <span className="flex min-w-0 items-center gap-2">
            <span className="truncate text-[13px] font-semibold" style={{ color: "hsl(var(--assistant-text-primary))" }}>
              {profileName}
            </span>
          </span>
          <span className="mt-0.5 block truncate text-[12px]" style={{ color: "hsl(var(--assistant-text-secondary))" }}>
            {currentAction(subAgent)}
          </span>
        </span>
        <span className="shrink-0 text-right">
          <span className="block font-mono text-[11px] tabular-nums" style={{ color: "hsl(var(--assistant-text-secondary))" }}>
            {elapsedLabel(subAgent, nowMs)}
          </span>
          <span
            className={cn("mt-0.5 flex items-center justify-end gap-1 text-[10px]", meta.text)}
            style={{ color: "hsl(var(--assistant-text-primary))" }}
            aria-label={`Status: ${meta.label}`}
          >
            <StatusIcon className={cn("h-3 w-3", subAgent.status === "running" && "animate-pulse")} />
            {meta.label}
          </span>
        </span>
        <ChevronDown
          className={cn(
            "h-3.5 w-3.5 shrink-0 transition-transform",
            expanded && "rotate-180",
          )}
          style={{ color: "hsl(var(--assistant-text-secondary))" }}
          aria-hidden="true"
        />
      </button>

      {expanded ? (
        <div
          id={detailsId}
          className="mb-3 ml-11 space-y-3 border-l border-[hsl(var(--assistant-border))] pl-3 pr-2 text-[12px]"
        >
          <section>
            <h4 className="font-semibold" style={{ color: "hsl(var(--assistant-text-primary))" }}>Assigned task</h4>
            <p className="mt-1 leading-relaxed" style={{ color: "hsl(var(--assistant-text-secondary))" }}>
              {safeSubAgentText(subAgent.description, 500) || "No host-provided task summary."}
            </p>
          </section>

          {subAgent.error ? (
            <div className="rounded-md border border-red-500/25 bg-red-500/8 px-2.5 py-2 text-red-700 dark:text-red-300" role="alert">
              {safeSubAgentText(subAgent.error, 800)}
            </div>
          ) : null}

          {subAgent.resultSummary ? (
            <section>
              <h4 className="font-semibold" style={{ color: "hsl(var(--assistant-text-primary))" }}>Result</h4>
              <p className="mt-1 whitespace-pre-wrap break-words text-[12px] leading-relaxed" style={{ color: "hsl(var(--assistant-text-secondary))" }}>
                {safeSubAgentText(subAgent.resultSummary, 4_000)}
              </p>
            </section>
          ) : null}

          {subAgent.evidence && subAgent.evidence.length > 0 ? (
            <section>
              <h4 className="font-semibold" style={{ color: "hsl(var(--assistant-text-primary))" }}>Evidence</h4>
              <ul className="mt-1 space-y-1.5">
                {subAgent.evidence.map((evidence, index) => (
                  <li key={evidence.evidenceId || evidence.callId || index} className="rounded-md bg-[hsl(var(--assistant-surface-soft))] px-2.5 py-2">
                    <span className="font-mono text-[10px]" style={{ color: "hsl(var(--assistant-text-secondary))" }}>
                      {safeToolLabel(evidence.toolName || "observed evidence")}
                    </span>
                    {evidence.summary ? (
                      <span className="mt-0.5 block" style={{ color: "hsl(var(--assistant-text-secondary))" }}>
                        {safeSubAgentText(evidence.summary, 500)}
                      </span>
                    ) : null}
                  </li>
                ))}
              </ul>
            </section>
          ) : null}

          {subAgent.limitations && subAgent.limitations.length > 0 ? (
            <section>
              <h4 className="font-semibold text-amber-800 dark:text-amber-300">Limitations</h4>
              <ul className="mt-1 list-disc space-y-1 pl-4" style={{ color: "hsl(var(--assistant-text-secondary))" }}>
                {subAgent.limitations.map((limitation, index) => (
                  <li key={`${index}-${limitation.slice(0, 24)}`}>{safeSubAgentText(limitation, 500)}</li>
                ))}
              </ul>
            </section>
          ) : null}

          {hasStructuredResult ? (
            <details>
              <summary className="cursor-pointer font-semibold" style={{ color: "hsl(var(--assistant-text-primary))" }}>
                Structured result
              </summary>
              <pre className="mt-1 max-h-48 overflow-auto whitespace-pre-wrap break-words rounded-md bg-[hsl(var(--assistant-surface-soft))] p-2 font-mono text-[10px]" style={{ color: "hsl(var(--assistant-text-secondary))" }}>
                {safeSubAgentText(JSON.stringify(subAgent.structuredResult, null, 2), 12_000)}
              </pre>
            </details>
          ) : null}

          <section className="flex flex-wrap gap-x-3 gap-y-1 border-t border-[hsl(var(--assistant-border-soft))] pt-2 font-mono text-[10px]" style={{ color: "hsl(var(--assistant-text-secondary))" }}>
            {Object.entries(subAgent.usage ?? {}).map(([key, value]) => (
              <span key={key}>{usageLabel(key)} {Math.round(value)}</span>
            ))}
            {subAgent.effectiveExecution?.modelId ? (
              <span>model {safeSubAgentText(subAgent.effectiveExecution.modelId, 80)}</span>
            ) : null}
            {subAgent.sourcePlugin ? (
              <span>source {safeSubAgentText(subAgent.sourcePlugin, 80)}</span>
            ) : null}
            {finishedTime ? <span>finished {finishedTime}</span> : null}
          </section>
        </div>
      ) : null}
    </article>
  );
}
