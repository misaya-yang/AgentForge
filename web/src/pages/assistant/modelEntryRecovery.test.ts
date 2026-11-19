import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

type Tree = { type: unknown; props: Record<string, unknown> };
type Slot = { value?: unknown; deps?: unknown[]; cleanup?: () => void };
function nodes(value: unknown): Tree[] { if (Array.isArray(value)) return value.flatMap(nodes); if (!value || typeof value !== "object") return []; const node = value as Tree; return [node, ...nodes(node.props?.children)]; }
const noop = () => {};
const settle = () => new Promise(resolve => setImmediate(resolve));
const model = (provider = "qwen", id = "same") => ({ id, provider, name: id, access_level: "public", supports_vision: false });
const config = (providers = ["qwen"]) => ({ default_model_id: "same", available_providers: providers, kb_enabled: false, web_search_enabled: false });
function host() {
  let auth = { token: "a", hydrated: true, isAuthenticated: true, user: { user_id: "a", roles: ["member"], permissions: [] as string[], tier: "normal" } };
  const subscribers = new Set<(current: typeof auth, prior: typeof auth) => void>();
  let slots: Slot[] = []; const hookSlots: Slot[] = []; let index = 0; let uuid = 0; let effects: Array<() => void> = [];
  const requests: Array<{ path: string; signal: AbortSignal; resolve: (value: unknown) => void; reject: (error: Error) => void }> = [];
  const memo = (fn: () => unknown, deps: unknown[]) => { const i = index++; const previous = slots[i]; if (!previous || deps.some((d, j) => !Object.is(d, previous.deps?.[j]))) slots[i] = { value: fn(), deps }; return slots[i].value; };
  const effect = (fn: () => (() => void) | undefined, deps: unknown[]) => { const i = index++; const previous = slots[i]; if (!previous || deps.some((d, j) => !Object.is(d, previous.deps?.[j]))) { const next: Slot = { deps }; slots[i] = next; effects.push(() => { previous?.cleanup?.(); next.cleanup = fn(); }); } };
  const named = new Proxy({}, { get: (_, name) => String(name) }); const jsx = (type: unknown, props: Record<string, unknown>) => ({ type, props });
  const react = { useState(initial: unknown) { const i = index++; const owner = slots; owner[i] ??= { value: initial }; return [owner[i].value, (next: unknown) => { owner[i].value = typeof next === "function" ? next(owner[i].value) : next; }]; }, useMemo: memo, useCallback: (fn: unknown, deps: unknown[]) => memo(() => fn, deps), useRef: (value: unknown) => memo(() => ({ current: value }), []), useEffect: effect, useLayoutEffect: effect };
  const modules: Record<string, unknown> = {
    react, "react/jsx-runtime": { jsx, jsxs: jsx }, "lucide-react": named, "react-router-dom": { Link: "Link" },
    "react-i18next": { useTranslation: () => ({ t: (key: string, fallback?: string) => fallback || key }) },
    "motion/react": { motion: named, AnimatePresence: "AnimatePresence", useReducedMotion: () => true },
    "@/lib/utils": { generateUUID: () => `scope-${++uuid}`, cn: (...values: unknown[]) => values.filter(Boolean).join(" ") },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope: (user: unknown) => JSON.stringify(user) },
    "@/store/useAuthStore": { useAuthStore: Object.assign((selector: (state: typeof auth) => unknown) => selector(auth), { getState: () => auth, subscribe: (fn: (current: typeof auth, prior: typeof auth) => void) => { subscribers.add(fn); return () => subscribers.delete(fn); } }) },
    "@/lib/api": { api: { get: (path: string, { signal }: { signal: AbortSignal }) => new Promise((resolve, reject) => requests.push({ path, signal, resolve: value => resolve({ data: value }), reject })) } },
    "@/pages/assistant/sse-events": { SSEEventType: {} }, "@/api/files": { isImageFile: () => false, formatFileSize: () => "1 KB" },
    "../components/QuickActionsMenu": { QuickActionsMenu: "QuickActionsMenu" }, "../components/StyleSelector": { StyleSelector: "StyleSelector" },
  };
  function compile(path: string, source?: string) { const exports: Record<string, (...args: never[]) => unknown> = {}; vm.runInNewContext(ts.transpileModule((source ?? readFileSync(new URL(path, import.meta.url), "utf8")).replaceAll('import.meta.env.VITE_ASSISTANT_UI_V2', '"true"'), { compilerOptions: { jsx: ts.JsxEmit.ReactJSX, module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText, { exports, AbortController, require: (id: string) => { if (id.startsWith("@/components/ui/")) return named; assert.ok(id in modules, id); return modules[id]; } }); return exports; }
  modules["@/api/assistant"] = compile("../../api/assistant.ts"); modules["../modelSelection"] = compile("./modelSelection.ts");
  const hook = compile("./hooks/useAssistantModelCatalog.ts").useAssistantModelCatalog;
  const status = compile("./components/AssistantModelStatus.tsx").AssistantModelStatus as (props: unknown) => Tree;
  const composer = compile("./components/ChatInputArea.tsx").ChatInputArea as (props: unknown) => Tree;
  const flush = () => { const pending = effects; effects = []; pending.forEach(fn => fn()); };
  type Result = { models: ReturnType<typeof model>[]; config: ReturnType<typeof config> | null; status: string; refresh: () => void; isCurrent: () => boolean; isReady: () => boolean };
  return { requests, modules, compile, react,
    render() { slots = hookSlots; index = 0; const value = hook() as Result; flush(); return value; },
    isolated(fn: () => unknown) { slots = []; index = 0; const value = fn(); effects = []; return value; },
    status, composer,
    auth(patch: Partial<typeof auth>) { const prior = auth; auth = { ...auth, ...patch }; [...subscribers].forEach(fn => fn(auth, prior)); },
    unmount() { hookSlots.forEach(slot => slot.cleanup?.()); },
    succeed(offset = 0, models = [model()], settings = config()) { assert.equal(requests[offset].path, "/api/v1/assistant/models"); assert.equal(requests[offset + 1].path, "/api/v1/assistant/config"); requests[offset].resolve({ models }); requests[offset + 1].resolve(settings); },
  };
}
test("empty and failed catalogs recover through explicit two-read refresh, without duplicate requests", async () => {
  const h = host(); assert.equal(h.render().status, "loading"); h.succeed(0, []); await settle(); const empty = h.render(); assert.equal(empty.status, "ready"); assert.equal(empty.models.length, 0);
  empty.refresh(); empty.refresh(); h.render(); assert.equal(h.requests.length, 4); h.requests[2].reject(new Error("offline")); h.requests[3].resolve(config()); await settle(); assert.equal(h.render().status, "error");
  h.render().refresh(); h.render(); h.succeed(4); await settle(); assert.equal(h.render().models[0].provider, "qwen"); assert.equal(h.requests.length, 6);
});
test("refresh retains observations only for the same authority and filters configured providers", async () => {
  const h = host(); h.render(); h.succeed(0, [model(), model("other")]); await settle(); const first = h.render(); assert.equal(first.models.length, 1); first.refresh(); assert.equal(h.render().status, "loading"); assert.equal(h.render().models[0].provider, "qwen"); h.succeed(2, [model("other")], config(["other"])); await settle(); assert.equal(h.render().models[0].provider, "other");
});
test("credential A-B-A rejects old config and stale refresh callbacks before rerender", async () => {
  const h = host(); const first = h.render(); h.auth({ token: "b" }); h.auth({ token: "a" }); assert.equal(first.isCurrent(), false); first.refresh(); assert.equal(h.requests.length, 2); h.succeed(); await settle(); assert.equal(h.render().status, "loading"); assert.equal(h.render().models.length, 0); assert.equal(h.requests.length, 4); assert.equal(h.requests[0].signal.aborted, true); h.succeed(2); await settle(); assert.equal(h.render().status, "ready");
});
test("unmount/signout suppress late reads, while same-user permission changes start a fresh catalog", async () => {
  const h = host(); h.render(); h.auth({ user: { user_id: "a", roles: ["member"], permissions: ["new"], tier: "normal" } }); h.render(); assert.equal(h.requests.length, 4); h.auth({ isAuthenticated: false }); h.render(); assert.equal(h.requests.length, 4); h.succeed(2); await settle(); assert.equal(h.render().models.length, 0); h.unmount();
  const second = host(); const old = second.render(); second.unmount(); second.succeed(); await settle(); old.refresh(); assert.equal(second.requests.length, 2); assert.equal(old.isCurrent(), false);
});
test("status offers existing setup route only to authorized viewers and distinguishes loading, empty and error", () => {
  const h = host(); const render = (state: string, canManageModels: boolean) => h.isolated(() => h.status({ state, canManageModels, onRefresh: noop }));
  assert.equal(nodes(render("loading", true)).filter(n => n.type === "Button").length, 0);
  assert.equal(nodes(render("empty", false)).filter(n => n.type === "Link").length, 0);
  assert.equal(nodes(render("empty", true)).find(n => n.type === "Link")?.props.to, "/services?tab=providers");
  assert.equal(nodes(render("error", true)).filter(n => n.type === "Button").length, 1); assert.equal(render("ready", true), null);
});
test("real composer preserves editable draft and attachments without a catalog, blocks Send and keeps active Stop", () => {
  const h = host(); let stops = 0; const file = { file: { name: "draft.txt", size: 1, type: "text/plain" }, status: "success", selected: true, response: { file_id: "f" } };
  const props = { input: "retained draft", setInput: noop, files: [file], isUploading: false, selectionNotice: "", isStreaming: false, isComposerBlocked: false, isGeneratingImage: false, isImageMode: false, hasAvailableModel: false, supportsVision: false, handleFileSelect: noop, removeFile: noop, retryFile: noop, toggleFileSelection: noop, onSend: noop, onStop: () => { stops++; }, onCancelImageMode: noop, handlePaste: noop, fileInputRef: { current: null }, config: null, datasets: [], selectedDatasets: [], webSearchEnabled: false, thinkingLevel: "auto", reasoningOptions: [], selectedStyle: "default" };
  const idle = nodes(h.isolated(() => h.composer(props))); assert.equal(idle.find(n => n.type === "Textarea")?.props.value, "retained draft"); assert.equal(idle.find(n => n.type === "Textarea")?.props.disabled, false); assert.equal(idle.find(n => n.props["aria-label"] === "Send")?.props.disabled, true);
  const active = nodes(h.isolated(() => h.composer({ ...props, isStreaming: true, isComposerBlocked: true }))); const stop = active.find(n => n.props["aria-label"] === "Stop generating"); assert.ok(stop); assert.notEqual(stop.props.disabled, true); (stop.props.onClick as (event: unknown) => void)({ detail: 1 }); assert.equal(stops, 1); assert.equal(props.files[0], file);
});

test("production page selection effect preserves explicit and newly restored session choices across late catalog reads", () => {
  const h = host(); const source = readFileSync(new URL("./index.tsx", import.meta.url), "utf8");
  const file = ts.createSourceFile("index.tsx", source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const page = file.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "AssistantPage") as ts.FunctionDeclaration;
  const statement = page.body!.statements.find(node => ts.isExpressionStatement(node) && node.getText(file).includes('modelCatalogStatus !== "ready"') && node.getText(file).includes("setSelectedModel(current =>")); assert.ok(statement);
  const production = `export function apply(context) { const { modelCatalogStatus, ownsModelCatalog, models, config, userId, setSelectedModel, readLastModelId } = context; const { preferredAssistantModelId } = require('../modelSelection'); const useEffect = fn => fn(); ${statement.getText(file)} }`;
  const apply = h.compile("", production).apply as (context: unknown) => void;
  let selected = '["other","same"]'; const context = { modelCatalogStatus: "ready", ownsModelCatalog: () => true, models: [model(), model("other")], config: config(["qwen", "other"]), userId: "a", readLastModelId: () => "", setSelectedModel: (fn: (value: string) => string) => { selected = fn(selected); } };
  apply(context); assert.equal(selected, '["other","same"]');
  selected = '["session-provider","saved"]'; apply(context); assert.equal(selected, '["session-provider","saved"]');
  selected = ""; apply({ ...context, ownsModelCatalog: () => false }); assert.equal(selected, ""); apply(context); assert.equal(selected, '["qwen","same"]');
});


test("retained readiness callback fails synchronously on refresh and cannot revive after a later successful read", async () => {
  const h = host(); h.render(); h.succeed(); await settle(); const first = h.render(); assert.equal(first.isReady(), true);
  first.refresh(); assert.equal(first.isReady(), false); h.render(); h.succeed(2); await settle(); assert.equal(h.render().isReady(), true); assert.equal(first.isReady(), false);
});

test("both actual page Send callbacks reject a stale catalog before any other side effect", () => {
  const h = host(); const source = readFileSync(new URL("./index.tsx", import.meta.url), "utf8");
  const file = ts.createSourceFile("index.tsx", source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const page = file.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "AssistantPage") as ts.FunctionDeclaration;
  for (const name of ["handleSend", "handleImageSend"]) {
    const declaration = page.body!.statements.filter(ts.isVariableStatement).flatMap(node => [...node.declarationList.declarations]).find(node => node.name.getText(file) === name); assert.ok(declaration);
    const callback = (declaration.initializer as ts.CallExpression).arguments[0];
    const compiled = h.compile("", `export function stale() { const canUseModelCatalog = () => false; return (${callback.getText(file)})(); }`);
    // Every remaining page dependency is intentionally absent: no dispatch, drawer,
    // draft, session, or image side effect may be reached by a retained callback.
    assert.doesNotThrow(() => compiled.stale());
  }
});
