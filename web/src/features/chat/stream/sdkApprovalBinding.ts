import type { RequestBody } from "@/api/contracts";
/** Immutable SDK action identity returned by the authenticated approval endpoint. */
export type SdkApprovalBinding = Readonly<Required<Pick<RequestBody<"POST /api/v1/assistant/approvals/{approval_id}">, "expected_run_id" | "expected_tool_call_id" | "expected_arguments_hash">>>;

export function sdkApprovalBinding(
  response: unknown,
  expected: { approvalId: string; runId: string; toolCallId: string; tool: string },
): SdkApprovalBinding | null {
  if (!response || typeof response !== "object") return null;
  const { approval, preview } = response as Record<string, unknown>;
  if (!approval || typeof approval !== "object" || Array.isArray(approval) ||
      !preview || typeof preview !== "object" || Array.isArray(preview)) return null;
  const action = approval as Record<string, unknown>;
  const details = preview as Record<string, unknown>;
  // An unsupported preview cannot authorize execution, but its verified row
  // identity can still bind rejection. Never repair a malformed preview digest
  // or ignore conflicting hashes from the same authenticated response.
  const argumentsHash = details.can_approve === false && details.arguments_hash === undefined
    ? action.arguments_hash : details.arguments_hash;
  if (!expected.runId.trim() || !expected.toolCallId.trim() ||
      action.approval_id !== expected.approvalId || action.run_id !== expected.runId ||
      action.tool_call_id !== expected.toolCallId || action.tool_name !== expected.tool ||
      typeof argumentsHash !== "string" || !/^[a-f0-9]{64}$/.test(argumentsHash) ||
      (action.arguments_hash !== undefined && action.arguments_hash !== argumentsHash)) return null;
  return Object.freeze({ expected_run_id: expected.runId, expected_tool_call_id: expected.toolCallId,
    expected_arguments_hash: argumentsHash });
}

export function sameSdkApprovalBinding(reviewed: SdkApprovalBinding | null, latest: SdkApprovalBinding | null): boolean {
  return reviewed !== null && latest !== null && reviewed.expected_run_id === latest.expected_run_id &&
    reviewed.expected_tool_call_id === latest.expected_tool_call_id &&
    reviewed.expected_arguments_hash === latest.expected_arguments_hash;
}
