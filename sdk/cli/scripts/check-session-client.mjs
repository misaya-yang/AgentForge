import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { isIP } from "node:net";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const TOKEN = "fixture-session-token";
function clientHost({ origin = "https://gateway.example", token = TOKEN, respond, timeout = 30, timers } = {}) {
  const source = readFileSync(new URL("../src/remote/session_client.ts", import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, {
    fileName: "session_client.ts",
    compilerOptions: { target: ts.ScriptTarget.ES2023, module: ts.ModuleKind.CommonJS },
    reportDiagnostics: true,
  });
  assert.deepEqual(compiled.diagnostics, []);
  const requests = [];
  const exports = {};
  vm.runInNewContext(compiled.outputText, {
    exports, URL, AbortController, TextDecoder,
    setTimeout: timers?.setTimeout ?? setTimeout,
    clearTimeout: timers?.clearTimeout ?? clearTimeout,
    require(id) { assert.equal(id, "node:net"); return { isIP }; },
    async fetch(url, options) {
      requests.push({ url: String(url), options });
      return respond ? respond(url, options) : Response.json({ sessions: [], total: 0 });
    },
  });
  return { requests, client: new exports.GatewaySessionClient(origin, token, timeout) };
}

function bearerOnly(request) {
  assert.equal(request.options.method, "GET");
  assert.equal(request.options.redirect, "error");
  assert.deepEqual(Object.keys(request.options.headers).sort(), ["Accept", "Authorization"]);
  assert.equal(request.options.headers.Authorization, `Bearer ${TOKEN}`);
  assert.equal(request.options.headers.Accept, "application/json");
  assert.equal(request.options.body, undefined);
  assert.ok(!request.url.includes(TOKEN));
}

test("session list sends one same-origin GET with only bearer session authentication", async () => {
  const h = clientHost();
  assert.deepEqual(JSON.parse(JSON.stringify(await h.client.list())), { sessions: [], total: 0 });
  assert.equal(h.requests.length, 1);
  assert.equal(h.requests[0].url, "https://gateway.example/api/v1/assistant/sessions?limit=20");
  bearerOnly(h.requests[0]);
});

test("session history cannot turn an opaque ID into an off-origin or alternate-route request", async () => {
  for (const id of ["known-id", "https://other.example/history", "//other.example/path", "id?limit=500#fragment", "session/name"]) {
    const h = clientHost();
    await h.client.history(id);
    assert.equal(h.requests.length, 1);
    assert.equal(h.requests[0].url, `https://gateway.example/api/v1/assistant/sessions/${encodeURIComponent(id)}/history`);
    bearerOnly(h.requests[0]);
  }
});

test("missing or malformed session credentials fail before fetch without API-key fallback", async () => {
  for (const token of [null, "", "bad token", "bad\nvalue", "x".repeat(16_385)]) {
    const h = clientHost({ token });
    await assert.rejects(h.client.list(), /Gateway device login|AI_GATEWAY_SESSION_TOKEN/);
    assert.equal(h.requests.length, 0);
  }
});

test("invalid session IDs fail before fetch", async () => {
  for (const id of ["", ".", "..", "bad\nvalue", "\0", "x".repeat(1025)]) {
    const h = clientHost();
    await assert.rejects(h.client.history(id), /Invalid session ID/);
    assert.equal(h.requests.length, 0);
  }
});

test("session origin rejects plaintext remote hosts, URL credentials and non-origin configuration", async () => {
  for (const origin of [
    "not-a-url", "http://gateway.example", "http://localhost:8080", "ftp://127.0.0.1",
    "https://username:secret@gateway.example", "https://gateway.example/path",
    "https://gateway.example?secret=value", "https://gateway.example#fragment",
    "http://127.0.0.1.other.example", "//gateway.example",
  ]) {
    const h = clientHost({ origin });
    await assert.rejects(h.client.list(), (error) => {
      assert.match(error.message, /HTTPS Gateway origin or explicit loopback IP/);
      assert.ok(!error.message.includes("secret"));
      return true;
    });
    assert.equal(h.requests.length, 0);
  }
});

test("session reads allow HTTPS and explicit IPv4/IPv6 loopback HTTP origins", async () => {
  for (const origin of ["https://gateway.example:8443", "http://127.0.0.1:8080", "http://127.1.2.3:8080", "http://[::1]:8080"]) {
    const h = clientHost({ origin });
    await h.client.list();
    assert.equal(h.requests.length, 1);
    assert.equal(new URL(h.requests[0].url).origin, origin);
    bearerOnly(h.requests[0]);
  }
});

for (const status of [301, 302, 303, 307, 308, 401, 403, 404, 429, 503]) {
  test(`session HTTP ${status} is one attempt with no redirect or credential fallback`, async () => {
    const h = clientHost({ respond: () => new Response("do-not-print-private-response", {
      status, headers: { Location: "https://other.example/collect" },
    }) });
    await assert.rejects(h.client.list(), { message: `Gateway session read returned HTTP ${status}` });
    assert.equal(h.requests.length, 1);
    bearerOnly(h.requests[0]);
  });
}

test("session transport failures are sanitized and never retried", async () => {
  const h = clientHost({ respond: () => { throw new Error(`upstream echoed ${TOKEN}`); } });
  await assert.rejects(h.client.list(), (error) => {
    assert.ok(!error.message.includes(TOKEN));
    assert.match(error.message, /Session read failed/);
    return true;
  });
  assert.equal(h.requests.length, 1);
});

test("session responses reject malformed UTF-8, JSON and bodies over 8 MiB", async () => {
  for (const body of ["{private-response", new Uint8Array([0xff]), "x".repeat(8 * 1024 * 1024 + 1)]) {
    const h = clientHost({ respond: () => new Response(body) });
    await assert.rejects(h.client.list(), { message: "Invalid or oversized session response" });
    assert.equal(h.requests.length, 1);
  }
});

test("session deadline aborts the pending read once and clears its timer", async () => {
  let fire;
  let cleared = 0;
  const timers = {
    setTimeout(fn, delay) { fire = fn; assert.equal(delay, 1000); return 7; },
    clearTimeout(timer) { assert.equal(timer, 7); cleared++; },
  };
  const h = clientHost({ timeout: 0, timers, respond: (_url, options) => new Promise((_resolve, reject) => {
    options.signal.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
  }) });
  const pending = h.client.list();
  assert.equal(h.requests.length, 1);
  fire();
  await assert.rejects(pending, /Session read failed/);
  assert.equal(cleared, 1);
  assert.equal(h.requests.length, 1);
});

// Link the actual handler to the actual session client; only fetch and UI imports
// are doubles. This is source-level integration, not live Gateway authorization.
function productionHandler() {
  const source = readFileSync(new URL("../src/app.tsx", import.meta.url), "utf8");
  const exports = {};
  const modules = new Set([
    "react", "react/jsx-runtime", "ink", "ink-text-input", "./types/events.js",
    "./remote/client.js", "./remote/session_client.js", "./remote/chat.js",
    "./agent/slash-commands.js", "./memory.js", "./mcp/manager.js", "./skills.js",
    "./project-memory.js", "./agents.js", "node:fs", "./version.js",
  ]);
  vm.runInNewContext(ts.transpileModule(`${source}\nexport { handleAsyncCommand };`, {
    fileName: "app.tsx", compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, { exports, require(id) { assert.ok(modules.has(id), id); return {}; } });
  return exports.handleAsyncCommand;
}
const handle = productionHandler();
const ID = "93675f88-dab2-4496-bcde-b3c3460392d6";

test("production Gateway resume uses the isolated bearer client, then updates the exact selected session", async () => {
  const h = clientHost({ respond: () => Response.json({ session_id: ID, messages: [{ role: "assistant", content: "Recovered" }], total: 1 }) });
  let session;
  let messages;
  await handle({ action: "resume", resumeSessionId: ID }, {
    request() { assert.fail("Session recovery must not use the API-key model client"); },
  }, h.client, {}, () => {}, (id) => { session = id; }, (rows) => { messages = rows; }, () => {});
  assert.equal(session, ID);
  assert.equal(messages[1].content, "Recovered");
  assert.equal(h.requests[0].url, `https://gateway.example/api/v1/assistant/sessions/${ID}/history`);
  assert.equal(h.requests.length, 1);
  bearerOnly(h.requests[0]);
});

test("production Gateway history requires an explicit session credential and never falls back to the model key", async () => {
  const h = clientHost({ token: null });
  await assert.rejects(handle({ action: "history" }, {
    request() { assert.fail("Missing session token must not fall back to API key"); },
  }, h.client, {}, () => assert.fail("No history should be displayed"), () => assert.fail("No session change"),
  () => assert.fail("No message replacement"), () => {}), /Gateway device login|AI_GATEWAY_SESSION_TOKEN/);
  assert.equal(h.requests.length, 0);
});

test("production Gateway resume rejects a mismatched identity after the bearer fetch without changing selection", async () => {
  const h = clientHost({ respond: () => Response.json({ session_id: "different", messages: [], total: 0 }) });
  await assert.rejects(handle({ action: "resume", resumeSessionId: ID }, {}, h.client, {}, () => {},
    () => assert.fail("No session change"), () => assert.fail("No message replacement"), () => {}), /Invalid session history response/);
  assert.equal(h.requests.length, 1);
});
