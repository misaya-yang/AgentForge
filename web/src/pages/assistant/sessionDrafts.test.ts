import assert from "node:assert/strict";
import { test } from "node:test";

import {
  assistantDraftKey,
  bindNewAssistantDraft,
  clearAdmittedAssistantDrafts,
  readAssistantDrafts,
  transitionAssistantDraft,
  writeAssistantDrafts,
} from "./sessionDrafts.ts";

test("two conversations and the new-chat composer keep separate drafts", () => {
  let state = transitionAssistantDraft({}, assistantDraftKey(undefined), "new draft", "session-a");
  assert.equal(state.input, "");
  state = transitionAssistantDraft(state.drafts, "session-a", "draft A", "session-b");
  assert.equal(state.input, "");
  state = transitionAssistantDraft(state.drafts, "session-b", "draft B", "session-a");
  assert.equal(state.input, "draft A");
  state = transitionAssistantDraft(state.drafts, "session-a", state.input, assistantDraftKey(null));
  assert.equal(state.input, "new draft");
});

test("admitted new run clears both the new-chat and bound session draft", () => {
  assert.deepEqual(
    clearAdmittedAssistantDrafts({ new: "sent", "session-a": "old", "session-b": "keep" }, undefined, "session-a"),
    { new: "", "session-a": "", "session-b": "keep" },
  );
  assert.deepEqual(
    clearAdmittedAssistantDrafts({ new: "keep", "session-a": "sent" }, "session-a", "session-a"),
    { new: "keep", "session-a": "" },
  );
});

test("a persisted new chat takes its unsent draft out of the blank new-chat slot", () => {
  const drafts = bindNewAssistantDraft({ new: "retry this message", "session-b": "keep" }, "retry this message", "session-a");
  assert.deepEqual(drafts, { new: "", "session-a": "retry this message", "session-b": "keep" });
  assert.equal(transitionAssistantDraft(drafts, "session-a", drafts["session-a"], "new").input, "");
});

test("persisted drafts stay scoped to the authenticated user", () => {
  const rows = new Map<string, string>();
  (globalThis as { window?: unknown }).window = {
    localStorage: {
      getItem: (key: string) => rows.get(key) ?? null,
      setItem: (key: string, value: string) => rows.set(key, value),
    },
  };
  writeAssistantDrafts("user-a", { "session-a": "private draft" });
  assert.equal(readAssistantDrafts("user-a")["session-a"], "private draft");
  assert.deepEqual(readAssistantDrafts("user-b"), {});
  assert.deepEqual(readAssistantDrafts(undefined), {});
});
