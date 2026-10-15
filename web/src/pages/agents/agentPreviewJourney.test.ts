import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import * as recovery from "./agentPreviewRecovery.ts";
import * as events from "./agentPreviewEvents.ts";

// Execute the complete production panel and its real recovery helpers. Only
// React scheduling, UI primitives, storage and server transports are controlled.
// These are offline interaction regressions, not browser or live Runtime proof.
type Tree = { type: unknown; props: Record<string, unknown> };
type Session = {
  session_id: string; agent_id: string; agent_version_id: string | null;
  draft_revision: number | null; publication_id: null; channel: "preview";
  runtime_fingerprint: string; request_id: string;
};
type Message = { role: string; content: string; metadata: { runtime_run_id: string } };
const spec = (model: string) => ({
  schema_version: "agent-spec/v1", identity: {}, instructions: "Answer safely.",
  model: { model_id: model }, capabilities: [], knowledge: [], memory: { mode: "session" },
});
const session = (id: string, revision: number | null, version: string | null = null): Session => ({
  session_id: id, agent_id: "agent-one", agent_version_id: version,
  draft_revision: revision, publication_id: null, channel: "preview",
  runtime_fingerprint: `sha256:${id}`, request_id: "fixture-request",
});
function all(tree: unknown, predicate: (node: Tree) => boolean): Tree[] {
  if (!tree || typeof tree !== "object") return [];
  if (Array.isArray(tree)) return tree.flatMap(child => all(child, predicate));
  const node = tree as Tree;
  return [...(predicate(node) ? [node] : []), ...all(node.props?.children, predicate)];
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}
function host(options: { status?: string; version?: boolean } = {}) {
  const first = session("session-r1", options.version ? null : 1, options.version ? "version-one" : null);
  const sessions = new Map([[first.session_id, first]]);
  const histories = new Map<string, Message[]>([[first.session_id, [
    { role: "user", content: "Original question", metadata: { runtime_run_id: "run-r1" } },
  ]]]);
  const run = { run_id: "run-r1", session_id: first.session_id, harness_thread_id: "thread-r1", status: options.status ?? "running" };
  let runRead: () => Promise<{ run: typeof run }> = async () => ({ run: { ...run } });
  let approvalPending = false;
  const reads: string[] = [];
  const created: Array<{ agentId: string; revision?: number; version?: string }> = [];
  const dispatched: unknown[] = [];
  const cancellations: unknown[][] = [];
  const decisions: unknown[][] = [];
  const artifactReads: string[] = [];
  const storage = new Map([[recovery.previewLocatorKey("agent-one"), JSON.stringify(recovery.previewLocator(first))]]);
  const waits: (() => void)[] = [];
  const slots: unknown[] = [];
  const cleanups = new Map<number, () => void>();
  let cursor = 0;
  let dirty = false;
  let effects: (() => void)[] = [];
  let identity = 0;
  let props = { agentId: "agent-one", agentName: "Fixture Agent", draftRevision: 1, savedSpec: spec("model-r1"), dirty: false,
    versions: [{ agent_version_id: "version-one", version_number: 1, source_draft_revision: 1, spec: spec("published-model") }] };
  const same = (a?: unknown[], b?: unknown[]) => a?.length === b?.length && a?.every((value, i) => Object.is(value, b?.[i]));
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
      const index = cursor++; const old = slots[index] as { deps?: unknown[]; value: unknown } | undefined;
      if (!old || !same(old.deps, deps)) slots[index] = { deps, value: compute() };
      return (slots[index] as { value: unknown }).value;
    },
    useEffect(effect: () => void | (() => void), deps: unknown[]) {
      const index = cursor++;
      if (!same(slots[index] as unknown[], deps)) {
        slots[index] = deps;
        effects.push(() => {
          cleanups.get(index)?.();
          const cleanup = effect();
          if (cleanup) cleanups.set(index, cleanup); else cleanups.delete(index);
        });
      }
    },
  };
  const jsx = (type: unknown, properties: Record<string, unknown>) => ({ type, props: properties });
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const translate = (key: string, values?: Record<string, unknown>) => key + (values?.model ? `:${values.model}` : "");
  const modules: Record<string, unknown> = {
    react: { ...react, useCallback: (callback: () => unknown, deps: unknown[]) => react.useMemo(() => callback, deps) },
    "react/jsx-runtime": { jsx, jsxs: jsx }, "lucide-react": named,
    antd: { Alert: "Alert", Button: "Button", Input: { TextArea: "TextArea" }, Select: "Select", Spin: "Spin", Tag: "Tag", Typography: named },
    "react-router-dom": { Link: "Link" }, "react-i18next": { useTranslation: () => ({ t: translate }) },
    "./agentPreviewRecovery": recovery, "./agentPreviewEvents": events,
    "@/lib/authenticatedDownload": { artifactAccessErrorKey: () => "artifact-error", downloadAssistantArtifact: async () => {} },
    "@/lib/api": { api: { async get(path: string) {
      assert.equal(path, `/api/v1/agents/agent-one/preview/sessions/${first.session_id}`);
      return { data: { ...first } };
    } } },
    "@/api/agents": {
      agentErrorDetail: (error: Error) => ({ code: "", message: error.message }),
      async createDraftPreviewSession(agentId: string, revision: number) {
        created.push({ agentId, revision }); const next = session(`session-r${revision}`, revision);
        sessions.set(next.session_id, next); histories.set(next.session_id, []); return next;
      },
      async createVersionPreviewSession(agentId: string, version: string) {
        created.push({ agentId, version }); const next = session("session-version-new", null, version);
        sessions.set(next.session_id, next); histories.set(next.session_id, []); return next;
      },
      async *streamAgentPreview(input: unknown) {
        dispatched.push(input); yield { event_type: "run_finished", data: { status: "succeeded" } };
      },
    },
    "@/api/assistant": {
      async getSessionHistory(id: string) { assert.ok(sessions.has(id)); return { messages: histories.get(id) ?? [] }; },
      async getSessionArtifacts(id: string) { artifactReads.push(id); return []; },
      async getAssistantRunStatus(id: string) { reads.push(id); assert.equal(id, run.run_id); return runRead(); },
      async cancelTask(...args: unknown[]) { cancellations.push(args); },
      getArtifactDownloadUrl: (id: string) => id,
    },
    "@/api/agentThreads": {
      async getAgentRuntimeThread(id: string) { assert.equal(id, "thread-r1"); return { session_id: first.session_id }; },
      async getAgentRuntimeV2RunSnapshot(threadId: string, runId: string) {
        assert.equal(threadId, "thread-r1"); assert.equal(runId, "run-r1");
        return { terminalStatus: null, pendingApproval: approvalPending ? { threadId, approvalId: "approval-r1" } : null };
      },
      async getAgentRuntimeApproval(threadId: string, approvalId: string) {
        assert.equal(threadId, "thread-r1"); assert.equal(approvalId, "approval-r1");
        return { approval: { status: "pending", run_id: "run-r1" }, preview: { can_approve: true, tool_name: "fixture-tool" } };
      },
      async decideAgentRuntimeApproval(...args: unknown[]) { decisions.push(args); approvalPending = false; run.status = args[2] ? "succeeded" : "failed"; },
    },
  };
  const compiled: { AgentPreviewPanel?: (input: typeof props) => Tree } = {};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./AgentPreviewPanel.tsx", import.meta.url), "utf8"), {
    compilerOptions: { jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
  }).outputText, {
    exports: compiled, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; },
    window: { confirm: () => true, setTimeout: (callback: () => void) => { waits.push(callback); }, sessionStorage: {
      getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value), removeItem: (key: string) => storage.delete(key),
    } }, crypto: { randomUUID: () => `fixture-${++identity}` }, AbortController, DOMException, Error,
  });
  const h = {
    reads, created, dispatched, cancellations, decisions, artifactReads, storage, run,
    render() {
      for (let attempt = 0; attempt < 20; attempt++) {
        cursor = 0; dirty = false; effects = [];
        const tree = compiled.AgentPreviewPanel!(props); effects.forEach(effect => effect());
        if (!dirty) return tree;
      }
      throw new Error("Preview panel did not settle");
    },
    async flush() {
      for (let attempt = 0; attempt < 30; attempt++) { await Promise.resolve(); h.render(); }
    },
    nodes(predicate: (node: Tree) => boolean) { return all(h.render(), predicate); },
    buttons(suffix: string) { return h.nodes(node => node.type === "Button" && node.props.children === `agents.preview.${suffix}`); },
    button(suffix: string) { const value = h.buttons(suffix)[0]; assert.ok(value, `Missing ${suffix} button`); return value; },
    async click(suffix: string) {
      const value = h.button(suffix); assert.equal(Boolean(value.props.disabled), false, `${suffix} disabled`);
      (value.props.onClick as () => void)(); await h.flush();
    },
    target() { return h.nodes(node => node.type === "Select")[0]; },
    async select(value: string) {
      assert.equal(Boolean(h.target().props.disabled), false, "target switching disabled");
      (h.target().props.onChange as (target: string) => void)(value); await h.flush();
    },
    saveRevision(revision: number) { props = { ...props, draftRevision: revision, savedSpec: spec(`model-r${revision}`) }; h.render(); },
    approval(value: boolean) { approvalPending = value; },
    setRead(value: typeof runRead) { runRead = value; },
    finish(status = "succeeded") {
      run.status = status;
      histories.get(first.session_id)!.push({ role: "assistant", content: "Original run final answer", metadata: { runtime_run_id: "run-r1" } });
    },
    text(value: string) { return h.nodes(node => node.props?.children === value).length > 0; },
    trace() { return h.nodes(node => node.type === "Link")[0]?.props.to; },
    async wakeReads() { waits.splice(0).forEach(callback => callback()); await h.flush(); },
    dispose() { cleanups.forEach(cleanup => cleanup()); },
  };
  return h;
}

async function leaveRunningDraft(h: ReturnType<typeof host>) {
  await h.flush(); assert.equal(h.target().props.value, "draft");
  h.saveRevision(2); assert.equal(h.target().props.value, "draft:1");
  await h.click("currentDraft"); assert.equal(h.target().props.value, "draft");
  assert.deepEqual(h.created, [{ agentId: "agent-one", revision: 2 }]);
  assert.equal(h.dispatched.length, 0);
}

test("r1 to r2 to r1 restores the original run identity and recovery controls without rerunning", async () => {
  const h = host(); try {
    await leaveRunningDraft(h); await h.select("draft:1");
    assert.equal(Boolean(h.button("refreshStatus").props.disabled), false);
    assert.equal(Boolean(h.button("stop").props.disabled), false);
    assert.equal(h.target().props.disabled, true);
    assert.equal(h.text("agents.preview.summaryModel:model-r1"), true);
    assert.equal(h.trace(), "/eval?tab=traces&family=assistant&session_id=session-r1");
    await h.click("refreshStatus");
    assert.ok(h.reads.length > 2); assert.ok(h.reads.every(id => id === "run-r1"));
    assert.equal(h.dispatched.length, 0);
  } finally { h.dispose(); }
});

test("returning to r1 discovers its later approval and rejection without dispatching another run", async () => {
  const h = host(); try {
    await leaveRunningDraft(h); h.approval(true); await h.select("draft:1");
    assert.equal(Boolean(h.button("approve").props.disabled), false);
    await h.click("reject");
    assert.deepEqual(h.decisions, [["thread-r1", "approval-r1", false]]);
    assert.equal(h.buttons("approve").length, 0);
    assert.equal(h.buttons("stop").length, 0);
    assert.equal(h.dispatched.length, 0);
  } finally { h.dispose(); }
});

test("returning to r1 refreshes its terminal answer and artifact facts while preserving r2", async () => {
  const h = host(); try {
    await leaveRunningDraft(h); h.finish(); await h.select("draft:1");
    assert.equal(h.text("Original run final answer"), true);
    assert.equal(h.buttons("stop").length, 0);
    assert.equal(h.artifactReads.at(-1), "session-r1");
    await h.select("draft");
    assert.equal(h.text("Original run final answer"), false);
    assert.equal(h.trace(), "/eval?tab=traces&family=assistant&session_id=session-r2");
    assert.equal(h.created.length, 1); assert.equal(h.dispatched.length, 0);
  } finally { h.dispose(); }
});

test("accepted cancellation remains pending across r2 and cannot be submitted twice on return", async () => {
  const h = host(); try {
    await h.flush(); await h.click("stop");
    assert.deepEqual(h.cancellations, [["run-r1", "user_requested_stop"]]);
    h.saveRevision(2); await h.click("currentDraft"); await h.select("draft:1");
    assert.equal(h.button("stop").props.disabled, true);
    await h.click("refreshStatus");
    assert.equal(h.button("stop").props.disabled, true);
    h.finish("cancelled"); await h.click("refreshStatus");
    assert.equal(h.buttons("stop").length, 0); assert.equal(h.cancellations.length, 1);
    assert.equal(h.dispatched.length, 0);
  } finally { h.dispose(); }
});

test("unavailable original status preserves recovery controls and never silently clears or reruns", async () => {
  const h = host(); try {
    await leaveRunningDraft(h); h.setRead(async () => { throw new Error("fixture unavailable"); });
    await h.select("draft:1"); await h.click("refreshStatus");
    assert.equal(h.button("clear").props.disabled, true);
    assert.equal(h.target().props.disabled, true);
    assert.equal(Boolean(h.button("stop").props.disabled), false);
    assert.equal(h.dispatched.length, 0);
  } finally { h.dispose(); }
});

test("late r1 read cannot overwrite the active r2 transcript or locator", async () => {
  const h = host(); try {
    await h.flush(); const pending = deferred<{ run: typeof h.run }>();
    h.setRead(() => pending.promise); await h.wakeReads();
    h.saveRevision(2); await h.click("currentDraft");
    h.finish(); pending.resolve({ run: { ...h.run } }); await h.flush();
    assert.equal(h.target().props.value, "draft");
    assert.equal(h.text("Original run final answer"), false);
    const pin = JSON.parse(h.storage.get(recovery.previewLocatorKey("agent-one"))!);
    assert.equal(pin.session_id, "session-r2"); assert.equal(pin.draft_revision, 2);
    assert.equal(h.dispatched.length, 0);
  } finally { h.dispose(); }
});

test("published version preview remains pinned when draft revisions change and targets are revisited", async () => {
  const h = host({ status: "succeeded", version: true }); try {
    await h.flush(); h.saveRevision(2);
    assert.equal(h.target().props.value, "version-one");
    assert.equal(h.text("agents.preview.summaryModel:published-model"), true);
    await h.select("draft"); await h.select("version-one");
    assert.equal(h.trace(), "/eval?tab=traces&family=assistant&session_id=session-r1");
    const pin = JSON.parse(h.storage.get(recovery.previewLocatorKey("agent-one"))!);
    assert.equal(pin.agent_version_id, "version-one"); assert.equal(pin.draft_revision, null);
    assert.equal(h.created.length, 0); assert.equal(h.dispatched.length, 0);
  } finally { h.dispose(); }
});
