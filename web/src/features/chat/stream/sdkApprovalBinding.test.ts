import assert from "node:assert/strict";
import test from "node:test";
import { sdkApprovalBinding, sameSdkApprovalBinding } from "./sdkApprovalBinding.ts";

const expected = { approvalId: "approval", runId: "run", toolCallId: "call", tool: "create_document" };
const response = () => ({ approval: { approval_id: "approval", run_id: "run", tool_call_id: "call", tool_name: "create_document" },
  preview: { arguments_hash: "a".repeat(64) } });

test("review binding copies and freezes only the complete exact authenticated identity", () => {
  const value = response();
  const binding = sdkApprovalBinding(value, expected)!;
  assert.deepEqual(binding, { expected_run_id: "run", expected_tool_call_id: "call", expected_arguments_hash: "a".repeat(64) });
  assert.equal(Object.isFrozen(binding), true);
  value.preview.arguments_hash = "b".repeat(64);
  assert.equal(binding.expected_arguments_hash, "a".repeat(64));
});

test("binding fails closed on substituted identity and absent or malformed digest", () => {
  for (const field of ["approval_id", "run_id", "tool_call_id", "tool_name"]) {
    const value = response();
    Object.assign(value.approval, { [field]: "other" });
    assert.equal(sdkApprovalBinding(value, expected), null, field);
  }
  for (const hash of [undefined, null, "", "hash", "a".repeat(63), "A".repeat(64), "g".repeat(64), 123]) {
    assert.equal(sdkApprovalBinding({ ...response(), preview: { arguments_hash: hash } }, expected), null);
  }
  for (const value of [null, {}, { approval: {} }, { preview: {} }]) assert.equal(sdkApprovalBinding(value, expected), null);
});

test("both decisions must compare every reviewed field with the new binding", () => {
  const binding = sdkApprovalBinding(response(), expected)!;
  assert.equal(sameSdkApprovalBinding(binding, { ...binding }), true);
  for (const field of Object.keys(binding)) assert.equal(sameSdkApprovalBinding(binding, { ...binding, [field]: "changed" }), false);
  assert.equal(sameSdkApprovalBinding(null, binding), false);
  assert.equal(sameSdkApprovalBinding(binding, null), false);
});

test("explicit unavailable previews retain exact row binding for rejection", () => {
  const value = { approval: { ...response().approval, arguments_hash: "a".repeat(64) },
    preview: { can_approve: false, reason: "Action details could not be verified." } };
  const binding = sdkApprovalBinding(value, expected)!;
  assert.deepEqual(binding, { expected_run_id: "run", expected_tool_call_id: "call", expected_arguments_hash: "a".repeat(64) });
  assert.equal(Object.isFrozen(binding), true);
  value.approval.arguments_hash = "b".repeat(64);
  assert.equal(binding.expected_arguments_hash, "a".repeat(64));
});

test("row fallback never repairs missing previews, malformed digests or contradictory hashes", () => {
  const approval = { ...response().approval, arguments_hash: "a".repeat(64) };
  for (const preview of [undefined, null, [], {}, { can_approve: true }, { can_approve: "false" }]) {
    assert.equal(sdkApprovalBinding({ approval, preview }, expected), null);
  }
  for (const arguments_hash of [null, "", "invalid", "a".repeat(63), "A".repeat(64), 123]) {
    assert.equal(sdkApprovalBinding({ approval, preview: { can_approve: false, arguments_hash } }, expected), null);
    assert.equal(sdkApprovalBinding({ approval: { ...approval, arguments_hash }, preview: { can_approve: false } }, expected), null);
    assert.equal(sdkApprovalBinding({ approval: { ...approval, arguments_hash }, preview: response().preview }, expected), null);
  }
  assert.equal(sdkApprovalBinding({ approval: response().approval, preview: { can_approve: false } }, expected), null);
  assert.equal(sdkApprovalBinding({ approval, preview: { can_approve: false, arguments_hash: "b".repeat(64) } }, expected), null);
  assert.equal(sdkApprovalBinding({ approval, preview: { can_approve: true, arguments_hash: "b".repeat(64) } }, expected), null);
  assert.deepEqual(sdkApprovalBinding({ approval, preview: response().preview }, expected), sdkApprovalBinding(response(), expected));
  for (const field of ["approval_id", "run_id", "tool_call_id", "tool_name"]) {
    assert.equal(sdkApprovalBinding({ approval: { ...approval, [field]: "other" }, preview: { can_approve: false } }, expected), null, field);
  }
});
