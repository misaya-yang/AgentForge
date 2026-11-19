import assert from "node:assert/strict";
import test from "node:test";

import { agentVersionTarget, buildExperimentTargetConfig } from "./agentVersionTarget.ts";

test("experiment JSON model is never replaced by the trace filter or a fallback model", () => {
  assert.deepEqual(buildExperimentTargetConfig(
    { model_id: "custom-model", trace_family: "assistant" }, "rag",
  ), { trace_family: "assistant", model_id: "custom-model" });
  assert.deepEqual(buildExperimentTargetConfig({}, "assistant"), { trace_family: "assistant" });
});

test("typed Agent Version readback exposes only server-declared identity and scope", () => {
  assert.deepEqual(agentVersionTarget({
    candidate_type: "agent_version",
    agent_id: "agent-a",
    agent_version_id: "version-b",
    agent_spec_hash: "sha256:spec",
    model_id: "resolved-model",
    knowledge_dataset_ids: ["kb-a", "kb-b"],
    secret: "ignored",
  }), {
    candidate_type: "agent_version",
    agent_id: "agent-a",
    agent_version_id: "version-b",
    agent_spec_hash: "sha256:spec",
    agent_runtime_snapshot_hash: undefined,
    model_id: "resolved-model",
    knowledge_dataset_ids: ["kb-a", "kb-b"],
  });
  assert.equal(agentVersionTarget({ candidate_type: "agent_version", agent_id: "agent-a" }), null);
});
