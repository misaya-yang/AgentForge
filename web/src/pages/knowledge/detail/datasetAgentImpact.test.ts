// @ts-expect-error -- node built-ins are supplied by the node --test runtime.
import assert from "node:assert/strict";
// @ts-expect-error -- node built-ins are supplied by the node --test runtime.
import { test } from "node:test";

import { canConfirmDatasetImpactDelete, isDatasetAgentImpact } from "./datasetAgentImpact.ts";

const impact = {
  dataset_id: "kb-a",
  visible_agents: [{
    agent_id: "agent-1", name: "Assistant", current_draft: true,
    active_publication: false, historical_version_count: 2,
  }],
  hidden_agent_count: 1,
  counts: {
    current_draft: { visible: 1, hidden: 0 },
    active_publication: { visible: 0, hidden: 1 },
    historical_version: { visible: 1, hidden: 1 },
  },
};

test("only a complete, matching owner preflight can enable deletion", () => {
  assert.equal(isDatasetAgentImpact(impact, "kb-a"), true);
  assert.equal(isDatasetAgentImpact(impact, "kb-b"), false);
  assert.equal(isDatasetAgentImpact({ ...impact, counts: undefined }, "kb-a"), false);
  assert.equal(isDatasetAgentImpact({ ...impact, hidden_agent_count: undefined }, "kb-a"), false);
  assert.equal(isDatasetAgentImpact({ ...impact, counts: {
    ...impact.counts, current_draft: { visible: 0, hidden: 0 },
  } }, "kb-a"), false);
  assert.equal(canConfirmDatasetImpactDelete("owner", "kb-a", impact), true);
  assert.equal(canConfirmDatasetImpactDelete("editor", "kb-a", impact), false);
  assert.equal(canConfirmDatasetImpactDelete("owner", "kb-a", null), false);
  assert.equal(canConfirmDatasetImpactDelete("owner", "kb-b", impact), false);
});
