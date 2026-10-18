import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

// The entire production workspace executes here. React scheduling, UI primitives
// and transports are controlled; this is not browser or live-server evidence.
type Tree = { type: unknown; key?: string; props: Record<string, unknown> };
type Values = { name: string; description: string; spec: ReturnType<typeof spec> };
const spec = (welcome = "Original welcome") => ({
  schema_version: "agent-spec/v1", identity: { welcome_message: welcome, suggested_prompts: [] },
  instructions: "Answer the question", model: { model_id: "fixture-model" },
  capabilities: [], knowledge: [], memory: { mode: "session" },
});
const draft = (revision: number, welcome?: string) => ({ revision, spec: spec(welcome), updated_at: "2026-10-12T12:00:00Z" });
const agent = (name = "Original name", description = "Original description") => ({
  agent_id: "fixture-agent", name, description, caller_role: "owner", status: "draft", draft_revision: 1,
});
function all(tree: unknown, predicate: (node: Tree) => boolean): Tree[] {
  if (!tree || typeof tree !== "object") return [];
  if (Array.isArray(tree)) return tree.flatMap(child => all(child, predicate));
  const node = tree as Tree;
  return [...(predicate(node) ? [node] : []), ...all(node.props?.children, predicate), ...all(node.props?.action, predicate)];
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}
function host() {
  const slots: unknown[] = [];
  let cursor = 0;
  let changed = false;
  let effects: (() => void)[] = [];
  let pending = false;
  const cleanups = new Map<number, () => void>();
  let selectedId = "fixture-agent";
  const writes: unknown[][] = [];
  const notices: string[] = [];
  const cache = new Map<string, unknown>();
  let save: (...args: unknown[]) => Promise<unknown> = async () => draft(2);
  let readAgent = async () => agent("Server name", "Server description");
  let readDraft = async () => draft(2, "Server welcome");
  let initialAgent = agent();
  let initialDraft = draft(1);
  const react = {
    lazy: () => "AgentReleasePanel", Suspense: "Suspense",
    useState(initial: unknown) {
      const index = cursor++;
      if (!(index in slots)) slots[index] = typeof initial === "function" ? initial() : initial;
      return [slots[index], (next: unknown) => {
        const value = typeof next === "function" ? next(slots[index]) : next;
        if (!Object.is(value, slots[index])) { slots[index] = value; changed = true; }
      }];
    },
    useRef(initial: unknown) { const index = cursor++; return slots[index] ?? (slots[index] = { current: initial }); },
    useEffect(effect: () => void | (() => void), deps: unknown[]) {
      const index = cursor++;
      const previous = slots[index] as unknown[] | undefined;
      if (!previous || deps.some((value, i) => !Object.is(value, previous[i]))) {
        slots[index] = deps;
        effects.push(() => {
          cleanups.get(index)?.();
          const cleanup = effect();
          if (cleanup) cleanups.set(index, cleanup); else cleanups.delete(index);
        });
      }
    },
  };
  const jsx = (type: unknown, props: Record<string, unknown>, key?: string) => ({ type, props, key });
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const modules: Record<string, unknown> = {
    react, "react/jsx-runtime": { jsx, jsxs: jsx }, "lucide-react": named,
    antd: { ...Object.fromEntries(["Alert", "Button", "Checkbox", "Drawer", "InputNumber", "Select", "Skeleton", "Slider", "Tabs"].map(key => [key, key])),
      Form: Object.assign(() => {}, { Item: "Form.Item" }), Input: Object.assign(() => {}, { TextArea: "TextArea" }),
      Typography: named, App: { useApp: () => ({ message: { success: (text: string) => notices.push(text) } }) } },
    "react-router-dom": { useParams: () => ({ agentId: selectedId }), useLocation: () => ({ pathname: "/agents/fixture-agent" }), useNavigate: () => () => {} },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key, i18n: { language: "en-US" } }) },
    "@tanstack/react-query": {
      useQuery: ({ queryKey }: { queryKey: string[] }) => ({ data: queryKey[0] === "agent"
        ? queryKey.length === 2 ? { ...initialAgent, agent_id: selectedId } : queryKey[2] === "draft" ? initialDraft : [] : [] }),
      useQueryClient: () => ({ setQueryData: (key: string[], value: unknown) => cache.set(JSON.stringify(key), value), invalidateQueries: async () => {} }),
      useMutation: (options: { mutationFn: (payload: unknown) => Promise<unknown>; onMutate: () => void; onSuccess: (data: unknown, payload: unknown) => void; onError: (error: unknown) => void }) => ({
        isPending: pending,
        mutate(payload: unknown) {
          pending = true; options.onMutate();
          void options.mutationFn(payload).then(data => options.onSuccess(data, payload), error => options.onError(error)).finally(() => { pending = false; });
        },
      }),
    },
    "@/api/agents": {
      agentErrorDetail: (error: unknown) => error,
      updateAgentDraft: async (...args: unknown[]) => { writes.push(args); return save(...args); },
      getAgent: () => readAgent(), getAgentDraft: () => readDraft(),
    },
    "./agentCatalogPresentation": {}, "./AgentPreviewPanel": { AgentPreviewPanel: "AgentPreviewPanel" },
    "./AgentReleasePanel": { AgentReleasePanel: "AgentReleasePanel" }, "./agent-studio.css": {},
  };
  const compiled: { Workspace?: (props: unknown) => Tree; AgentStudioPage?: () => Tree } = {};
  const source = readFileSync(new URL("./AgentStudioPage.tsx", import.meta.url), "utf8");
  vm.runInNewContext(ts.transpileModule(source + "\nexports.Workspace = AgentStudioWorkspace;", {
    compilerOptions: { jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
  }).outputText, {
    exports: compiled, require: (id: string) => { assert.ok(id in modules, `Unexpected module: ${id}`); return modules[id]; },
    window: { addEventListener() {}, removeEventListener() {} }, URL, requestAnimationFrame: () => {},
  });
  const h = {
    writes, notices, cache,
    unmount() { cleanups.forEach(cleanup => cleanup()); cleanups.clear(); },
    reopen() {
      h.unmount(); slots.length = 0;
      initialAgent = (cache.get(JSON.stringify(["agent", "fixture-agent"])) as typeof initialAgent) ?? initialAgent;
      initialDraft = (cache.get(JSON.stringify(["agent", "fixture-agent", "draft"])) as typeof initialDraft) ?? initialDraft;
      return h.render();
    },
    page(id: string) { selectedId = id; return compiled.AgentStudioPage!(); },
    render() {
      for (let attempt = 0; attempt < 20; attempt++) {
        cursor = 0; changed = false; effects = [];
        const tree = compiled.Workspace!({ initialAgent, initialDraft, versions: [], catalog: {
          tools: [], mcp: [], skills: [], connectors: [], datasets: [], evalDatasets: [], models: [], degraded: [], retry() {},
        } });
        effects.forEach(effect => effect());
        if (!changed) return tree;
      }
      throw new Error("Workspace did not settle");
    },
    async flush() { for (let i = 0; i < 20; i++) { await Promise.resolve(); h.render(); } },
    button(label: string) { const node = all(h.render(), node => node.type === "Button" && node.props.children === label)[0]; assert.ok(node, label); return node; },
    click(label: string) { const button = h.button(label); assert.ok(!button.props.disabled); (button.props.onClick as () => void)(); h.render(); },
    edit(label: string, value: string) {
      const input = all(h.render(), node => node.props?.["aria-label"] === label)[0]; assert.ok(input, label);
      (input.props.onChange as (event: unknown) => void)({ target: { value } }); h.render();
    },
    value(label: string) { return all(h.render(), node => node.props?.["aria-label"] === label)[0]?.props.value; },
    preview() { return all(h.render(), node => node.type === "AgentPreviewPanel")[0].props; },
    state() { return all(h.render(), node => String(node.props?.className).startsWith("agent-save-state"))[0].props.children; },
    setSave(handler: typeof save) { save = handler; },
    setRead(agentHandler: typeof readAgent, draftHandler: typeof readDraft) { readAgent = agentHandler; readDraft = draftHandler; },
  };
  h.render(); return h;
}
const nameField = "agents.studio.overview.name";
const descriptionField = "agents.studio.overview.descriptionLabel";
const welcomeField = "agents.studio.overview.welcome";
const saveButton = "agents.common.saveDraft";
async function conflict(h: ReturnType<typeof host>) {
  h.edit(nameField, "Local name");
  h.setSave(async () => { throw { status: 409, code: "AGENT_DRAFT_CONFLICT", currentRevision: 2 }; });
  h.click(saveButton); await h.flush();
  assert.equal(h.state(), "agents.studio.saveStates.conflict");
}

test("conflict reapply preserves name, description and spec edits made during readback", async () => {
  const h = host(); await conflict(h);
  const freshAgent = deferred<ReturnType<typeof agent>>();
  const freshDraft = deferred<ReturnType<typeof draft>>();
  h.setRead(() => freshAgent.promise, () => freshDraft.promise);
  h.click("agents.studio.reloadReapply");
  h.edit(nameField, "Newest name"); h.edit(descriptionField, "Newest description"); h.edit(welcomeField, "Newest welcome");
  freshAgent.resolve(agent("Server name", "Server description")); freshDraft.resolve(draft(2, "Server welcome"));
  await h.flush();
  assert.equal(h.value(nameField), "Newest name");
  assert.equal(h.value(descriptionField), "Newest description");
  assert.equal(h.value(welcomeField), "Newest welcome");
  assert.equal(h.preview().draftRevision, 2); assert.equal(h.preview().dirty, true);
  assert.equal((h.preview().savedSpec as Values["spec"]).identity.welcome_message, "Server welcome");
  assert.equal(h.writes.length, 1); assert.deepEqual(h.notices, []);
  h.setSave(async () => draft(3, "Newest welcome")); h.click(saveButton); await h.flush();
  assert.equal(h.writes[1][1], 2); assert.equal(h.preview().dirty, false);
  assert.equal(h.state(), "agents.studio.saveStates.saved");
});

test("validation rejection keeps local fields and explicit retry saves corrected input", async () => {
  const h = host(); h.edit(nameField, "Invalid name");
  h.setSave(async () => { throw { status: 422, message: "Name rejected", errors: [{ field: "name", message: "Fix name" }] }; });
  h.click(saveButton); await h.flush();
  assert.equal(h.value(nameField), "Invalid name"); assert.equal(h.preview().dirty, true);
  assert.deepEqual(h.notices, []); assert.equal(h.writes.length, 1);
  h.edit(nameField, "Corrected name"); h.setSave(async () => draft(2));
  h.click(saveButton); await h.flush(); assert.equal(h.preview().dirty, false);
  assert.equal(h.writes.length, 2); assert.equal(h.notices.length, 1);
});

test("lost save response remains unsaved until explicit conflict readback confirms matching content", async () => {
  const h = host(); h.edit(nameField, "Saved remotely");
  h.setSave(async () => { throw { message: "Response lost" }; });
  h.click(saveButton); await h.flush();
  assert.equal(h.value(nameField), "Saved remotely"); assert.equal(h.preview().dirty, true);
  assert.notEqual(h.state(), "agents.studio.saveStates.saved"); assert.deepEqual(h.notices, []);
  h.setSave(async () => { throw { status: 409, code: "AGENT_DRAFT_CONFLICT", currentRevision: 2 }; });
  h.click("agents.studio.retrySave"); await h.flush();
  h.setRead(async () => agent("Saved remotely"), async () => draft(2));
  h.click("agents.studio.reloadReapply"); await h.flush();
  assert.equal(h.preview().dirty, false); assert.equal(h.preview().draftRevision, 2);
  assert.equal(h.writes.length, 2); assert.deepEqual(h.notices, []);
  h.reopen(); assert.equal(h.value(nameField), "Saved remotely"); assert.equal(h.preview().draftRevision, 2);
});

test("acknowledged save preserves later edits and caches only acknowledged values for reopen", async () => {
  const h = host(); const saved = deferred<ReturnType<typeof draft>>();
  h.edit(nameField, "Submitted name"); h.setSave(() => saved.promise); h.click(saveButton);
  h.edit(nameField, "Later unsaved name"); saved.resolve(draft(2)); await h.flush();
  assert.equal(h.value(nameField), "Later unsaved name"); assert.equal(h.preview().dirty, true);
  assert.equal(h.state(), "agents.studio.saveStates.dirty");
  assert.equal((h.cache.get(JSON.stringify(["agent", "fixture-agent"])) as Values).name, "Submitted name");
  assert.equal((h.cache.get(JSON.stringify(["agent", "fixture-agent", "draft"])) as { revision: number }).revision, 2);
});


test("reapply without new typing retains local edits but reopen uses only fresh server baseline", async () => {
  const h = host(); await conflict(h);
  h.click("agents.studio.reloadReapply"); await h.flush();
  assert.equal(h.value(nameField), "Local name"); assert.equal(h.preview().dirty, true);
  assert.equal(h.writes.length, 1);
  h.reopen(); assert.equal(h.value(nameField), "Server name"); assert.equal(h.preview().dirty, false);
  assert.equal(h.preview().draftRevision, 2);
});

test("explicit reload supersedes earlier reapply and caches the selected readback", async () => {
  const h = host(); await conflict(h);
  const old = deferred<ReturnType<typeof draft>>();
  h.setRead(async () => agent("Older remote"), () => old.promise);
  h.click("agents.studio.reloadReapply");
  h.setRead(async () => agent("Latest remote"), async () => draft(3, "Latest welcome"));
  h.click("agents.studio.reloadDraft"); await h.flush();
  old.resolve(draft(2, "Older welcome")); await h.flush();
  assert.equal(h.value(nameField), "Latest remote"); assert.equal(h.preview().draftRevision, 3);
  assert.equal(h.preview().dirty, false); assert.equal(h.writes.length, 1);
  h.reopen(); assert.equal(h.value(nameField), "Latest remote"); assert.equal(h.preview().draftRevision, 3);
});

test("discard supersedes pending reapply and does not revive conflict or local changes", async () => {
  const h = host(); await conflict(h); const old = deferred<ReturnType<typeof draft>>();
  h.setRead(async () => agent("Late remote"), () => old.promise);
  h.click("agents.studio.reloadReapply"); h.click("agents.studio.discard");
  old.resolve(draft(2)); await h.flush();
  assert.equal(h.value(nameField), "Original name"); assert.equal(h.preview().draftRevision, 1);
  assert.equal(h.preview().dirty, false); assert.equal(h.cache.size, 0);
});

test("explicit save supersedes pending reapply without replacing acknowledged revision", async () => {
  const h = host(); await conflict(h); const old = deferred<ReturnType<typeof draft>>();
  h.setRead(async () => agent("Late remote"), () => old.promise);
  h.click("agents.studio.reloadReapply"); h.setSave(async () => draft(3));
  h.click(saveButton); await h.flush(); old.resolve(draft(2)); await h.flush();
  assert.equal(h.value(nameField), "Local name"); assert.equal(h.preview().draftRevision, 3);
  assert.equal(h.preview().dirty, false); h.reopen(); assert.equal(h.preview().draftRevision, 3);
});

test("readback failure preserves current edit and conflict and allows explicit retry", async () => {
  const h = host(); await conflict(h);
  h.setRead(async () => { throw { message: "Read unavailable" }; }, async () => draft(2));
  h.click("agents.studio.reloadReapply"); h.edit(nameField, "Still editing"); await h.flush();
  assert.equal(h.value(nameField), "Still editing"); assert.equal(h.preview().draftRevision, 1);
  assert.equal(h.state(), "agents.studio.saveStates.conflict"); assert.equal(h.cache.size, 0);
  assert.deepEqual(h.notices, []); assert.equal(h.writes.length, 1);
  const alert = all(h.render(), node => node.props?.className === "agent-save-error")[0];
  assert.equal(alert.props.description, "Read unavailable");
  h.setRead(async () => agent(), async () => draft(2)); h.click("agents.studio.reloadReapply"); await h.flush();
  assert.equal(h.value(nameField), "Still editing"); assert.equal(h.preview().draftRevision, 2);
  assert.equal(all(h.render(), node => node.props?.className === "agent-save-error").length, 0);
});

test("unmount ignores a late readback and page keys editor by selected agent", async () => {
  const h = host(); await conflict(h); const old = deferred<ReturnType<typeof draft>>();
  h.setRead(async () => agent("Late remote"), () => old.promise);
  h.click("agents.studio.reloadReapply"); h.unmount(); old.resolve(draft(2)); await h.flush();
  assert.equal(h.cache.size, 0); assert.equal(h.preview().draftRevision, 1); assert.deepEqual(h.notices, []);
  const first = h.page("agent-one"); const second = h.page("agent-two");
  assert.equal(first.key, "agent-one"); assert.equal(second.key, "agent-two");
});
