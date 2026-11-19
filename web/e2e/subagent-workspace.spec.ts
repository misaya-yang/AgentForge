import { expect, test } from "@playwright/test";
import { reduceSubAgentEvent } from "../src/pages/assistant/subagentEventReducer";
import type { SubAgentState } from "../src/pages/assistant/types";

function started(agentId: string, dispatchIndex: number) {
  return {
    agent_id: agentId,
    agent_type: "task",
    description: `Analyze workstream ${dispatchIndex}`,
    profile_id: `reviewer-${dispatchIndex}`,
    profile_name: `Reviewer ${dispatchIndex}`,
    delegation_id: "delegation-real-1",
    dispatch_index: dispatchIndex,
    attempt_id: "attempt-1",
  };
}

test("sub-agent lifecycle reducer survives reconnect duplicates, reordering, and unknown frames", () => {
  let agents: SubAgentState[] = [];
  const untouched = reduceSubAgentEvent(
    agents,
    "subagent_future_event",
    { agent_id: "future-agent", payload: "ignored" },
    1,
  );
  expect(untouched).toBe(agents);

  for (let index = 0; index < 3; index += 1) {
    agents = reduceSubAgentEvent(agents, "subagent_started", started(`agent-${index}`, index), 10 + index);
  }
  // Reconnect replay: a duplicate start is an upsert, not a fourth child.
  agents = reduceSubAgentEvent(agents, "subagent_started", started("agent-1", 1), 50);
  expect(agents).toHaveLength(3);
  expect(agents.find((agent) => agent.agentId === "agent-1")?.startedAtMs).toBe(11);

  // Out-of-order result before start is materialized once; the late start
  // supplies its safe tool label without downgrading the completed result.
  agents = reduceSubAgentEvent(agents, "subagent_tool_result", {
    agent_id: "agent-0",
    call_id: "lookup-1",
    success: true,
    summary: "Verified two authorities",
  }, 60);
  agents = reduceSubAgentEvent(agents, "subagent_tool_start", {
    agent_id: "agent-0",
    call_id: "lookup-1",
    tool_name: "search_primary_sources",
  }, 61);
  agents = reduceSubAgentEvent(agents, "subagent_tool_start", {
    agent_id: "agent-0",
    call_id: "lookup-1",
    tool_name: "search_primary_sources",
  }, 62);
  agents = reduceSubAgentEvent(agents, "subagent_tool_result", {
    agent_id: "agent-0",
    call_id: "lookup-1",
    success: false,
    summary: "conflicting replay",
  }, 63);
  expect(agents[0].steps).toHaveLength(1);
  expect(agents[0].steps[0]).toMatchObject({
    status: "completed",
    toolName: "search_primary_sources",
    summary: "Verified two authorities",
  });
  expect(agents[0].toolCallsMade).toBe(1);

  // Three children terminate in a different order and with distinct outcomes.
  agents = reduceSubAgentEvent(agents, "subagent_finished", {
    agent_id: "agent-2",
    status: "completed",
    duration_ms: 90,
    result_summary: "Completed child result",
    result: {
      evidence: [{ evidence_id: "e-2", tool_name: "filing_lookup", summary: "10-K located" }],
      limitations: [],
      usage: { model_turns: 3, tool_calls: 2 },
      structured_payload: { verdict: "supported" },
    },
  }, 100);
  agents = reduceSubAgentEvent(agents, "subagent_finished", {
    agent_id: "agent-0",
    status: "failed",
    error: "Provider unavailable",
    duration_ms: 110,
    result: { limitations: ["Primary source fetch failed"] },
  }, 110);
  agents = reduceSubAgentEvent(agents, "subagent_finished", {
    agent_id: "agent-1",
    status: "cancelled",
    error: "Cancelled by parent",
    duration_ms: 120,
    result: { limitations: ["Parent run stopped"] },
  }, 120);

  // Replayed/conflicting terminal and late progress cannot rewrite the first
  // accepted terminal receipt or resurrect the child.
  agents = reduceSubAgentEvent(agents, "subagent_finished", {
    agent_id: "agent-2",
    status: "failed",
    error: "replayed conflict",
  }, 130);
  agents = reduceSubAgentEvent(agents, "subagent_step", {
    agent_id: "agent-2",
    step: "Late turn",
    status: "running",
  }, 140);

  expect(agents.map((agent) => agent.status)).toEqual(["failed", "cancelled", "completed"]);
  expect(agents[2].error).toBeUndefined();
  expect(agents[2].resultSummary).toBe("Completed child result");
  expect(agents[2].structuredResult).toEqual({ verdict: "supported" });
  expect(agents[2].evidence?.[0]?.summary).toBe("10-K located");
  expect(agents[2].usage).toEqual({ model_turns: 3, tool_calls: 2 });
});
