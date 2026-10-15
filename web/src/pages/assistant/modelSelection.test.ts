import assert from "node:assert/strict";
import { test } from "node:test";

import { activeAssistantModels, assistantModelKey, preferredAssistantModelId, resolveAssistantModel, savedAssistantModelSelection } from "./modelSelection.ts";
import type { ModelInfo } from "../../api/assistant.ts";

const models = [
  { id: "claude", provider: "anthropic" },
  { id: "qwen3.7-plus", provider: "dashscope" },
  { id: "qwen3.8-flash", provider: "dashscope" },
] as ModelInfo[];

test("assistant offers only models from configured providers", () => {
  assert.deepEqual(activeAssistantModels(models, ["dashscope"]).map((model) => model.id), [
    "qwen3.7-plus", "qwen3.8-flash",
  ]);
  assert.deepEqual(activeAssistantModels(models, []), []);
});

test("Qwen 3.8 Flash is the new default while explicit available choices survive", () => {
  const active = activeAssistantModels(models, ["dashscope"]);
  assert.equal(preferredAssistantModelId(active, "qwen3.7-plus"), assistantModelKey(models[2]));
  assert.equal(preferredAssistantModelId(active, "qwen3.7-plus", "qwen3.7-plus"), assistantModelKey(models[1]));
  assert.equal(preferredAssistantModelId(active, "qwen3.7-plus", "claude"), assistantModelKey(models[2]));
  assert.equal(preferredAssistantModelId([], "qwen3.8-flash"), "");
});

const duplicates = [
  { id: "shared-model", name: "First", provider: "provider-a", supports_vision: false, capability_revision: 3 },
  { id: "shared-model", name: "Second", provider: "provider-b", supports_vision: true, capability_revision: 9 },
] as ModelInfo[];

test("same model ID keeps provider-specific capability and selection identity", () => {
  const [first, second] = duplicates.map(assistantModelKey);
  assert.notEqual(first, second);
  assert.equal(resolveAssistantModel(duplicates, first), duplicates[0]);
  assert.equal(resolveAssistantModel(duplicates, second), duplicates[1]);
  assert.equal(resolveAssistantModel(duplicates, second)?.supports_vision, true);
  assert.equal(resolveAssistantModel(duplicates, first)?.supports_vision, false);
  assert.equal(resolveAssistantModel(duplicates, "shared-model"), undefined);
});

test("legacy model-only choices resolve only with one available provider", () => {
  assert.equal(resolveAssistantModel([duplicates[1]], "shared-model"), duplicates[1]);
  assert.equal(resolveAssistantModel(duplicates, "shared-model"), undefined);
  assert.equal(resolveAssistantModel([], "shared-model"), undefined);
});

test("saved provider identity never falls back to a same-named replacement", () => {
  const saved = savedAssistantModelSelection({ selected_model: "shared-model", selected_provider: "provider-b" });
  assert.equal(resolveAssistantModel(duplicates, saved), duplicates[1]);
  assert.equal(resolveAssistantModel([duplicates[0]], saved), undefined);
  assert.equal(savedAssistantModelSelection({ selected_model: "shared-model" }), "shared-model");
  assert.equal(savedAssistantModelSelection(null), "");
});

test("defaults and persisted choices produce a provider-bound selection", () => {
  const saved = assistantModelKey(duplicates[1]);
  assert.equal(preferredAssistantModelId(duplicates, "shared-model", saved), saved);
  assert.equal(preferredAssistantModelId(duplicates, "shared-model"), assistantModelKey(duplicates[0]));
  assert.equal(preferredAssistantModelId([duplicates[1]], "shared-model", "shared-model"), saved);
});

test("selection encoding preserves slashes, quotes and delimiters", () => {
  const unusual = { id: 'org/model::["x"]', provider: 'provider::"x"' } as ModelInfo;
  assert.equal(resolveAssistantModel([unusual], assistantModelKey(unusual)), unusual);
});
