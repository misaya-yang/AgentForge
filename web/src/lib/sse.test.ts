import assert from "node:assert/strict";
import { getEventListeners } from "node:events";
import { registerHooks } from "node:module";
import { test } from "node:test";

// Load the production transport with only its browser configuration/auth
// dependencies stubbed. Parsing, fetch, timeout and reader cleanup stay real.
const transportUrl = new URL("./sse.ts", import.meta.url).href;
const hooks = registerHooks({
  resolve(specifier, context, nextResolve) {
    if (context.parentURL === transportUrl) {
      const stubs = {
        "@/config/runtime": "export const isSseDebugEnabled = () => false;",
        "@/lib/api": "export const getAuthToken = () => null;",
      };
      if (specifier in stubs) return {
        url: `data:text/javascript,${encodeURIComponent(stubs[specifier as keyof typeof stubs])}`,
        shortCircuit: true,
      };
      if (specifier === "./sseEventParser") return nextResolve(new URL("./sseEventParser.ts", transportUrl).href, context);
    }
    return nextResolve(specifier, context);
  },
  load(url, context, nextLoad) {
    const loaded = nextLoad(url, context);
    if (url !== transportUrl) return loaded;
    return { ...loaded, source: `import.meta.env = { DEV: false };\n${loaded.source}` };
  },
});
const { sseFetch, sseFetchEvents, sseFetchAGUI, sseReconnectDelayMs, SSEIdleTimeoutError, SSEInterruptedError, waitForSseReconnect } = await import("./sse.ts");
hooks.deregister();

const transports = [sseFetch, sseFetchEvents, sseFetchAGUI];

for (const transport of transports) {
  test(`${transport.name} cancels an open HTTP body on an early terminal return`, async t => {
    let cancelled = 0;
    const response = new Response(new ReadableStream<Uint8Array>({
      start(controller) { controller.enqueue(new TextEncoder().encode('data: {"event":"run_finished"}\n\n')); },
      cancel() { cancelled++; },
    }));
    t.mock.method(globalThis, "fetch", async () => response);
    const caller = new AbortController();
    const stream = transport("/fixture", { signal: caller.signal });
    assert.equal((await stream.next()).done, false);
    await stream.return();
    assert.equal(cancelled, 1);
    assert.equal(response.body?.locked, false);
    assert.equal(getEventListeners(caller.signal, "abort").length, 0);
  });
}

for (const transport of [sseFetch, sseFetchEvents]) {
  for (const failure of ["network", "http"] as const) {
    test(`${transport.name} clears timeout and caller listener after ${failure} failure`, async t => {
      const activeTimers = new Set<ReturnType<typeof setTimeout>>();
      const setTimer = globalThis.setTimeout;
      const clearTimer = globalThis.clearTimeout;
      t.after(() => { for (const timer of activeTimers) clearTimer(timer); });
      t.mock.method(globalThis, "setTimeout", (...args: Parameters<typeof setTimeout>) => {
        const timer = setTimer(...args);
        activeTimers.add(timer);
        return timer;
      });
      t.mock.method(globalThis, "clearTimeout", (timer: ReturnType<typeof setTimeout>) => {
        activeTimers.delete(timer);
        clearTimer(timer);
      });
      t.mock.method(globalThis, "fetch", async () => {
        if (failure === "network") throw new TypeError("fixture disconnected");
        return new Response('{"detail":"fixture failure"}', { status: 503 });
      });
      const caller = new AbortController();
      await assert.rejects(transport("/fixture", { signal: caller.signal }).next());
      assert.equal(getEventListeners(caller.signal, "abort").length, 0);
      assert.equal(activeTimers.size, 0);
    });
  }

  test(`${transport.name} propagates an already-aborted caller before fetch`, async t => {
    const reason = new Error("fixture cancelled");
    t.mock.method(globalThis, "fetch", async (_url, init: RequestInit) => {
      assert.equal(init.signal?.aborted, true);
      assert.equal(init.signal?.reason, reason);
      throw reason;
    });
    const caller = new AbortController();
    caller.abort(reason);
    await assert.rejects(transport("/fixture", { signal: caller.signal }).next(), reason);
    assert.equal(getEventListeners(caller.signal, "abort").length, 0);
  });

  test(`${transport.name} cancels a blocked reader on timeout`, async t => {
    let cancelled = 0;
    const response = new Response(new ReadableStream<Uint8Array>({ cancel() { cancelled++; } }));
    t.mock.method(globalThis, "fetch", async () => response);
    const caller = new AbortController();
    await assert.rejects(transport("/fixture", { signal: caller.signal, timeoutMs: 5 }).next(), /idle budget exceeded/);
    assert.equal(cancelled, 1);
    assert.equal(response.body?.locked, false);
    assert.equal(getEventListeners(caller.signal, "abort").length, 0);
  });
}


async function flushStreamTasks() { for (let i = 0; i < 8; i++) await Promise.resolve(); }
for (const transport of transports) {
  test(`${transport.name} survives a virtual hour of byte keep-alives with no wall-clock deadline`, async t => {
    t.mock.timers.enable({ apis: ["setTimeout", "Date"] });
    let body!: ReadableStreamDefaultController<Uint8Array>;
    t.mock.method(globalThis, "fetch", async () => new Response(new ReadableStream({ start(controller) { body = controller; } })));
    const stream = transport("/long-turn", {}); const first = stream.next();
    await flushStreamTasks();
    for (let i = 0; i < 181; i++) {
      t.mock.timers.tick(20_000); body.enqueue(new TextEncoder().encode(": heartbeat\n\n")); await flushStreamTasks();
    }
    body.enqueue(new TextEncoder().encode('data: {"event":"run_finished"}\n\n'));
    assert.equal((await first).done, false);
    await stream.return();
    // This simulates elapsed time; it is explicitly not the required real one-hour provider acceptance.
  });
}

test("default idle budget rejects silence with an explicit unknown outcome", async t => {
  t.mock.timers.enable({ apis: ["setTimeout", "Date"] });
  let cancelled = 0;
  t.mock.method(globalThis, "fetch", async () => new Response(new ReadableStream({ cancel() { cancelled++; } })));
  const states: string[] = [];
  const rejected = assert.rejects(sseFetch("/silent", { onConnectionState: state => states.push(state) }).next(), /idle budget.*unknown/);
  await flushStreamTasks(); t.mock.timers.tick(45_000); await rejected;
  assert.equal(cancelled, 1); assert.equal(states.at(-1), "unknown_outcome");
});

test("background tabs suspend the idle budget and foreground resumes a fresh budget", async t => {
  t.mock.timers.enable({ apis: ["setTimeout", "Date"] });
  const previous = Object.getOwnPropertyDescriptor(globalThis, "document");
  const document = Object.assign(new EventTarget(), { visibilityState: "visible" });
  Object.defineProperty(globalThis, "document", { configurable: true, value: document });
  t.after(() => { if (previous) Object.defineProperty(globalThis, "document", previous); else Reflect.deleteProperty(globalThis, "document"); });
  let cancelled = 0;
  t.mock.method(globalThis, "fetch", async () => new Response(new ReadableStream({ cancel() { cancelled++; } })));
  const states: string[] = [];
  const pending = assert.rejects(sseFetchEvents("/background", { onConnectionState: state => states.push(state) }).next(), /idle budget/);
  await flushStreamTasks();
  document.visibilityState = "hidden"; document.dispatchEvent(new Event("visibilitychange"));
  t.mock.timers.tick(3_600_000); await flushStreamTasks(); assert.equal(cancelled, 0);
  document.visibilityState = "visible"; document.dispatchEvent(new Event("visibilitychange"));
  t.mock.timers.tick(44_999); await flushStreamTasks(); assert.equal(cancelled, 0);
  t.mock.timers.tick(1); await pending;
  assert.ok(states.includes("background")); assert.equal(getEventListeners(document, "visibilitychange").length, 0);
});

test("Last-Event-ID replay after disconnect yields each token, tool result and terminal once", async t => {
  const bodies = [
    'id: cursor-1\ndata: {"token":"A"}\n\n',
    'id: cursor-1\ndata: {"token":"duplicate"}\n\nid: cursor-2\ndata: {"tool_result":"effect-1"}\n\nid: cursor-3\nevent: terminal\ndata: {"done":true}\n\n',
  ];
  const cursors: Array<string | null> = []; let reads = 0; const states: string[] = [];
  t.mock.method(globalThis, "fetch", async (_url, init: RequestInit) => {
    cursors.push(new Headers(init.headers).get("Last-Event-ID"));
    assert.ok(reads < 2, "terminal must stop reconnects"); return new Response(bodies[reads++]);
  });
  const received = [];
  for await (const event of sseFetchEvents("/durable", { onConnectionState: state => states.push(state), resume: { initialDelayMs: 0, maxDelayMs: 0, isTerminal: event => event.event === "terminal" } })) received.push(event);
  assert.deepEqual(cursors, [null, "cursor-1"]); assert.equal(received.length, 3);
  assert.deepEqual(received.map(event => event.id), ["cursor-1", "cursor-2", "cursor-3"]);
  assert.ok(states.includes("reconnecting")); assert.equal(states.at(-1), "server_terminal");
});

test("resume never retries POST inference or events delivered without a cursor", async t => {
  let dispatched = 0;
  t.mock.method(globalThis, "fetch", async () => { dispatched++; return new Response('data: {"token":"A"}\n\n'); });
  const resume = { initialDelayMs: 0, isTerminal: () => false };
  await assert.rejects(sseFetchEvents("/inference", { method: "POST", resume }).next(), /never be resubmitted/);
  assert.equal(dispatched, 0);
  await assert.rejects(async () => { for await (const event of sseFetchEvents("/unsafe-replay", { resume })) void event; }, /safe resume cursor/);
  assert.equal(dispatched, 1);
});

test("reconnect backoff has bounded equal jitter instead of a fixed polling interval", t => {
  t.mock.method(Math, "random", () => 0);
  assert.equal(sseReconnectDelayMs(0), 125); assert.equal(sseReconnectDelayMs(2), 500);
  assert.equal(sseReconnectDelayMs(100), 5000);
  t.mock.method(Math, "random", () => 0.99999);
  assert.ok(sseReconnectDelayMs(100) <= 10000);
});


for (const transport of transports) test(`${transport.name} never repeats an interrupted inference POST`, async t => {
  let controller!: ReadableStreamDefaultController<Uint8Array>; let requests = 0;
  t.mock.method(globalThis, "fetch", async () => { requests++; return new Response(new ReadableStream({ start(body) { controller = body; body.enqueue(new TextEncoder().encode('data: {"event":"text_delta","content":"partial"}\n\n')); } })); });
  const stream = transport("/inference", { method: "POST", body: "{}" });
  assert.equal((await stream.next()).done, false);
  controller.error(new TypeError("network disconnected"));
  await assert.rejects(stream.next(), /server outcome is unknown/);
  assert.equal(requests, 1);
});


test("Playground renders interrupted and idle-expired POST outcomes as unknown", async () => {
  const { getPlaygroundErrorMessage } = await import("./utils.ts");
  for (const error of [new SSEIdleTimeoutError(), new SSEInterruptedError(new TypeError("disconnected"))]) {
    assert.equal(getPlaygroundErrorMessage(error, ((key: string) => key) as never), "playground.errors.streamOutcomeUnknown");
  }
});


test("shared reconnect wait pauses in hidden tabs and abort cleans up without another read", async t => {
  t.mock.timers.enable({ apis: ["setTimeout", "Date"] });
  t.mock.method(Math, "random", () => 0);
  const previous = Object.getOwnPropertyDescriptor(globalThis, "document");
  const document = Object.assign(new EventTarget(), { visibilityState: "hidden" });
  Object.defineProperty(globalThis, "document", { configurable: true, value: document });
  t.after(() => { if (previous) Object.defineProperty(globalThis, "document", previous); else Reflect.deleteProperty(globalThis, "document"); });
  const caller = new AbortController(); let resumed = false;
  const pending = waitForSseReconnect(caller.signal).then(() => { resumed = true; });
  t.mock.timers.tick(3_600_000); await flushStreamTasks(); assert.equal(resumed, false);
  document.visibilityState = "visible"; document.dispatchEvent(new Event("visibilitychange"));
  t.mock.timers.tick(125); await pending; assert.equal(resumed, true);
  assert.equal(getEventListeners(document, "visibilitychange").length, 0);
  document.visibilityState = "hidden";
  const cancelled = waitForSseReconnect(caller.signal); caller.abort(); await cancelled;
  assert.equal(getEventListeners(document, "visibilitychange").length, 0);
  assert.equal(getEventListeners(caller.signal, "abort").length, 0);
});
