import assert from "node:assert/strict";
import { test } from "node:test";
import { buildAgentTurnPayload, buildAgentThreadPayload } from "./agentTurnPayload.ts";
import { assistantModelKey, resolveAssistantModel, savedAssistantModelSelection } from "../pages/assistant/modelSelection.ts";
import type { ModelInfo } from "./assistant.ts";

test("provider choice survives selector, saved conversation, first thread and turn", () => {
  const models = [{ id: "same", provider: "one" }, { id: "same", provider: "two" }] as ModelInfo[];
  const selected = resolveAssistantModel(models, assistantModelKey(models[1]))!;
  const config = { selected_model: selected.id, selected_provider: selected.provider };
  const restored = resolveAssistantModel(models, savedAssistantModelSelection(config))!;
  const request = { provider_id: restored.provider };
  assert.deepEqual(buildAgentThreadPayload("session", restored.id, request.provider_id), {
    session_id: "session", model_id: "same", provider_id: "two",
  });
  const firstTurn = buildAgentTurnPayload("hello", restored.id, "auto", request);
  assert.equal(firstTurn.model_id, "same");
  assert.equal(firstTurn.provider_id, "two");
  // Changing the later UI request never mutates an already constructed turn.
  request.provider_id = "one";
  assert.equal(firstTurn.provider_id, "two");
});

test("legacy API callers omit provider instead of sending an empty override", () => {
  assert.deepEqual(buildAgentThreadPayload("session", "unique"), { session_id: "session", model_id: "unique" });
  assert.equal(Object.hasOwn(buildAgentTurnPayload("hi", "unique"), "provider_id"), false);
  assert.equal(Object.hasOwn(buildAgentTurnPayload("hi", "unique", undefined, { provider_id: "" }), "provider_id"), false);
});
