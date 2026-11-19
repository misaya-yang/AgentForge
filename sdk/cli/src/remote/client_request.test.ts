import { afterEach, describe, expect, it, vi } from "vitest";
import { GatewayClient } from "./client.js";

// Real Response JSON consumption with controlled fetch dispatch/body delivery.
// No listener, network, provider or native binary is involved.
function fixture(status = 200) {
  let body!: ReadableStreamDefaultController<Uint8Array>;
  let signal!: AbortSignal;
  const response = new Response(status === 204 ? null : new ReadableStream<Uint8Array>({
    start(controller) { body = controller; },
  }), { status, headers: { "content-type": "application/json" } });
  const fetchMock = vi.fn(async (_url: string, options: RequestInit) => {
    signal = options.signal as AbortSignal;
    if (status !== 204) signal.addEventListener("abort", () => body.error(signal.reason), { once: true });
    return response;
  });
  vi.stubGlobal("fetch", fetchMock);
  const client = new GatewayClient({ base_url: "https://gateway.invalid", api_key: "", tenant_id: "", timeout: 1 } as ConstructorParameters<typeof GatewayClient>[0]);
  return {
    client, fetchMock, signal: () => signal,
    finish(text: string) { body.enqueue(new TextEncoder().encode(text)); body.close(); },
  };
}
async function headersRead() { for (let i = 0; i < 5; i++) await Promise.resolve(); }
afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

describe("hosted JSON response-body lifetime", () => {
  it("keeps the command deadline active after successful headers", async () => {
    vi.useFakeTimers(); const h = fixture();
    const result = h.client.request("GET", "/api/v1/admin/models").then(value => ({ value }), error => ({ error }));
    await headersRead(); await vi.advanceTimersByTimeAsync(1000);
    const aborted = h.signal().aborted;
    if (!aborted) h.finish("[]"); // Settle the baseline without hanging the runner.
    const outcome = await result;
    expect(aborted).toBe(true);
    expect("error" in outcome && outcome.error.message).toBe("request timed out after 1000ms");
    expect(h.fetchMock).toHaveBeenCalledTimes(1); expect(vi.getTimerCount()).toBe(0);
  });
  it("forwards caller cancellation after headers without replay", async () => {
    vi.useFakeTimers(); const h = fixture(); const caller = new AbortController();
    const reason = new Error("fixture cancellation");
    const result = h.client.request("GET", "/api/v1/usage/summary", undefined, { signal: caller.signal }).then(value => ({ value }), error => ({ error }));
    await headersRead(); caller.abort(reason); const aborted = h.signal().aborted;
    if (!aborted) h.finish("{}");
    const outcome = await result;
    expect(aborted).toBe(true); expect("error" in outcome && outcome.error).toBe(reason);
    expect(h.fetchMock).toHaveBeenCalledTimes(1); expect(vi.getTimerCount()).toBe(0);
  });
  it("cleans deadline and caller forwarding only after successful JSON settles", async () => {
    vi.useFakeTimers(); const h = fixture(); const caller = new AbortController();
    const removed = vi.spyOn(caller.signal, "removeEventListener");
    const result = h.client.request("GET", "/api/v1/auth/me", undefined, { signal: caller.signal });
    await headersRead(); const timersDuringBody = vi.getTimerCount(); const removalsDuringBody = removed.mock.calls.length;
    h.finish('{"username":"fixture"}'); expect(await result).toEqual({ username: "fixture" });
    expect(timersDuringBody).toBe(1); expect(removalsDuringBody).toBe(0);
    expect(removed).toHaveBeenCalledTimes(1); expect(vi.getTimerCount()).toBe(0);
    caller.abort(); expect(h.signal().aborted).toBe(false); expect(h.fetchMock).toHaveBeenCalledTimes(1);
  });
  it("propagates invalid JSON and cleans up without replay", async () => {
    vi.useFakeTimers(); const h = fixture(); const caller = new AbortController();
    const result = h.client.request("GET", "/api/v1/admin/models", undefined, { signal: caller.signal }).catch(error => error);
    await headersRead(); h.finish("not JSON"); expect(await result).toBeInstanceOf(SyntaxError);
    expect(vi.getTimerCount()).toBe(0); caller.abort(); expect(h.signal().aborted).toBe(false);
    expect(h.fetchMock).toHaveBeenCalledTimes(1);
  });
  it("preserves the existing HTTP error-body deadline", async () => {
    vi.useFakeTimers(); const h = fixture(503);
    const result = h.client.request("GET", "/api/v1/admin/models").catch(error => error);
    await headersRead(); await vi.advanceTimersByTimeAsync(1000);
    expect((await result).message).toBe("request timed out after 1000ms");
    expect(h.fetchMock).toHaveBeenCalledTimes(1); expect(vi.getTimerCount()).toBe(0);
  });
  it("preserves bodyless 204 success and cleanup", async () => {
    vi.useFakeTimers(); const h = fixture(204); const caller = new AbortController();
    expect(await h.client.request("DELETE", "/fixture", undefined, { signal: caller.signal })).toBeUndefined();
    expect(vi.getTimerCount()).toBe(0); caller.abort(); expect(h.signal().aborted).toBe(false);
    expect(h.fetchMock).toHaveBeenCalledTimes(1);
  });
  it("preserves bounded management JSON deadline behavior", async () => {
    vi.useFakeTimers(); const h = fixture();
    const result = h.client.request("GET", "/fixture", undefined, { boundedJson: true }).catch(error => error);
    await headersRead(); await vi.advanceTimersByTimeAsync(1000);
    expect((await result).message).toBe("request timed out after 1000ms");
    expect(h.fetchMock).toHaveBeenCalledTimes(1); expect(vi.getTimerCount()).toBe(0);
  });
});
