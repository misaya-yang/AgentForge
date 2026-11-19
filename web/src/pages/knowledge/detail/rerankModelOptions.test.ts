import assert from "node:assert/strict";
import test from "node:test";

import { getDefaultRerankModel, getRerankModelOptions } from "./rerankModelOptions.ts";

test("rerank choices use dataset config and server presets without duplicates", () => {
  const presets = [
    { config: { rerank: { enabled: false, provider: "dashscope", model: "qwen3-rerank" } } },
    { config: { rerank: { enabled: true, provider: "dashscope", model: "qwen3-rerank" } } },
    { config: { rerank: { enabled: true, provider: "cohere", model: "cohere-rerank-v3.5" } } },
  ];

  assert.deepEqual(
    getRerankModelOptions(" qwen3-rerank ", "dashscope", presets),
    ["qwen3-rerank"]
  );
  assert.equal(getDefaultRerankModel("qwen3-rerank", "dashscope", presets), "qwen3-rerank");
  assert.equal(getDefaultRerankModel("", "dashscope", presets), "qwen3-rerank");
  assert.deepEqual(getRerankModelOptions("", "cohere", presets), ["cohere-rerank-v3.5"]);
});

test("unconfigured rerank has no invented client default", () => {
  const presets = [{ config: { rerank: { enabled: true, provider: "dashscope", model: "qwen3-rerank" } } }];
  assert.deepEqual(getRerankModelOptions(null, null, presets), []);
  assert.equal(getDefaultRerankModel(undefined, null, presets), "");
});

test("malformed and blank platform model values are ignored", () => {
  const presets = [
    { config: { rerank: { enabled: true, model: "   " } } },
    { config: { rerank: { enabled: true, model: 123 } } },
  ];
  assert.deepEqual(getRerankModelOptions(" ", "dashscope", presets), []);
  assert.equal(getDefaultRerankModel("", "dashscope", presets), "");
});
