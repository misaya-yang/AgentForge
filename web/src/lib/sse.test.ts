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
const { sseFetch, sseFetchEvents, sseFetchAGUI } = await import("./sse.ts");
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
    assert.equal((await transport("/fixture", { signal: caller.signal, timeoutMs: 5 }).next()).done, true);
    assert.equal(cancelled, 1);
    assert.equal(response.body?.locked, false);
    assert.equal(getEventListeners(caller.signal, "abort").length, 0);
  });
}
