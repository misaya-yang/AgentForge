import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

type Entry = { id: string; status: string; selected: boolean; file: File; progress?: number; response?: unknown };
type Result = { files: Entry[]; isUploading: boolean; handleFileSelect: (files: FileList) => void; removeFile: (index: number) => void; retryFile: (index: number) => void; clearFiles: () => void; consumeSelectedFiles: () => void; toggleFileSelection: (index: number) => void; rekeyFiles: (from: string, to: string) => void };
type Slot = { value: unknown; deps?: unknown[]; cleanup?: () => void };
const settle = () => new Promise(resolve => setImmediate(resolve));
const file = (name: string) => new File(["input"], name, { type: "text/plain" });
function compile(path: string, modules: Record<string, unknown>) {
  const exports: Record<string, unknown> = {};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8").replaceAll("import.meta.env.VITE_ASSISTANT_UI_V2", '"true"'), { compilerOptions: { jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, {
    exports, AbortController, File, FormData,
    require: (name: string) => { assert.ok(name in modules, name); return modules[name]; },
  });
  return exports;
}
function host() {
  const slots: Slot[] = []; let cursor = 0; let id = 0;
  let auth = { token: "a", isAuthenticated: true, user: { user_id: "a" } };
  const subscribers = new Set<(state: typeof auth, previous: typeof auth) => void>();
  const requests: Array<{ file: File; signal?: AbortSignal; progress: (event: { percent: number }) => void; resolve: (value: unknown) => void; reject: (value: unknown) => void }> = [];
  const memo = (value: unknown, deps: unknown[]) => { const i = cursor++; if (!slots[i] || deps.some((dep, j) => !Object.is(dep, slots[i].deps?.[j]))) slots[i] = { value, deps }; return slots[i].value; };
  const react = {
    useRef: (value: unknown) => memo({ current: value }, []), useCallback: memo,
    useState(initial: unknown) { const i = cursor++; slots[i] ??= { value: initial }; return [slots[i].value, (next: unknown) => { slots[i].value = typeof next === "function" ? next(slots[i].value) : next; }]; },
    useLayoutEffect(fn: () => (() => void) | undefined, deps: unknown[]) { const i = cursor++; if (!slots[i]) slots[i] = { value: null, deps, cleanup: fn() }; },
  };
  const modules = {
    "@/store/useAuthStore": { useAuthStore: { getState: () => auth, subscribe: (fn: (state: typeof auth, previous: typeof auth) => void) => { subscribers.add(fn); return () => subscribers.delete(fn); } } },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope: (user: unknown) => JSON.stringify(user) },
    react, "@/hooks/usePasteImage": { usePasteImage: (fn: unknown) => fn }, "@/lib/utils": { generateUUID: () => `file-${++id}` },
    "../fileSelection": compile("../fileSelection.ts", {}),
    "@/api/files": { isFileTypeSupported: () => true, uploadFileWithProgress: (input: File, progress: (event: { percent: number }) => void, _compress: boolean, signal?: AbortSignal) => new Promise((resolve, reject) => requests.push({ file: input, progress, signal, resolve, reject })) },
  };
  const hook = compile("./useFileHandler.ts", modules).useFileHandler as (key: string) => Result;
  return { requests, render(key = "a") { cursor = 0; return hook(key); },
    select(names: string[], key = "a") { this.render(key).handleFileSelect(names.map(file) as unknown as FileList); },
    succeed(index: number) { requests[index].resolve({ file_id: `remote-${index}`, file_path: `/remote/${index}` }); },
    auth(token: string, user = auth.user, isAuthenticated = true) { const previous = auth; auth = { token, user, isAuthenticated }; subscribers.forEach(fn => fn(auth, previous)); },
    unmount() { slots.forEach(slot => slot.cleanup?.()); },
  };
}

test("removing the only active upload aborts transport and immediately releases Send readiness", async () => {
  const h = host(); h.select(["a.txt"]); assert.equal(h.render().isUploading, true); h.render().removeFile(0);
  assert.equal(h.requests[0].signal?.aborted, true); assert.equal(h.render().isUploading, false);
  h.requests[0].progress({ percent: 80 }); h.succeed(0); await settle(); assert.equal(h.render().files.length, 0);
});
test("removed queued file is never uploaded and successful siblings remain selected", async () => {
  const h = host(); h.select(["a.txt", "b.txt", "c.txt"]); h.render().removeFile(1); h.succeed(0); await settle();
  assert.equal(h.requests.length, 2); assert.equal(h.requests[1].file.name, "c.txt"); h.succeed(1); await settle();
  assert.deepEqual(Array.from(h.render().files, entry => entry.file.name), ["a.txt", "c.txt"]); assert.ok(h.render().files.every(entry => entry.selected)); assert.equal(h.render().isUploading, false);
});
test("a canceled request's late rejection cannot reset newer upload readiness or restore a chip", async () => {
  const h = host(); h.select(["old.txt"]); h.render().removeFile(0); h.select(["new.txt"]);
  h.requests[0].reject(new Error("late canceled transport")); await settle(); assert.equal(h.render().isUploading, true); assert.equal(h.render().files[0].file.name, "new.txt");
  h.succeed(1); await settle(); assert.equal(h.render().isUploading, false); assert.equal(h.render().files[0].status, "success");
});
test("partial failure can retry once without losing siblings or duplicating attachment identity", async () => {
  const h = host(); h.select(["a.txt", "b.txt", "c.txt"]); h.succeed(0); await settle(); h.requests[1].reject(new Error("offline")); await settle(); h.succeed(2); await settle();
  const before = h.render(); assert.deepEqual(Array.from(before.files, entry => entry.status), ["success", "error", "success"]); const id = before.files[1].id;
  before.retryFile(1); before.retryFile(1); assert.equal(h.requests.length, 4); h.succeed(3); await settle(); assert.equal(h.render().files.length, 3); assert.equal(h.render().files[1].id, id);
  h.render().toggleFileSelection(1); h.render().consumeSelectedFiles(); assert.equal(h.render().files.length, 1); assert.equal(h.render().files[0].id, id);
});
test("clear cancels active and prevents queued dispatch, while a later selection remains usable", async () => {
  const h = host(); h.select(["a.txt", "b.txt"]); h.render().clearFiles(); assert.equal(h.requests[0].signal?.aborted, true); assert.equal(h.render().isUploading, false);
  h.select(["c.txt"]); h.succeed(0); await settle(); assert.equal(h.requests.length, 2); h.succeed(1); await settle(); assert.equal(h.render().files[0].file.name, "c.txt");
});
test("unmount aborts active uploads and suppresses pending dispatch even if transport resolves late", async () => {
  const h = host(); h.select(["a.txt", "b.txt"]); h.unmount(); assert.equal(h.requests[0].signal?.aborted, true); h.succeed(0); await settle(); assert.equal(h.requests.length, 1);
});
test("active and queued attachments follow draft rekey with independent source-session readiness", async () => {
  const h = host(); h.select(["a.txt", "b.txt"]); h.render().rekeyFiles("a", "b"); assert.equal(h.render("a").isUploading, false); assert.equal(h.render("b").isUploading, true);
  h.succeed(0); await settle(); assert.equal(h.requests.length, 2); h.succeed(1); await settle(); assert.ok(h.render("b").files.every(entry => entry.status === "success")); assert.equal(h.render("b").isUploading, false);
});
test("removing active upload after rekey aborts its original transport", () => {
  const h = host(); h.select(["a.txt"]); h.render().rekeyFiles("a", "b"); h.render("b").removeFile(0); assert.equal(h.requests[0].signal?.aborted, true); assert.equal(h.render("b").isUploading, false);
});
test("actual upload API passes cancellation to Axios and prevents dispatch canceled during preparation", async () => {
  const requests: Array<Record<string, unknown>> = [];
  const api = compile("../../../api/files.ts", { "@/lib/format": { formatFileSize: () => "1 KB" }, "@/lib/api": { api: { post: async (_path: string, _body: unknown, options: Record<string, unknown>) => { requests.push(options); return { data: { file_id: "id" } }; } } } }).uploadFileWithProgress as (input: File, progress?: unknown, compress?: boolean, signal?: AbortSignal) => Promise<unknown>;
  const first = new AbortController(); await api(file("a.txt"), undefined, false, first.signal); assert.equal(requests[0].signal, first.signal);
  const canceled = new AbortController(); canceled.abort(); await assert.rejects(api(file("b.txt"), undefined, true, canceled.signal)); assert.equal(requests.length, 1);
  const during = new AbortController(); const pending = api(file("c.txt"), undefined, true, during.signal); during.abort(); await assert.rejects(pending); assert.equal(requests.length, 1);
});


test("auth A-B-A cancels old transports, clears references and fences retained selection callbacks", async () => {
  const h = host(); h.select(["a.txt", "b.txt"]); const old = h.render(); h.auth("b"); h.auth("a");
  assert.equal(h.requests[0].signal?.aborted, true); old.handleFileSelect([file("stale.txt")] as unknown as FileList); assert.equal(h.requests.length, 1); assert.equal(h.render().files.length, 0);
  h.succeed(0); await settle(); assert.equal(h.requests.length, 1); h.select(["fresh.txt"]); h.succeed(1); await settle(); assert.equal(h.render().files[0].file.name, "fresh.txt");
});

test("actual composer exposes removal for uploading chips and invokes the selected index", () => {
  type Node = { type: unknown; props: Record<string, unknown> };
  const nodes = (value: unknown): Node[] => Array.isArray(value) ? value.flatMap(nodes) : value && typeof value === "object" ? [value as Node, ...nodes((value as Node).props?.children)] : [];
  const named = new Proxy({}, { get: (_, key) => String(key) }); const jsx = (type: unknown, props: Record<string, unknown>) => ({ type, props });
  const noop = () => {}; let removed = -1;
  const modules: Record<string, unknown> = {
    react: { useRef: () => ({ current: null }), useCallback: (fn: unknown) => fn, useEffect: noop }, "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-i18next": { useTranslation: () => ({ t: (key: string, fallback?: string) => fallback || key }) }, "framer-motion": { motion: named, AnimatePresence: "AnimatePresence", useReducedMotion: () => true }, "lucide-react": named,
    "@/lib/utils": { cn: () => "" }, "@/api/files": { isImageFile: () => false, formatFileSize: () => "1 KB" },
    "../components/QuickActionsMenu": { QuickActionsMenu: "QuickActionsMenu" }, "../components/StyleSelector": { StyleSelector: "StyleSelector" },
  };
  for (const name of ["button", "dropdown-menu", "textarea"]) modules[`@/components/ui/${name}`] = named;
  const composer = compile("../components/ChatInputArea.tsx", modules).ChatInputArea as (props: unknown) => unknown;
  const props = { hasAvailableModel: true, input: "draft", files: [{ id: "a", file: file("a.txt"), status: "uploading", selected: false }], isUploading: true, selectedDatasets: [], datasets: [], reasoningOptions: [], removeFile: (index: number) => { removed = index; } };
  const tree = composer(props);
  const remove = nodes(tree).find(node => node.props["aria-label"] === "common.delete: a.txt"); assert.ok(remove); assert.notEqual(remove.props.disabled, true); (remove.props.onClick as () => void)(); assert.equal(removed, 0);
  assert.equal(nodes(tree).find(node => node.props["aria-label"] === "Send")?.props.disabled, true);
  const after = composer({ ...props, files: [], isUploading: false });
  assert.equal(nodes(after).find(node => node.props["aria-label"] === "Send")?.props.disabled, false);
});


test("session B removal never cancels session A and pending session readiness stays isolated", async () => {
  const h = host(); h.select(["a.txt"], "a"); h.select(["b.txt"], "b"); h.render("b").removeFile(0);
  assert.equal(h.requests[0].signal?.aborted, false); assert.equal(h.requests[1].signal?.aborted, true); assert.equal(h.render("a").isUploading, true); assert.equal(h.render("b").isUploading, false);
  h.succeed(0); h.succeed(1); await settle(); assert.equal(h.render("a").files[0].status, "success"); assert.equal(h.render("b").files.length, 0);
});
test("logout and same-token user authority changes retire uploads before rerender", async () => {
  for (const logout of [false, true]) {
    const h = host(); h.select(["a.txt", "b.txt"]); const old = h.render();
    h.auth("a", { user_id: logout ? "a" : "other" }, !logout);
    assert.equal(h.requests[0].signal?.aborted, true); old.handleFileSelect([file("stale.txt")] as unknown as FileList); h.succeed(0); await settle();
    assert.equal(h.requests.length, 1); assert.equal(h.render().files.length, 0);
  }
});
test("actual expired-reference classifier retains explicit manual reupload instruction", () => {
  const source = readFileSync(new URL("./useChatSession.ts", import.meta.url), "utf8");
  const parsed = ts.createSourceFile("useChatSession.ts", source, ts.ScriptTarget.Latest, true);
  const names = new Set(["sourceRevoked", "needsNewThread", "attachmentUnavailable", "startFailureMessage"]);
  const statements: string[] = [];
  function visit(node: ts.Node) { if (ts.isVariableStatement(node) && node.declarationList.declarations.some(declaration => names.has(declaration.name.getText(parsed)))) statements.push(node.getText(parsed)); ts.forEachChild(node, visit); }
  visit(parsed); assert.equal(statements.length, 4);
  const message = vm.runInNewContext(ts.transpileModule(statements.join("\n") + "\nstartFailureMessage;", { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText, { error: { response: { data: { detail: { code: "ATTACHMENT_UNAVAILABLE" } } } }, t: (_key: string, fallback: string) => fallback });
  assert.equal(message, "A selected attachment is unavailable. Remove it and upload the file again, then send manually. No run was started.");
});
