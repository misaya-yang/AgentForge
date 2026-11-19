/**
 * Sub-agent workbench drawer.
 *
 * This is a lifecycle/receipt surface, not a reasoning transcript. It groups
 * children by host-observed state and visualizes parallel fan-in without
 * claiming durable recovery or controls the backend does not provide.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { GitMerge, Network, X } from "lucide-react";
import { cn } from "@/lib/utils";
import type { ChatMessage, SubAgentState, SubAgentStatus } from "../types";
import { SubAgentCard } from "./SubAgentCard";
import { ensureActivityStyles } from "./activityTheme";

interface SubAgentWorkspacePanelProps {
  open: boolean;
  onClose: () => void;
  message: ChatMessage | null;
  width?: number;
  className?: string;
}

const TERMINAL = new Set<SubAgentStatus>([
  "completed",
  "failed",
  "cancelled",
  "blocked",
  "partial",
]);

const SEGMENT_COLOR: Record<SubAgentStatus, string> = {
  running: "bg-sky-500",
  completed: "bg-emerald-500",
  failed: "bg-red-500",
  cancelled: "bg-slate-500",
  blocked: "bg-amber-500",
  partial: "bg-orange-500",
};

const EMPTY_SUBAGENTS: SubAgentState[] = [];

type DelegationGroup = {
  id: string;
  agents: SubAgentState[];
};

function ordered(agents: SubAgentState[]): SubAgentState[] {
  return [...agents].sort((left, right) => {
    const leftIndex = left.dispatchIndex ?? Number.MAX_SAFE_INTEGER;
    const rightIndex = right.dispatchIndex ?? Number.MAX_SAFE_INTEGER;
    if (leftIndex !== rightIndex) return leftIndex - rightIndex;
    return (left.startedAtMs ?? 0) - (right.startedAtMs ?? 0);
  });
}

function groupDelegations(agents: SubAgentState[]): DelegationGroup[] {
  const groups = new Map<string, SubAgentState[]>();
  for (const agent of agents) {
    const key = agent.delegationId
      || (agent.dispatchIndex === undefined ? `single:${agent.agentId}` : "legacy-batch");
    groups.set(key, [...(groups.get(key) ?? []), agent]);
  }
  return [...groups.entries()].map(([id, values]) => ({ id, agents: ordered(values) }));
}

function ParallelFanIn({ group }: { group: DelegationGroup }) {
  if (group.agents.length < 2) return null;
  const terminalCount = group.agents.filter((agent) => TERMINAL.has(agent.status)).length;
  const fanInReady = terminalCount === group.agents.length;

  return (
    <div
      className="rounded-lg border border-[hsl(var(--assistant-border-soft))] bg-[hsl(var(--assistant-surface-soft))] px-3 py-2.5"
      aria-label={`Parallel delegation: ${terminalCount} of ${group.agents.length} child terminals received`}
    >
      <div className="flex items-center gap-2">
        <GitMerge className="h-3.5 w-3.5 text-[hsl(var(--assistant-accent))]" aria-hidden="true" />
        <span className="text-[11px] font-semibold" style={{ color: "hsl(var(--assistant-text-primary))" }}>
          Parallel delegation
        </span>
        <span className="ml-auto font-mono text-[10px] tabular-nums" style={{ color: "hsl(var(--assistant-text-secondary))" }}>
          {terminalCount}/{group.agents.length} terminal
        </span>
      </div>
      <div className="mt-2 flex h-1.5 gap-1" aria-hidden="true">
        {group.agents.map((agent) => (
          <span
            key={agent.agentId}
            className={cn("min-w-0 flex-1 rounded-full", SEGMENT_COLOR[agent.status])}
          />
        ))}
      </div>
      <div className="mt-1.5 text-[10px]" style={{ color: "hsl(var(--assistant-text-secondary))" }}>
        {fanInReady
          ? "All child terminals received; parent synthesis can continue."
          : `Fan-in waiting for ${group.agents.length - terminalCount} child ${group.agents.length - terminalCount === 1 ? "terminal" : "terminals"}.`}
      </div>
    </div>
  );
}

function Section({
  title,
  agents,
  nowMs,
  emptyLabel,
}: {
  title: string;
  agents: SubAgentState[];
  nowMs: number;
  emptyLabel: string;
}) {
  return (
    <section aria-labelledby={`subagent-section-${title.toLowerCase()}`}>
      <div className="mb-1.5 flex items-center gap-2 px-2">
        <h3
          id={`subagent-section-${title.toLowerCase()}`}
          className="text-[11px] font-semibold uppercase tracking-[0.12em]"
          style={{ color: "hsl(var(--assistant-text-secondary))" }}
        >
          {title}
        </h3>
        <span className="font-mono text-[10px] tabular-nums" style={{ color: "hsl(var(--assistant-text-secondary))" }}>
          {agents.length}
        </span>
      </div>
      {agents.length > 0 ? (
        <div>
          {agents.map((agent) => (
            <SubAgentCard key={agent.agentId} subAgent={agent} nowMs={nowMs} />
          ))}
        </div>
      ) : (
        <p className="px-2 py-3 text-[12px]" style={{ color: "hsl(var(--assistant-text-secondary))" }}>
          {emptyLabel}
        </p>
      )}
    </section>
  );
}

export function SubAgentWorkspacePanel({
  open,
  onClose,
  message,
  width = 420,
  className,
}: SubAgentWorkspacePanelProps) {
  ensureActivityStyles();
  const agents = message?.activeSubAgents ?? EMPTY_SUBAGENTS;
  const hasRunningAgent = agents.some((agent) => agent.status === "running");
  const [nowMs, setNowMs] = useState(() => Date.now());
  const closeButtonRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!open) return undefined;
    const previousFocus = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
    const focusFrame = window.requestAnimationFrame(() => closeButtonRef.current?.focus());
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      onClose();
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      window.cancelAnimationFrame(focusFrame);
      document.removeEventListener("keydown", handleKeyDown);
      if (previousFocus?.isConnected) previousFocus.focus();
    };
  }, [onClose, open]);

  useEffect(() => {
    if (!open || !hasRunningAgent) return undefined;
    const timer = window.setInterval(() => setNowMs(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [hasRunningAgent, open]);

  const { active, completed, delegations } = useMemo(() => ({
    active: ordered(agents.filter((agent) => agent.status === "running")),
    completed: [...agents]
      .filter((agent) => agent.status !== "running")
      .sort((left, right) => (right.finishedAtMs ?? 0) - (left.finishedAtMs ?? 0)),
    delegations: groupDelegations(agents),
  }), [agents]);

  const subtitle = `${active.length} active · ${completed.length} completed`;

  return (
    <aside
      aria-hidden={!open}
      aria-label="Sub-agent workbench"
      className={cn(
        "flex h-full shrink-0 flex-col border-l border-[hsl(var(--assistant-border))] bg-[hsl(var(--assistant-canvas-bg))] font-assistant",
        className,
      )}
      style={{ width }}
      data-testid="subagent-workbench"
    >
      <header className="flex shrink-0 items-center gap-2.5 border-b border-[hsl(var(--assistant-border-soft))] px-4 py-3.5">
        <span className="flex h-7 w-7 items-center justify-center rounded-lg bg-[hsl(var(--assistant-accent-soft))] text-[hsl(var(--assistant-accent))]">
          <Network className="h-4 w-4" aria-hidden="true" />
        </span>
        <span className="min-w-0 flex-1">
          <span className="block text-[13px] font-semibold" style={{ color: "hsl(var(--assistant-text-primary))" }}>
            Sub-agents
          </span>
          <span className="mt-0.5 block font-mono text-[10px] tabular-nums" style={{ color: "hsl(var(--assistant-text-secondary))" }}>
            {subtitle}
          </span>
        </span>
        <button
          ref={closeButtonRef}
          type="button"
          onClick={onClose}
          aria-label="Close sub-agent workbench"
          className="act-btn act-hover flex h-7 w-7 items-center justify-center rounded-md focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[hsl(var(--assistant-accent))]"
          style={{ color: "hsl(var(--assistant-text-secondary))" }}
        >
          <X className="h-4 w-4" aria-hidden="true" />
        </button>
      </header>

      <div className="act-scroll flex-1 space-y-5 overflow-y-auto px-3 py-4">
        {delegations.map((group) => <ParallelFanIn key={group.id} group={group} />)}
        <Section
          title="Active"
          agents={active}
          nowMs={nowMs}
          emptyLabel="No active child agents."
        />
        <Section
          title="Completed"
          agents={completed}
          nowMs={nowMs}
          emptyLabel="Completed child receipts will appear here."
        />
      </div>

      <footer className="shrink-0 border-t border-[hsl(var(--assistant-border-soft))] px-4 py-2.5 text-[10px] leading-relaxed" style={{ color: "hsl(var(--assistant-text-secondary))" }}>
        Child steering and per-agent cancellation are not available in this runtime. Parent stream stop remains available in chat.
      </footer>
    </aside>
  );
}
