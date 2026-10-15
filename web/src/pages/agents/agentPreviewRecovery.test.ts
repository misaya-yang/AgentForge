import assert from "node:assert/strict";
import test from "node:test";

import {
  downloadPreviewArtifact,
  latestPreviewRunId,
  pollPreviewOutcome,
  previewArtifactCanDownload,
  previewCanStartCurrentDraft,
  previewHistoryMessages,
  previewLocator,
  previewRunIsTerminal,
  previewSessionActionsBlocked,
  readPreviewLocator,
} from "./agentPreviewRecovery.ts";

test("only an isolated preview locator for this agent can be restored", () => {
  const session = previewLocator({
    session_id: "session-preview",
    agent_id: "agent-one",
    agent_version_id: null,
    draft_revision: 2,
    publication_id: null,
    channel: "preview",
    runtime_fingerprint: "sha256:private-runtime-fingerprint",
    request_id: "request-one",
  });
  const stored = JSON.stringify(session);
  assert.deepEqual(readPreviewLocator(stored, "agent-one"), session);
  assert.equal(readPreviewLocator(stored, "agent-two"), null);
  assert.equal(readPreviewLocator(JSON.stringify({ ...session, channel: "hosted" }), "agent-one"), null);
  assert.equal(readPreviewLocator(JSON.stringify({ ...session, draft_revision: null }), "agent-one"), null);
  assert.equal(readPreviewLocator("not-json", "agent-one"), null);
  assert.equal(stored.includes("private-runtime-fingerprint"), false);
});

test("preview recovery takes only the latest server-recorded run identity", () => {
  assert.equal(latestPreviewRunId([
    { role: "user", content: "first", metadata: { runtime_run_id: "run-one" } },
    { role: "assistant", content: "done", metadata: { runtime_run_id: "run-one" } },
    { role: "user", content: "second", metadata: { runtime_run_id: "run-two" } },
  ]), "run-two");
  assert.equal(latestPreviewRunId([{ role: "user", content: "not dispatched" }]), null);
});

test("active and approval runs block new, clear, and target switching until terminal history is visible", () => {
  const active = {
    restoring: false, starting: false, sending: false, runUnsettled: true,
    decidingApproval: false, cancelling: false, refreshingRun: false,
  };
  assert.equal(previewSessionActionsBlocked(active), true);
  assert.equal(previewSessionActionsBlocked({ ...active, decidingApproval: true }), true);
  assert.equal(previewRunIsTerminal("awaiting_approval"), false);
  assert.equal(previewRunIsTerminal("succeeded"), true);
  const finalMessages = previewHistoryMessages([
    { role: "user", content: "question", metadata: { runtime_run_id: "run-one" } },
    { role: "assistant", content: "final answer", metadata: { runtime_run_id: "run-one" } },
  ]);
  assert.equal(finalMessages[1].content, "final answer");
  assert.equal(previewSessionActionsBlocked({ ...active, runUnsettled: false }), false);
});

test("a saved r2 can start an isolated Preview while a restored r1 run remains active", () => {
  const state = {
    target: "draft:1", pinnedDraftRevision: 1, currentDraftRevision: 2,
    restoring: false, starting: false, sending: false, decidingApproval: false,
    cancelling: false, refreshingRun: false, approvalPending: false,
  };
  assert.equal(previewCanStartCurrentDraft(state), true);
  assert.equal(previewCanStartCurrentDraft({ ...state, target: "draft" }), false);
  assert.equal(previewCanStartCurrentDraft({ ...state, currentDraftRevision: 1 }), false);
  assert.equal(previewCanStartCurrentDraft({ ...state, approvalPending: true }), false);
  assert.equal(previewCanStartCurrentDraft({ ...state, sending: true }), false);
});

test("a rejected approval keeps the Preview blocked until the rejection reaches a terminal run", () => {
  const state = {
    restoring: false, starting: false, sending: false, runUnsettled: true,
    decidingApproval: false, cancelling: false, refreshingRun: false,
  };
  assert.equal(previewSessionActionsBlocked(state), true);
  assert.equal(previewRunIsTerminal("failed"), true);
  assert.equal(previewSessionActionsBlocked({ ...state, runUnsettled: false }), false);
});

test("refresh recovers approval, then approved terminal result and authorized final answer", async () => {
  let status = "awaiting_approval";
  const history = [
    { role: "user", content: "question", metadata: { runtime_run_id: "run-one" } },
  ];
  const options = {
    runId: null as string | null,
    readHistory: async () => history,
    readRun: async () => ({ status, approval: status === "awaiting_approval" ? "approval-one" : null }),
    isActive: () => true,
    wait: async () => undefined,
    onRunId: () => undefined,
    attempts: 2,
  };
  const parked = await pollPreviewOutcome(options);
  assert.deepEqual(parked, { kind: "approval", runId: "run-one", approval: "approval-one" });
  status = "succeeded";
  history.push({ role: "assistant", content: "final answer", metadata: { runtime_run_id: "run-one" } });
  const finished = await pollPreviewOutcome({ ...options, runId: "run-one" });
  assert.equal(finished.kind, "terminal");
  if (finished.kind === "terminal") {
    assert.equal(previewHistoryMessages(finished.messages)[1].content, "final answer");
  }
});

test("rejected approval stays blocked until failure history is read", async () => {
  let status = "awaiting_approval";
  const messages = [{ role: "user", content: "question", metadata: { runtime_run_id: "run-one" } }];
  const options = {
    runId: "run-one",
    readHistory: async () => messages,
    readRun: async () => ({ status, approval: status === "awaiting_approval" ? "approval-one" : null }),
    isActive: () => true,
    wait: async () => undefined,
    onRunId: () => undefined,
    attempts: 2,
  };
  assert.equal((await pollPreviewOutcome(options)).kind, "approval");
  status = "running";
  assert.equal((await pollPreviewOutcome(options)).kind, "pending");
  status = "failed";
  assert.equal((await pollPreviewOutcome(options)).kind, "terminal");
});

test("Preview only offers download for confirmed, nonempty generated artifacts", () => {
  assert.equal(previewArtifactCanDownload({ source: "ai", ready: true, size_bytes: 5 }), true);
  assert.equal(previewArtifactCanDownload({ source: "ai", ready: false, size_bytes: 0 }), false);
  assert.equal(previewArtifactCanDownload({ source: "user", ready: true, size_bytes: 5 }), false);
});

test("clicking a ready Preview file uses the authenticated downloader", async () => {
  const requests: Array<{ url: string; filename: string }> = [];
  const artifact = {
    artifact_id: "art_0123456789abcdef", filename: "report.pdf", title: "Report",
    source: "ai" as const, ready: true, size_bytes: 2048,
  };
  await downloadPreviewArtifact(
    artifact,
    (id) => `/api/v1/assistant/artifacts/${id}/download`,
    async (url, filename) => { requests.push({ url, filename }); },
  );
  assert.deepEqual(requests, [{
    url: "/api/v1/assistant/artifacts/art_0123456789abcdef/download",
    filename: "report.pdf",
  }]);
  await assert.rejects(downloadPreviewArtifact(
    { ...artifact, ready: false },
    () => "unused",
    async () => { throw new Error("should not download"); },
  ));
});
