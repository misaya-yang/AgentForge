import assert from "node:assert/strict";
import { test } from "node:test";

import {
  acceptPendingRunSession,
  confirmsAssistantRunAdmission,
  beginNewChatSession,
  persistNewChatSession,
  resetStreamStartMetrics,
  startChatWithoutAwaitingSessionCreate,
  streamStartMetrics,
} from "./newChatStream.ts";

test("existing and new sessions acknowledge only their admitted run", () => {
  assert.equal(confirmsAssistantRunAdmission({ requestedSessionId: "existing", eventSessionId: "existing", isResume: false }), true);
  assert.equal(confirmsAssistantRunAdmission({ requestedSessionId: "new", eventSessionId: "new", isResume: false }), true);
  assert.equal(confirmsAssistantRunAdmission({ requestedSessionId: "existing", eventSessionId: "other", isResume: false }), false);
  assert.equal(confirmsAssistantRunAdmission({ requestedSessionId: "existing", eventSessionId: "existing", isResume: true }), false);
});
import {
  createActivityFlushQueue,
  resetActivityFlushMetrics,
  activityFlushMetrics,
  type ActivityBatch,
} from "./coalesceUpdates.ts";

test("new chat opens the stream before session create resolves", async () => {
  resetStreamStartMetrics();
  const { sessionId, isNew } = beginNewChatSession(undefined, () => "client-session");
  assert.equal(sessionId, "client-session");
  assert.equal(isNew, true);

  let persistResolved = false;
  let openedWith: string | undefined;
  const persist = new Promise<void>((resolve) => {
    setTimeout(() => {
      persistResolved = true;
      resolve();
    }, 50);
  });

  const stream = startChatWithoutAwaitingSessionCreate({
    sessionId,
    isNew,
    openStream: (id) => {
      openedWith = id;
      return { kind: "sse" };
    },
    persistSession: () => persist,
  });

  assert.deepEqual(stream, { kind: "sse" });
  assert.equal(openedWith, "client-session");
  assert.equal(persistResolved, false);
  assert.equal(streamStartMetrics.starts, 1);
  assert.equal(streamStartMetrics.awaitedSessionCreate, 0);
  await persist;
});

test("openStream and persist share the minted session id", async () => {
  resetStreamStartMetrics();
  const minted = "11111111-1111-4111-8111-111111111111";
  const { sessionId, isNew } = beginNewChatSession(undefined, () => minted);
  const createdBodies: Array<{ session_id: string }> = [];
  let openedWith = "";
  let persistDone: Promise<{ session_id: string }> | undefined;

  const stream = startChatWithoutAwaitingSessionCreate({
    sessionId,
    isNew,
    openStream: (id) => {
      openedWith = id;
      return { kind: "sse" };
    },
    persistSession: (id) => {
      persistDone = persistNewChatSession(
        async (body) => {
          createdBodies.push({ session_id: body.session_id });
          return { session_id: body.session_id };
        },
        async () => undefined,
        { sessionId: id, title: "hello" },
      );
      return persistDone;
    },
  });

  assert.deepEqual(stream, { kind: "sse" });
  assert.ok(persistDone);
  const persisted = await persistDone;
  assert.equal(openedWith, minted);
  assert.equal(persisted.session_id, minted);
  assert.equal(createdBodies[0]?.session_id, minted);
  assert.equal(openedWith, persisted.session_id);
});

test("session create conflict is accepted only after owner-checked patch", async () => {
  const patched: Array<{ sessionId: string; body: Record<string, unknown> }> = [];
  const result = await persistNewChatSession(
    async () => {
      throw { response: { status: 409 } };
    },
    async (sessionId, body) => {
      patched.push({ sessionId, body });
    },
    {
      sessionId: "stream-created-session",
      title: "hello",
      config: { selected_model: "model-a" },
    },
  );

  assert.deepEqual(result, { session_id: "stream-created-session" });
  assert.deepEqual(patched, [
    {
      sessionId: "stream-created-session",
      body: {
        metadata: { title: "hello" },
        config: { selected_model: "model-a" },
      },
    },
  ]);
});

test("session create conflict is not swallowed when owner check fails", async () => {
  const ownerError = new Error("not owned");
  await assert.rejects(
    persistNewChatSession(
      async () => {
        throw { response: { status: 409 } };
      },
      async () => {
        throw ownerError;
      },
      { sessionId: "foreign-session", title: "hello" },
    ),
    ownerError,
  );
});

test("non-conflict create failures do not attempt patch recovery", async () => {
  let patchCalls = 0;
  const createError = { response: { status: 500 } };
  await assert.rejects(
    persistNewChatSession(
      async () => {
        throw createError;
      },
      async () => {
        patchCalls += 1;
      },
      { sessionId: "failed-session", title: "hello" },
    ),
    (error) => error === createError,
  );
  assert.equal(patchCalls, 0);
});

test("run_started binds only the current pending session", () => {
  assert.equal(
    acceptPendingRunSession({
      requestedSessionId: "current-session",
      pendingSessionId: "current-session",
      eventSessionId: "current-session",
    }),
    "current-session",
  );
  assert.equal(
    acceptPendingRunSession({
      requestedSessionId: "current-session",
      pendingSessionId: "stale-session",
      eventSessionId: "current-session",
    }),
    undefined,
  );
  assert.equal(
    acceptPendingRunSession({
      requestedSessionId: "current-session",
      pendingSessionId: "current-session",
      eventSessionId: "foreign-session",
    }),
    undefined,
  );
});

test("thinking and sub-agent events flush as one batch", () => {
  resetActivityFlushMetrics();
  const applied: ActivityBatch[] = [];
  const scheduled: Array<() => void> = [];
  const queue = createActivityFlushQueue({
    scheduleFlush: (flush) => {
      scheduled.push(flush);
    },
    apply: (batch) => {
      applied.push(batch);
    },
  });

  queue.enqueue({ kind: "thinking_start" });
  queue.enqueue({ kind: "thinking_delta", text: "first" });
  queue.enqueue({ kind: "thinking_delta", text: " second" });
  queue.enqueue({
    kind: "subagent",
    eventType: "subagent_started",
    data: { id: "child-1" },
    now: 1,
  });

  assert.equal(applied.length, 0);
  assert.equal(scheduled.length, 1);
  scheduled[0]();
  assert.equal(applied.length, 1);
  assert.equal(applied[0].thinkingStart, true);
  assert.equal(applied[0].thinkingDelta, "first second");
  assert.equal(applied[0].subagentEvents.length, 1);
  assert.equal(activityFlushMetrics.events, 4);
  assert.equal(activityFlushMetrics.flushes, 1);
});
