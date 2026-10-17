import assert from "node:assert/strict";
import test from "node:test";
import { createTelemetryClient, routeName, sanitizeRecord, scrubError, VitalAccumulator, type TelemetryRecord } from "./telemetryCore.ts";
const id = "0123456789abcdef0123456789abcdef";
const configuration = (sampled = true) => ({ enabled: true, sampled, sessionId: id, sampleRate: 0.01, ttlSeconds: 900 });
const metric = (): TelemetryRecord => ({ kind: "metric", name: "INP", route: "/assistant", value: 180 });
function fixture(config = configuration()) {
  let identity = { key: "authority-a", token: "SYNTHETIC-TOKEN", authenticated: true }; let time = 1000;
  const requests: Array<{ url: string; init: RequestInit }> = [];
  const client = createTelemetryClient({ origin: "https://console.example", authority: () => identity, now: () => time,
    fetch: async (input, init = {}) => { requests.push({ url: String(input), init }); return new Response(JSON.stringify(config)); } });
  return { client, requests, setIdentity: (next: typeof identity) => { identity = next; }, setTime: (next: number) => { time = next; } };
}

test("collector is opt-in, authenticated and server sampled; an unsampled session is not polled for a better draw", async () => {
  const h = fixture(configuration(false)); await h.client.configure();
  h.client.record(metric()); await h.client.flush(); assert.equal(h.requests.length, 1);
  assert.equal(h.client.traceparent("/api/v1/services"), undefined); assert.equal(h.client.needsRefresh(), false);
  h.setTime(901001); assert.equal(h.client.needsRefresh(), true);
  h.setIdentity({ key: "guest", token: null as unknown as string, authenticated: false }); h.client.reset(); await h.client.configure(); assert.equal(h.requests.length, 1);
});

test("sampled metrics export only to the fixed authenticated same-origin endpoint and include no identity fields", async () => {
  const h = fixture(); await h.client.configure();
  h.client.record({ ...metric(), prompt: "PRIVATE PROMPT", tenant_id: "FORGED", url: "https://outside?token=SECRET" } as TelemetryRecord);
  await h.client.flush(); assert.equal(h.requests.length, 2);
  const sent = h.requests[1]; assert.equal(sent.url, "https://console.example/api/v1/telemetry/browser");
  assert.equal(sent.init.credentials, "same-origin"); assert.equal(sent.init.redirect, "error");
  assert.equal(new Headers(sent.init.headers).get("Authorization"), "Bearer SYNTHETIC-TOKEN");
  const body = String(sent.init.body); assert.doesNotMatch(body, /PRIVATE|FORGED|SECRET|token|tenant|user|prompt|url/);
  assert.equal(JSON.parse(body).session_id, id); assert.equal(JSON.parse(body).records[0].trace_id, id);
});

test("authority changes and late A-B-A configuration cannot export or attach stale traces", async () => {
  let scope = "A"; let resolve!: (response: Response) => void;
  const requests: string[] = [];
  const client = createTelemetryClient({ origin: "https://console.example", authority: () => ({ key: scope, token: "fixture", authenticated: true }), fetch: async input => { requests.push(String(input)); return new Promise(done => { resolve = done; }); } });
  const pending = client.configure(); scope = "B"; client.reset(); scope = "A"; client.reset();
  resolve(new Response(JSON.stringify(configuration()))); await pending;
  client.record(metric()); await client.flush(); assert.equal(requests.length, 1); assert.equal(client.traceparent("/api/v1/services"), undefined);
});

test("tenant/credential transitions drop queued records and expire trace propagation", async () => {
  const h = fixture(); await h.client.configure(); h.client.record(metric());
  h.setIdentity({ key: "authority-b", token: "NEW-TOKEN", authenticated: true }); await h.client.flush(); assert.equal(h.requests.length, 1);
  assert.equal(h.client.traceparent("/api/v1/services"), undefined);
  h.setIdentity({ key: "authority-a", token: "SYNTHETIC-TOKEN", authenticated: true }); h.setTime(901001);
  assert.equal(h.client.traceparent("/api/v1/services"), undefined);
});

test("traceparent is W3C-shaped, has one page trace and distinct request spans; external and collector URLs receive none", async () => {
  const h = fixture(); await h.client.configure();
  const a = h.client.traceparent("/api/v1/services?token=PRIVATE")!; const b = h.client.traceparent("/v1/responses")!;
  assert.match(a, /^00-[a-f0-9]{32}-[a-f0-9]{16}-01$/); assert.equal(a.split("-")[1], id); assert.notEqual(a.split("-")[2], b.split("-")[2]);
  for (const url of ["https://outside.example/api/services", "//outside.example/api/services", "/assets/index.js", "/api/v1/telemetry/browser"]) assert.equal(h.client.traceparent(url), undefined);
});

test("privacy allowlist retains only route families and same-origin JS locations, never error text or query", () => {
  assert.equal(routeName("/knowledge/PRIVATE-ID?token=SECRET#prompt"), "/knowledge/:id"); assert.equal(routeName("/PRIVATE-UNKNOWN"), "/other");
  const error = new TypeError("PRIVATE PROMPT token=SECRET email=private@example.com");
  error.stack = "TypeError: PRIVATE PROMPT\n    at privateFunction (https://console.example/assets/index-abcd1234.js?token=SECRET:12:34)\n    at https://outside.example/PRIVATE.js:1:2";
  const value = scrubError(error, "https://console.example");
  assert.deepEqual(value, { name: "TypeError", frames: [{ asset: "/assets/index-abcd1234.js", line: 12, column: 34 }] });
  assert.doesNotMatch(JSON.stringify(value), /PRIVATE|SECRET|email|privateFunction/);
  assert.deepEqual(scrubError({ message: "PRIVATE", reason: "SECRET" }, "https://console.example", "UnhandledRejection"), { name: "UnhandledRejection", frames: [] });
  assert.equal(sanitizeRecord({ ...metric(), route: "/knowledge/PRIVATE?token=SECRET" } as TelemetryRecord), undefined);
});

test("CLS uses the largest session window and excludes recent input; INP groups duplicate event entries", () => {
  const value = new VitalAccumulator();
  value.addLayoutShift(100, 0.1, false); value.addLayoutShift(900, 0.2, false); value.addLayoutShift(1000, 5, true);
  value.addLayoutShift(2500, 0.15, false); assert.ok(Math.abs(value.cls - 0.3) < 0.00001);
  value.addLcp(850); value.addLcp(1200); assert.equal(value.lcp, 1200);
  value.addInteraction(7, 300); value.addInteraction(7, 320); value.addInteraction(14, 180);
  assert.deepEqual(value.inp(2), { value: 320, quality: "native" });
  assert.deepEqual(value.inp(100), { value: 180, quality: "native" });
  assert.equal(value.inp()?.quality, "observed");
});

test("export queue and per-minute admission are bounded without automatic retry", async () => {
  const h = fixture(); await h.client.configure();
  for (let batch = 0; batch < 4; batch++) { for (let i = 0; i < 30; i++) h.client.record({ kind: "metric", name: "api_headers", route: "/assistant", value: i }); await h.client.flush(); }
  assert.equal(h.requests.length, 4);
  assert.ok(h.requests.slice(1).every(request => JSON.parse(String(request.init.body)).records.length <= 20));
  h.setTime(62000); h.client.record(metric()); await h.client.flush(); assert.equal(h.requests.length, 5);
});

test("invalid collector sampling policy cannot enable client capture", async () => {
  for (const override of [{ sampleRate: 1 }, { sampleRate: "0.01" }, { ttlSeconds: 3600 }, { sessionId: "00000000000000000000000000000000" }]) {
    const client = createTelemetryClient({ origin: "https://console.example", authority: () => ({ key: "a", token: "fixture", authenticated: true }), fetch: async () => new Response(JSON.stringify({ ...configuration(), ...override })) });
    await client.configure(); assert.equal(client.enabled(), false);
  }
});

test("vitals and scrubbed errors survive a full ordinary API timing queue", async () => {
  const h = fixture(); await h.client.configure();
  for (let i = 0; i < 20; i++) h.client.record({ kind: "metric", name: "api_headers", route: "/assistant", value: i });
  h.client.record({ kind: "metric", name: "LCP", route: "/assistant", value: 850 });
  h.client.record({ kind: "error", name: "RenderError", route: "/assistant", frames: [] });
  await h.client.flush();
  const records = JSON.parse(String(h.requests[1].init.body)).records;
  assert.equal(records.length, 20);
  assert.ok(records.some((record: { name: string }) => record.name === "LCP"));
  assert.ok(records.some((record: { name: string }) => record.name === "RenderError"));
});


test("large scrubbed error batches stay below the collector body limit without dropping the remaining queue", async () => {
  const h = fixture(); await h.client.configure();
  const frames = Array.from({ length: 5 }, () => ({ asset: `/assets/${"a".repeat(110)}.js`, line: 9999999, column: 9999999 }));
  for (let i = 0; i < 20; i++) h.client.record({ kind: "error", name: "TypeError", route: "/assistant", frames });
  await h.client.flush(); await h.client.flush();
  assert.ok(h.requests.slice(1).every(request => String(request.init.body).length <= 15 * 1024));
  assert.equal(h.requests.slice(1).reduce((count, request) => count + JSON.parse(String(request.init.body)).records.length, 0), 20);
});
