// Compile-only positive/negative checks against the actual native producer output.
// This module is never imported by the application or executed by Node tests.
import type { createFetchClient, TypedTransport, ApiRequest } from "../../sdk/typescript/generated";
import { queryKey, queryOptions } from "../../sdk/typescript/generated";
import type { ConsoleApiRequest } from "../src/api/generatedTransport";
import type { RequestBody, RequestQuery } from "../src/api/contracts";

export function checkGeneratedContracts(client: ReturnType<typeof createFetchClient>, transport: TypedTransport) {
  void client.send("GET /api/v2/agent/threads/{thread}/events", { path: { thread: "thread" }, query: { after_sequence: 0, limit: 1000, turn_id: "turn" } }, { headers: { "Last-Event-ID": "0" } });
  // @ts-expect-error event page sizes keep the native numeric request contract
  void client.send("GET /api/v2/agent/threads/{thread}/events", { path: { thread: "thread" }, query: { limit: "1000" } });
  void client.request("GET /api/v1/providers", { query: { include_disabled: true } });
  void queryOptions(transport, "tenant/user/generation", "GET /api/v1/providers", { query: { include_disabled: true } });
  void queryKey("tenant/user/generation", "GET /api/v1/users", { query: { page: 1 } });
  // @ts-expect-error unsupported operations must fail before dispatch
  void client.request("POST /api/v1/not-published", {});
  // @ts-expect-error provider identifiers are required by the generated operation
  void client.request("PUT /api/v1/providers/{provider_id}", { body: { display_name: "review" } });
  // @ts-expect-error a read cannot carry an undeclared body
  void client.request("GET /api/v1/providers", { body: {} });
  // @ts-expect-error query factories cannot issue mutations
  void queryOptions(transport, "scope", "POST /api/v1/providers", {});
  // @ts-expect-error query values retain their native primitive types
  void queryKey("scope", "GET /api/v1/users", { query: { page: "all" } });
}
export const validApproval: RequestBody<"POST /api/v1/assistant/approvals/{approval_id}"> = { approved: true };
// @ts-expect-error approval choices are booleans, never string form values
export const invalidApproval: RequestBody<"POST /api/v1/assistant/approvals/{approval_id}"> = { approved: "true" };
// @ts-expect-error required comparison bounds cannot disappear behind optional UI filters
export const missingVersionBounds: RequestQuery<"GET /api/v1/knowledge/{dataset_id}/documents/{document_id}/versions/compare"> = {};
// @ts-expect-error WebAuthn credential is a JSON object rather than an unvalidated string
export const invalidCredential: RequestBody<"POST /api/auth/enterprise/passkeys/authenticate/finish"> = { flow_id: "flow", credential: "not-json" };
// @ts-expect-error generated Console input cannot manufacture browser session authority
export const invalidAuthority: ConsoleApiRequest<"GET /api/v1/providers"> = { header: { Cookie: "session" } };
export const multipart: ApiRequest<"POST /api/v1/files/upload"> = { body: { file: new Blob(["wire bytes"]) } };
