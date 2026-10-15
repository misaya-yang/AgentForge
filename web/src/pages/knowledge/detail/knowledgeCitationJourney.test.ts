import assert from "node:assert/strict";
import { createHash, webcrypto } from "node:crypto";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import * as qaOutcome from "./qaStreamOutcome.ts";
import { knowledgeCitation } from "./knowledgeCitation.ts";
import { sdkAuthorityScope } from "../../../lib/sdkAuthorityScope.ts";
import type { RetrieveHit } from "../../../types/knowledge.ts";
import type { CitationSource } from "../../assistant/components/citationSource.ts";

type Tree = { type: unknown; props: Record<string, unknown> };
type Component = (props: Record<string, unknown>) => unknown;
const hash = (body: string) => createHash("sha256").update(body).digest("hex");
const hit = (body = "saved paragraph", overrides: Partial<RetrieveHit> = {}): RetrieveHit => ({
  document_id: "document-A", segment_id: "segment-A", text: body, score: 0.75,
  source_version: 3, source_hash: hash(body), ...overrides,
});
function all(tree: unknown, type: string): Tree[] {
  if (!tree || typeof tree !== "object") return [];
  const node = tree as Tree;
  return [...(node.type === type ? [node] : []), ...Object.values(tree).flatMap((value) => all(value, type))];
}
function textOf(tree: unknown): string {
  if (typeof tree === "string") return tree;
  if (!tree || typeof tree !== "object") return "";
  const values = "props" in tree ? [(tree as Tree).props.children] : Object.values(tree);
  return values.map(textOf).join(" ");
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const same = (a: unknown[] | undefined, b: unknown[]) => a?.length === b.length && a.every((v, i) => Object.is(v, b[i]));
function host(file: string, name: string, initialProps: Record<string, unknown> = {}) {
  const slots: unknown[] = [];
  const cleanups = new Map<number, () => void>();
  let cursor = 0;
  let dirty = true;
  let effects: (() => void)[] = [];
  let props = initialProps;
  let tree: unknown;
  const timers = new Set<() => void>();
  type Digest = typeof webcrypto.subtle.digest;
  let digest: Digest = (...args) => webcrypto.subtle.digest(...args);
  const pendingDigests = new Set<ReturnType<Digest>>();
  const reads: { url: string; options: { params: { hash: string }; signal: AbortSignal } }[] = [];
  const legacyReads: string[][] = [];
  const auth = { token: "synthetic-session-A", user: { user_id: "user-A", permissions: ["read"], roles: ["member"], tier: "normal" } as ReturnType<typeof user> | null };
  function user() { return { user_id: "user-B", permissions: ["read"], roles: ["member"], tier: "normal" }; }
  let get: (url: string, options: typeof reads[number]["options"]) => Promise<unknown> = async () => { throw new Error("No fixture"); };
  let activeRead: () => Promise<unknown> = async () => ({ title: "Current title" });
  const qa = { calls: 0, fallbackCalls: 0, signals: [] as AbortSignal[], response: { retrieval_metadata: {}, answer: "saved answer", model: "fixture", query: "question", context_segments: [hit()], timing: { retrieval_ms: 1, llm_ms: 2, total_ms: 3 } } };
  let stream: () => AsyncGenerator<unknown> = async function* () { yield { event: "done", data: { result: qa.response } }; };
  const react = {
    useState(initial: unknown) {
      const index = cursor++;
      if (!(index in slots)) slots[index] = typeof initial === "function" ? initial() : initial;
      return [slots[index], (next: unknown) => {
        const value = typeof next === "function" ? next(slots[index]) : next;
        if (!Object.is(value, slots[index])) { slots[index] = value; dirty = true; }
      }];
    },
    useRef(initial: unknown) { const index = cursor++; return slots[index] ?? (slots[index] = { current: initial }); },
    useMemo(compute: () => unknown, deps: unknown[]) {
      const index = cursor++;
      const old = slots[index] as { deps: unknown[]; value: unknown } | undefined;
      if (!old || !same(old.deps, deps)) slots[index] = { deps, value: compute() };
      return (slots[index] as { value: unknown }).value;
    },
    useEffect(effect: () => void | (() => void), deps: unknown[]) {
      const index = cursor++;
      if (!same(slots[index] as unknown[] | undefined, deps)) {
        slots[index] = deps;
        effects.push(() => {
          cleanups.get(index)?.();
          const cleanup = effect();
          if (cleanup) cleanups.set(index, cleanup); else cleanups.delete(index);
        });
      }
    },
  };
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const jsx = (type: unknown, p: Record<string, unknown>) => ({ type, props: p });
  const modules: Record<string, unknown> = {
    react, "react/jsx-runtime": { jsx, jsxs: jsx, Fragment: "Fragment" },
    "react-i18next": { useTranslation: () => ({ t: (key: string, fallback?: unknown) => typeof fallback === "string" ? fallback : key }) },
    "lucide-react": named,
    "@/store/useAuthStore": { useAuthStore: (selector: (a: typeof auth) => unknown) => selector(auth) },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope },
    "@/lib/api": { api: { get: (url: string, options: typeof reads[number]["options"]) => { reads.push({ url, options }); return get(url, options); } } },
    "@/api/knowledge": {
      getActiveDocumentSource: async (...args: string[]) => { legacyReads.push(args); return activeRead(); },
      qaQueryStream: (_dataset: string, _request: unknown, signal: AbortSignal) => { qa.calls += 1; qa.signals.push(signal); return stream(); },
      qaQuery: async () => { qa.fallbackCalls += 1; return qa.response; },
    },
    "./knowledgeCitation": { knowledgeCitation },
    "./qaStreamOutcome": qaOutcome,
    "@/pages/knowledge/detail/evalCaseStore": { QA_EVAL_SOURCE: "qa" },
  };
  const exports: Record<string, Component> = {};
  const code = ts.transpileModule(readFileSync(new URL(file, import.meta.url), "utf8"), { compilerOptions: {
    module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX,
  } }).outputText;
  vm.runInNewContext(code, {
    exports, require: (id: string) => modules[id] ?? named,
    crypto: { subtle: { digest: (...args: Parameters<Digest>) => {
      const pending = digest(...args);
      pendingDigests.add(pending);
      void pending.then(() => pendingDigests.delete(pending), () => pendingDigests.delete(pending));
      return pending;
    } } },
    TextEncoder, AbortController, window: {
      setInterval: (cb: () => void) => { timers.add(cb); return cb; }, clearInterval: (cb: () => void) => timers.delete(cb),
    },
  });
  function render(next = props, runEffects = true): unknown {
    props = next;
    cursor = 0; dirty = false; tree = exports[name](props);
    if (runEffects) { const queued = effects; effects = []; queued.forEach((effect) => effect()); }
    return tree;
  }
  async function flush() {
    for (let i = 0; i < 10; i++) {
      await new Promise<void>((done) => setImmediate(done));
      if (pendingDigests.size > 0) {
        // Event-loop turns do not guarantee completion of real WebCrypto work.
        // Drain only owned digests, leaving deliberately deferred API reads pending.
        let timeout: ReturnType<typeof setTimeout> | undefined;
        try {
          await Promise.race([
            Promise.allSettled([...pendingDigests]),
            new Promise<never>((_resolve, reject) => {
              timeout = setTimeout(() => reject(new Error("Fixture WebCrypto digest did not settle")), 5000);
            }),
          ]);
        } finally {
          if (timeout) clearTimeout(timeout);
        }
      }
      if (dirty) render();
    }
    return tree;
  }
  return { auth, qa, reads, legacyReads, render, flush, user,
    setStream: (value: typeof stream) => { stream = value; },
    setDigest: (value: Digest) => { digest = value; },
    setGet: (value: typeof get) => { get = value; }, setActiveRead: (value: typeof activeRead) => { activeRead = value; },
    tick: () => timers.forEach((cb) => cb()), dispose: () => cleanups.forEach((cb) => cb()),
    current: () => tree,
  };
}
function source(body = "saved paragraph", overrides: Partial<RetrieveHit> = {}): CitationSource {
  return knowledgeCitation("dataset-A", hit(body, overrides));
}
function response(s: CitationSource, body = s.excerpt, override = {}) {
  return { data: { document_id: s.documentId, version_number: s.sourceVersion, title: "Saved title", content: body, source_hash: hash(body), ...override } };
}
const dialog = (s: CitationSource) => host("../../assistant/components/SourceDocumentDialog.tsx", "SourceDocumentDialog", { source: s, onClose: () => {} });

test("Knowledge preserves the exact top-level source identity and excerpt", () => {
  const s = knowledgeCitation("dataset-A", hit("中文跨页段落\n数量 42", { source_hash: "B".repeat(64) }));
  assert.deepEqual(s, { datasetId: "dataset-A", documentId: "document-A", excerpt: "中文跨页段落\n数量 42", provenance: "versioned", sourceVersion: 3, sourceHash: "b".repeat(64) });
});

test("missing, partial, legacy, conflicting and metadata-only identities are unavailable", () => {
  const variants: Partial<RetrieveHit>[] = [
    { source_version: null }, { source_hash: null }, { source_version: undefined, source_hash: undefined },
    { source_version: null, source_hash: null, metadata: { source_version: 8, source_hash: hash("newer") } },
    { metadata: { document_id: "other-document" } }, { metadata: { dataset_id: "other-dataset" } },
    { metadata: { source_version: 4, source_hash: hash("saved paragraph") } },
    { source_version: 0 }, { source_version: 1.5 }, { source_hash: "bad" }, { document_id: "" },
  ];
  for (const variant of variants) assert.equal(source(undefined, variant).provenance, "invalid");
  for (const document_id of [undefined, null, "   "]) {
    const malformed = { ...hit(), document_id, metadata: { document_id: "document-A" } } as unknown as RetrieveHit;
    assert.equal(knowledgeCitation("dataset-A", malformed).provenance, "invalid");
  }
  assert.equal(knowledgeCitation(undefined, hit()).provenance, "invalid");
  assert.equal(knowledgeCitation("dataset-A", { ...hit(), dataset_id: "dataset-B" } as RetrieveHit).provenance, "invalid");
});

test("retrieval card renders an open action with the unmodified hit", () => {
  const h = hit();
  const app = host("./RetrievalResultCard.tsx", "RetrievalResultCard", { hit: h, datasetId: "dataset-A", index: 0 });
  const button = all(app.render(), "KnowledgeSourceButton")[0];
  assert.ok(button, "every retrieved hit must have a source-opening action");
  assert.equal(button.props.hit, h);
  assert.equal(button.props.datasetId, "dataset-A");
});

test("source button opens exact citation and close clears selection without a mutation", async () => {
  const app = host("./KnowledgeSourceButton.tsx", "KnowledgeSourceButton", { hit: hit(), datasetId: "dataset-A" });
  const button = all(app.render(), "button")[0];
  (button.props.onClick as () => void)();
  const opened = all(await app.flush(), "SourceDocumentDialog")[0];
  assert.deepEqual(opened.props.source, source());
  (opened.props.onClose as () => void)();
  assert.equal(all(await app.flush(), "SourceDocumentDialog")[0].props.source, null);
  assert.equal(app.reads.length, 0);
});

test("QA source action uses the answered dataset and hides retired navigation receipts", async () => {
  const props = { datasetId: "dataset-A", hitTest: {} };
  const app = host("./QATab.tsx", "QATab", props);
  const input = all(app.render(), "Textarea")[0];
  (input.props.onChange as (event: unknown) => void)({ target: { value: "question" } });
  const ready = await app.flush();
  const run = all(ready, "Button").find((b) => (b.props.onClick as { name?: string })?.name === "runQA");
  assert.ok(run);
  await (run.props.onClick as () => Promise<void>)();
  await app.flush();
  const button = all(app.current(), "KnowledgeSourceButton")[0];
  assert.ok(button, "QA citations must have a source-opening action");
  assert.equal(button.props.datasetId, "dataset-A");
  assert.equal(button.props.hit, app.qa.response.context_segments[0]);
  const changed = app.render({ ...props, datasetId: "dataset-B" });
  assert.equal(all(changed, "KnowledgeSourceButton").length, 0);
});

test("historical read uses exact escaped identifiers/hash and never current source", async () => {
  const s = { ...source(), datasetId: "dataset/A", documentId: "doc?#A" };
  const app = dialog(s); app.setGet(async () => response(s)); app.render();
  assert.match(textOf(await app.flush()), /saved paragraph/);
  assert.equal(app.reads[0].url, "/api/v1/knowledge/dataset%2FA/documents/doc%3F%23A/versions/3/source");
  assert.equal(app.reads[0].options.params.hash, s.sourceHash);
  assert.equal(app.legacyReads.length, 0);
  app.dispose(); assert.equal(app.reads[0].options.signal.aborted, true);
});

test("fixture flush waits for an owned digest beyond a fixed event-loop turn budget", async () => {
  const s = source(); const pending = deferred<ArrayBuffer>(); const app = dialog(s);
  app.setDigest(() => pending.promise); app.setGet(async () => response(s)); app.render();
  let settled = false;
  const flushing = app.flush().then((tree) => { settled = true; return tree; });
  try {
    for (let i = 0; i < 20; i++) await new Promise<void>((done) => setImmediate(done));
    assert.equal(settled, false, "flush must await owned digest work, not count event-loop turns");
  } finally {
    pending.resolve(await webcrypto.subtle.digest("SHA-256", new TextEncoder().encode(s.excerpt)));
  }
  assert.match(textOf(await flushing), /saved paragraph/);
  app.dispose();
});

for (const status of [403, 404]) test(`source ${status} on recheck conceals saved excerpt and body`, async () => {
  const s = source(); const app = dialog(s); app.setGet(async () => response(s)); app.render();
  assert.match(textOf(await app.flush()), /saved paragraph/);
  app.setGet(async () => { throw { response: { status } }; }); app.tick();
  const text = textOf(await app.flush());
  assert.doesNotMatch(text, /saved paragraph|Saved title/);
  assert.match(text, /no longer available/);
  assert.equal(app.legacyReads.length, 0); app.dispose();
});

for (const mismatch of [
  { document_id: "other-document" }, { version_number: 4 }, { content: "new version" },
]) test(`mismatched historical response stays unavailable: ${JSON.stringify(mismatch)}`, async () => {
  const s = source(); const app = dialog(s); app.setGet(async () => response(s, s.excerpt, mismatch)); app.render();
  const text = textOf(await app.flush());
  assert.match(text, /no longer available/); assert.doesNotMatch(text, /saved paragraph|new version|Saved title/);
  assert.equal(app.legacyReads.length, 0); app.dispose();
});

test("late previous citation response cannot replace the newer source", async () => {
  const old = source(); const next = source("new citation", { document_id: "document-B", source_version: 4 });
  const pending = deferred<unknown>(); const app = dialog(old); app.setGet(() => pending.promise); app.render();
  app.setGet(async () => response(next)); app.render({ source: next, onClose: () => {} });
  assert.equal(app.reads[0].options.signal.aborted, true);
  assert.match(textOf(await app.flush()), /new citation/);
  pending.resolve(response(old));
  const text = textOf(await app.flush()); assert.match(text, /new citation/); assert.doesNotMatch(text, /saved paragraph/); app.dispose();
});

for (const change of ["account", "session", "permission", "logout"]) test(`${change} change hides prior authorized source before effects and rejects stale responses`, async () => {
  const s = source(); const app = dialog(s); app.setGet(async () => response(s)); app.render();
  assert.match(textOf(await app.flush()), /saved paragraph/);
  const pending = deferred<unknown>(); app.setGet(() => pending.promise); app.tick();
  if (change === "account") app.auth.user = app.user();
  if (change === "session") app.auth.token = "synthetic-session-B";
  if (change === "permission") app.auth.user!.permissions = [];
  if (change === "logout") { app.auth.user = null; app.auth.token = ""; }
  const beforeEffects = app.render(undefined, false);
  assert.doesNotMatch(textOf(beforeEffects), /saved paragraph|Saved title/);
  app.setGet(async () => { throw { response: { status: 404 } }; }); app.render();
  pending.resolve(response(s));
  const text = textOf(await app.flush()); assert.doesNotMatch(text, /saved paragraph|Saved title/);
  assert.match(text, /no longer available/); assert.equal(app.legacyReads.length, 0); app.dispose();
});

test("newer failed recheck wins over an older pending success", async () => {
  const s = source(); const older = deferred<unknown>(); const app = dialog(s); app.setGet(() => older.promise); app.render();
  app.setGet(async () => { throw { response: { status: 404 } }; }); app.tick(); await app.flush();
  older.resolve(response(s));
  assert.doesNotMatch(textOf(await app.flush()), /saved paragraph|Saved title/); app.dispose();
});

test("unversioned Knowledge references perform no source read and do not expose a fallback excerpt", async () => {
  const app = dialog(source(undefined, { source_version: null, source_hash: null })); app.render();
  assert.doesNotMatch(textOf(await app.flush()), /saved paragraph/);
  assert.equal(app.reads.length, 0); assert.equal(app.legacyReads.length, 0); app.dispose();
});

test("legacy Assistant behavior still checks current access before showing only its saved excerpt", async () => {
  const app = dialog({ datasetId: "dataset-A", documentId: "doc-A", excerpt: "old excerpt", provenance: "legacy" });
  app.render(); const text = textOf(await app.flush());
  assert.match(text, /old excerpt/); assert.match(text, /historical full document version cannot be verified/);
  app.setActiveRead(async () => { throw { response: { status: 403 } }; }); app.tick();
  assert.doesNotMatch(textOf(await app.flush()), /old excerpt/); app.dispose();
});


async function qaReady(app: ReturnType<typeof host>) {
  const input = all(app.render(), "Textarea")[0];
  (input.props.onChange as (event: unknown) => void)({ target: { value: "question" } });
  const ready = await app.flush();
  const run = all(ready, "Button").find(button => (button.props.onClick as { name?: string })?.name === "runQA");
  assert.ok(run); return run.props.onClick as () => Promise<void>;
}

test("QA empty/error streams dispatch once, expose interruption and reuse without submitting", async () => {
  for (const stream of [async function* () {}, async function* () { throw new Error("lost response"); yield; }, async function* () { yield { event: "error", data: { message: "untrusted raw message" } }; }]) {
    const app = host("./QATab.tsx", "QATab", { datasetId: "dataset-A", hitTest: {} });
    app.setStream(stream); const run = await qaReady(app); await run(); const tree = await app.flush();
    assert.equal(app.qa.calls, 1); assert.equal(app.qa.fallbackCalls, 0);
    assert.match(textOf(tree), /qaInterrupted/); assert.doesNotMatch(textOf(tree), /untrusted raw message/);
    assert.equal(all(tree, "KnowledgeSourceButton").length, 0);
    const reuse = all(tree, "Button").find(button => textOf(button).includes("qaReuseQuestion")); assert.ok(reuse);
    (reuse.props.onClick as () => void)(); await app.flush(); assert.equal(app.qa.calls, 1);
    assert.equal(all(app.current(), "Textarea")[0].props.value, "question");
  }
});

test("QA partial failures keep text, do not populate success history and never replay", async () => {
  for (const end of ["eof", "error", "malformed"] as const) {
    const app = host("./QATab.tsx", "QATab", { datasetId: "dataset-A", hitTest: {} });
    app.setStream(async function* () {
      yield { event: "delta", data: { content: "partial answer" } };
      if (end === "error") throw new Error("lost transport");
      if (end === "malformed") yield { event: "done", data: { result: {} } };
    });
    await (await qaReady(app))(); const tree = await app.flush();
    assert.equal(all(tree, "StreamOutput")[0].props.text, "partial answer");
    assert.match(textOf(tree), /qaInterrupted/); assert.equal(app.qa.calls, 1); assert.equal(app.qa.fallbackCalls, 0);
    assert.equal(all(tree, "KnowledgeSourceButton").length, 0);
    assert.doesNotMatch(textOf(tree), /qaHistory/);
  }
});

test("QA valid empty terminal replaces provisional text without spinner or replay", async () => {
  const app = host("./QATab.tsx", "QATab", { datasetId: "dataset-A", hitTest: {} });
  app.qa.response.answer = ""; app.qa.response.context_segments = [];
  app.setStream(async function* () { yield { event: "delta", data: { content: "provisional" } }; yield { event: "done", data: { result: app.qa.response } }; });
  await (await qaReady(app))(); const tree = await app.flush();
  assert.match(textOf(tree), /qaEmptyAnswer/); assert.doesNotMatch(textOf(tree), /qaInterrupted|qaGeneratingAnswer/);
  assert.equal(app.qa.calls, 1); assert.equal(app.qa.fallbackCalls, 0);
});

test("QA synchronous double submit uses one stream; navigation aborts and fences late result", async () => {
  const gate = deferred<void>(); const props = { datasetId: "dataset-A", hitTest: {} };
  const app = host("./QATab.tsx", "QATab", props);
  app.setStream(async function* () { await gate.promise; yield { event: "done", data: { result: app.qa.response } }; });
  const run = await qaReady(app); const pending = run(); await run(); assert.equal(app.qa.calls, 1);
  app.render({ ...props, datasetId: "dataset-B" }); assert.equal(app.qa.signals[0].aborted, true);
  gate.resolve(); await pending; const tree = await app.flush();
  assert.equal(all(tree, "KnowledgeSourceButton").length, 0); assert.doesNotMatch(textOf(tree), /saved answer/);
});

test("QA revoked authority and unmount do not accept late service completion", async () => {
  for (const dispose of [false, true]) {
    let authorized = true; const identityScope = { isCurrent: () => authorized };
    const gate = deferred<void>(); const app = host("./QATab.tsx", "QATab", { datasetId: "dataset-A", identityScope, hitTest: {} });
    app.setStream(async function* () { await gate.promise; yield { event: "done", data: { result: app.qa.response } }; });
    const pending = (await qaReady(app))();
    if (dispose) { app.dispose(); assert.equal(app.qa.signals[0].aborted, true); } else authorized = false;
    gate.resolve(); await pending;
    if (!dispose) { app.render(); assert.equal(all(app.current(), "KnowledgeSourceButton").length, 0); }
    assert.equal(app.qa.fallbackCalls, 0);
  }
});
