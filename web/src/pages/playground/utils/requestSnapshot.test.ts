import assert from "node:assert/strict";
import test from "node:test";
import { captureRequestSnapshot, persistedRequestSnapshot, restoreRequestSnapshot, serializeRequestSnapshot } from "./requestSnapshot.ts";

const capture = () => captureRequestSnapshot({
  attempt_id: "attempt-1", protocol: "gateway", service_id: "service", session_id: "session", memory_enabled: false,
  body: { service_id: "service", inputs: [{ type: "text", data: "User supplied sensitive text" }],
    parameters: { conversation_id: "conversation-1", api_key: "connector-secret" },
    Authorization: "Bearer fixture-secret", upstream_url: "https://provider.invalid?key=fixture-secret" },
})!;

test("allowlist drops transport credentials, provider URLs, arbitrary metadata, and duplicate persisted prompt", () => {
  const snapshot = capture();
  const encoded = serializeRequestSnapshot(snapshot);
  assert.ok(encoded.includes("User supplied sensitive text"));
  for (const secret of ["connector-secret", "fixture-secret", "Authorization", "upstream_url"]) assert.equal(encoded.includes(secret), false);
  const stored = persistedRequestSnapshot(snapshot)!;
  assert.equal(stored.input_text, undefined); assert.equal(stored.memory_enabled, false);
  const restored = restoreRequestSnapshot({ ...stored, input_text: "injected", headers: { Authorization: "secret" }, metadata: { key: "secret" } }, "service", "session")!;
  assert.equal(JSON.stringify(restored).includes("secret"), false); assert.equal(restored.input_text, undefined);
});

test("invalid versions, binding, identity URLs and malformed fields fail closed", () => {
  const snapshot = persistedRequestSnapshot(capture())!;
  for (const patch of [{ version: 2 }, { memory_enabled: "false" }, { service_id: "other" }, { session_id: "other" },
    { attempt_id: "https://provider.invalid?key=secret" }, { captured_at: "not-a-date" }, { protocol: "custom-proxy" }]) {
    assert.equal(restoreRequestSnapshot({ ...snapshot, ...patch }, "service", "session"), undefined);
  }
  const poisoned = restoreRequestSnapshot({ ...snapshot, conversation_id: "https://provider.invalid?key=secret", api_key: "secret" }, "service", "session")!;
  assert.equal(poisoned.conversation_id, undefined); assert.equal(JSON.stringify(poisoned).includes("secret"), false);
});

test("snapshot clones exact supported settings and never reads later mutated wire values", () => {
  const body = { input: { messages: [{ role: "user", content: "Original" }] }, stream_mode: ["messages-tuple", "updates", "custom"], durability: "exit" };
  const snapshot = captureRequestSnapshot({ attempt_id: "attempt-1", protocol: "langgraph", service_id: "service", memory_enabled: true, thread_id: "thread", body })!;
  body.input.messages[0].content = "Changed"; body.stream_mode[0] = "Changed";
  assert.equal(snapshot.input_text, "Original"); assert.equal(snapshot.stream_mode?.[0], "messages-tuple");
  assert.equal(snapshot.thread_id, "thread"); assert.equal(snapshot.durability, "exit");
});
