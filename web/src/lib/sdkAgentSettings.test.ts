import assert from "node:assert/strict";
import test from "node:test";
import { buildSdkAgentSettingsPatch, readSdkAgentSettings, sdkAgentSettingsMatch, validSdkAgentSettings } from "./sdkAgentSettings.ts";
import type { ServiceDetail } from "../types/gateway.ts";
const service = (framework = "openai_agents") => ({ service_id: "sdk-agent", metadata: { adapter_type: framework,
  sdk_agent: { model: "configured-model", instructions: "Help", max_turns: 8 },
  sdk_tools: { capability_ids: ["approved-tool"], knowledge_dataset_ids: ["approved-data"] },
  sdk_provider_key_configured: true, title: "Keep me" }, connector_config: {} } as unknown as ServiceDetail);

for (const framework of ["openai_agents", "claude_agent_sdk"]) {
  test(`${framework} edit retains framework/tools and never resends saved credentials`, () => {
    const input = service(framework); const saved = readSdkAgentSettings(input, "sdk-agent");
    const patch = buildSdkAgentSettingsPatch(saved, { ...saved.settings, model: "new-model" }, "");
    assert.equal("connector_config" in patch, false);
    assert.deepEqual(patch.metadata.sdk_tools, input.metadata!.sdk_tools);
    assert.equal(patch.metadata.adapter_type, framework);
    assert.equal(patch.metadata.title, "Keep me");
    assert.equal("sdk_provider_key_configured" in patch.metadata, false);
    assert.equal(patch.metadata.sdk_agent.model, "new-model");
    assert.equal(saved.settings.model, "configured-model");
  });
}
test("only explicit valid replacement creates a write-only credential patch", () => {
  const saved = readSdkAgentSettings(service(), "sdk-agent");
  const key = "synthetic-test-key-value";
  const patch = buildSdkAgentSettingsPatch(saved, saved.settings, key);
  assert.deepEqual(patch.connector_config, { sdk_provider_api_key: key });
  for (const key of [" ", "short", "enc:encrypted-value", "has whitespace fixture", "x".repeat(4097)]) {
    assert.throws(() => buildSdkAgentSettingsPatch(saved, saved.settings, key));
  }
});
test("server byte limits and turn range validated before transmission", () => {
  const base = { model: "model", instructions: "Help", max_turns: 8 };
  for (const patch of [{ model: "" }, { model: "猫".repeat(54) }, { instructions: "猫".repeat(10923) }, { max_turns: 0 }, { max_turns: 33 }, { max_turns: 1.5 }]) {
    assert.equal(validSdkAgentSettings({ ...base, ...patch }), false);
  }
  assert.equal(validSdkAgentSettings({ ...base, model: "猫".repeat(53), max_turns: 32 }), true);
});
test("readback is identity checked and blank instructions compare to native normalization", () => {
  assert.throws(() => readSdkAgentSettings(service(), "another"));
  assert.throws(() => readSdkAgentSettings(service("langgraph"), "sdk-agent"));
  const saved = readSdkAgentSettings(service(), "sdk-agent");
  saved.settings.instructions = "You are a helpful assistant.";
  assert.equal(sdkAgentSettingsMatch(saved, { ...saved.settings, instructions: "  " }), true);
  assert.equal(sdkAgentSettingsMatch(saved, { ...saved.settings, max_turns: 7 }), false);
});

test("incompatible raw or ciphertext read projections never enable this editor", () => {
  for (const field of ["sdk_provider_api_key", "sdk_provider_api_key_encrypted"]) {
    const value = service(); value.connector_config = { [field]: "fixture-only" };
    assert.throws(() => readSdkAgentSettings(value, "sdk-agent"));
  }
});
