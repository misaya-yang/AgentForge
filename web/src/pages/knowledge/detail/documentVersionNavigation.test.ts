import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

type Tree = { type: unknown; props: Record<string, unknown> };
type Slot = { value?: unknown; deps?: unknown[]; cleanup?: () => void };
type Request = { kind: string; args: unknown[]; resolve: (value: unknown) => void; reject: (value: unknown) => void };
type Result = { versions: Array<{ version_number: number }>; offset: number; total: number; loadError: boolean; loading: boolean; loadPage: (offset: number) => Promise<void> | undefined; setCompareMode: (value: boolean) => void; handleSelectForCompare: (version: number) => void; selectedForCompare: number[]; handleCompare: () => Promise<void>; compareResult: unknown; dismissComparison: () => void; restoreVersion: number | null; setRestoreVersion: (version: number | null) => void; handleRestore: () => Promise<void>; onDialogOpenChange: (open: boolean) => void };
function nodes(value: unknown): Tree[] { if (Array.isArray(value)) return value.flatMap(nodes); if (!value || typeof value !== "object") return []; const node = value as Tree; return [node, ...nodes(node.props?.children)]; }
function page(total: number, offset = 0, documentId = "doc-a") { return { total, current_version: total, versions: Array.from({ length: Math.min(20, Math.max(0, total - offset)) }, (_, i) => ({ version_id: `id-${total-offset-i}`, document_id: documentId, version_number: total-offset-i, created_at: "2026-10-14", word_count: 2, change_type: "updated", content_hash: `hash-${total-offset-i}` })) }; }
function host() {
  let auth = { token: "a", isAuthenticated: true, user: { user_id: "a", roles: ["member"] } }; const listeners = new Set<(state: typeof auth, prior: typeof auth) => void>();
  const slots: Slot[] = []; let cursor = 0; let effects: Array<() => void> = []; const requests: Request[] = []; const notices: unknown[] = []; let restored = 0;
  let props = { datasetId: "dataset-a", documentId: "doc-a", documentTitle: "Document A", open: true, onOpenChange: (value: boolean) => { props.open = value; }, onRestored: () => { restored++; } };
  const memo = (fn: () => unknown, deps: unknown[]) => { const i = cursor++; const prior = slots[i]; if (!prior || deps.some((d, j) => !Object.is(d, prior.deps?.[j]))) slots[i] = { value: fn(), deps }; return slots[i].value; };
  const effect = (fn: () => (() => void) | undefined, deps: unknown[]) => { const i = cursor++; const prior = slots[i]; if (!prior || deps.some((d, j) => !Object.is(d, prior.deps?.[j]))) { const next: Slot = { deps }; slots[i] = next; effects.push(() => { prior?.cleanup?.(); next.cleanup = fn(); }); } };
  const react = { useState(initial: unknown) { const i = cursor++; slots[i] ??= { value: typeof initial === "function" ? initial() : initial }; return [slots[i].value, (next: unknown) => { slots[i].value = typeof next === "function" ? next(slots[i].value) : next; }]; }, useMemo: memo, useCallback: (fn: unknown, deps: unknown[]) => memo(() => fn, deps), useRef: (value: unknown) => memo(() => ({ current: value }), []), useLayoutEffect: effect };
  const jsx = (type: unknown, props: Record<string, unknown>) => ({ type, props }); const named = new Proxy({}, { get: (_, name) => String(name) }); const t = (key: string) => key;
  const request = (kind: string) => (...args: unknown[]) => new Promise((resolve, reject) => requests.push({ kind, args, resolve, reject }));
  const modules: Record<string, unknown> = { react, "react/jsx-runtime": { jsx, jsxs: jsx }, "react-i18next": { useTranslation: () => ({ t, i18n: { language: "en-US" } }) }, "lucide-react": named,
    "@/store/useAuthStore": { useAuthStore: Object.assign((selector: (state: typeof auth) => unknown) => selector(auth), { getState: () => auth, subscribe: (fn: (state: typeof auth, prior: typeof auth) => void) => { listeners.add(fn); return () => listeners.delete(fn); } }) },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope: (user: unknown) => JSON.stringify(user) }, "@/hooks/use-toast": { toast: (value: unknown) => notices.push(value) },
    "@/pages/assistant/components/SourceDocumentDialog": { SourceDocumentDialog: "SourceDocumentDialog" },
    "@/api/knowledge": { listDocumentVersions: request("list"), compareDocumentVersions: request("compare"), restoreDocumentVersion: request("restore") },
    "@/components/ui/button": named, "@/components/ui/badge": named, "@/components/ui/dialog": named, "@/components/ui/alert-dialog": named,
  };
  function compile(path: string) { const exports: Record<string, (props: unknown) => unknown> = {}; vm.runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 } }).outputText, { exports, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; } }); return exports; }
  const actual = compile("./useDocumentVersionNavigation.ts"); let result: Result;
  modules["./useDocumentVersionNavigation"] = { ...actual, useDocumentVersionNavigation: (props: unknown) => { result = actual.useDocumentVersionNavigation(props) as Result; return result; } };
  const component = compile("./DocumentVersionHistory.tsx").DocumentVersionHistory;
  return { requests, notices, restored: () => restored,
    render() { cursor = 0; const tree = component(props); const pending = effects; effects = []; pending.forEach(fn => fn()); return { ...result, tree }; },
    navigate(patch: Partial<typeof props>) { props = { ...props, ...patch }; },
    auth(patch: Partial<typeof auth>) { const prior = auth; auth = { ...auth, ...patch }; [...listeners].forEach(fn => fn(auth, prior)); },
    unmount() { slots.forEach(slot => slot.cleanup?.()); },
  };
}
const settle = async () => { await Promise.resolve(); await Promise.resolve(); };
async function ready(total = 45) { const h = host(); h.render(); h.requests[0].resolve(page(total)); await settle(); h.render(); return h; }
test("actual version dialog reaches all 45 versions through bounded metadata pages", async () => {
  const h = await ready(); let state = h.render(); assert.equal(state.versions.length, 20); assert.deepEqual(h.requests[0].args, ["dataset-a", "doc-a", 20, 0]);
  const next = nodes(state.tree).find(node => node.type === "Button" && node.props.children === "knowledge.versionHistory.nextPage")!; assert.ok(next); (next.props.onClick as () => void)(); assert.equal(h.requests[1].args[3], 20); h.requests[1].resolve(page(45, 20)); await settle(); state = h.render(); assert.equal(state.versions[0].version_number, 25);
  const pending = state.loadPage(40); h.requests[2].resolve(page(45, 40)); await pending; state = h.render(); assert.equal(state.versions.length, 5); assert.equal(state.versions[4].version_number, 1); assert.equal(nodes(state.tree).find(node => node.type === "Button" && node.props.children === "knowledge.versionHistory.nextPage")?.props.disabled, true);
});
test("comparison retains immutable selected revisions across pages and returns to the same page", async () => {
  const h = await ready(); h.render().setCompareMode(true); h.render().handleSelectForCompare(45); let state = h.render(); const next = state.loadPage(40); h.requests[1].resolve(page(45, 40)); await next; h.render().handleSelectForCompare(1); state = h.render(); assert.deepEqual(Array.from(state.selectedForCompare), [45, 1]);
  const compare = state.handleCompare(); assert.deepEqual(h.requests[2].args, ["dataset-a", "doc-a", 1, 45]); h.requests[2].resolve({ from_version: 1, to_version: 45, diff: [], stats: { additions: 0, deletions: 0, changes: 0 } }); await compare; assert.ok(h.render().compareResult); h.render().dismissComparison(); assert.equal(h.render().offset, 40); assert.equal(h.requests.filter(r => r.kind === "restore").length, 0);
});
test("page shrink corrects a deleted final page once and preserves truthful server total", async () => {
  const h = await ready(); const pending = h.render().loadPage(40); h.requests[1].resolve(page(15, 40)); await settle(); assert.equal(h.requests[2].args[3], 0); h.requests[2].resolve(page(15)); await pending; assert.equal(h.render().offset, 0); assert.equal(h.render().total, 15);
});
test("page error hides old rows, offers explicit retry, and does not invent zero history", async () => {
  const h = await ready(); const pending = h.render().loadPage(20); h.requests[1].reject({ response: { status: 403, data: "secret" } }); await pending; const state = h.render(); assert.equal(state.loadError, true); assert.equal(state.versions.length, 0); assert.ok(nodes(state.tree).find(node => node.props.role === "alert")); const retry = state.loadPage(20); h.requests[2].resolve(page(45, 20)); await retry; assert.equal(h.render().offset, 20); assert.equal(h.render().loadError, false); assert.equal(h.notices.length, 0);
});
test("out-of-order pages and retired page actions cannot overwrite a newer request", async () => {
  const h = await ready(); const old = h.render(); const first = old.loadPage(20); const second = h.render().loadPage(40); h.requests[2].resolve(page(45, 40)); await second; h.requests[1].resolve(page(45, 20)); await first; assert.equal(h.render().offset, 40); old.setRestoreVersion(44); await old.handleCompare(); assert.equal(h.render().compareResult, null); assert.equal(h.requests.length, 3);
});
test("close/reopen, document change, auth A-B-A and unmount retire old reads and actions", async () => {
  const h = await ready(); const old = h.render(); const pending = old.loadPage(20); h.render().onDialogOpenChange(false); await old.handleRestore(); h.render(); h.navigate({ open: true, documentId: "doc-b" }); h.render(); h.requests[1].resolve(page(45, 20)); await pending; assert.equal(h.render().versions.length, 0);
  const retained = h.render(); h.auth({ token: "b" }); h.auth({ token: "a" }); await retained.loadPage(0); assert.equal(h.requests.length, 3); h.render(); h.unmount(); h.requests[3].resolve(page(45, 0, "doc-b")); await settle(); assert.equal(h.restored(), 0);
});
test("page navigation invalidates a pending comparison and stale restore confirmation", async () => {
  const h = await ready(); h.render().setRestoreVersion(44); const confirmed = h.render(); const next = confirmed.loadPage(20); await confirmed.handleRestore(); assert.equal(h.requests.filter(r => r.kind === "restore").length, 0); h.requests[1].resolve(page(45, 20)); await next;
  h.render().setCompareMode(true); h.render().handleSelectForCompare(25); h.render().handleSelectForCompare(24); const compare = h.render().handleCompare(); const newer = h.render().loadPage(40); h.requests[2].resolve({ from_version: 24, to_version: 25, diff: [], stats: {} }); await compare; assert.equal(h.render().compareResult, null); h.requests[3].resolve(page(45, 40)); await newer;
});
test("restore is explicit, duplicate-safe and cannot close a different document after completion", async () => {
  const h = await ready(); h.render().setRestoreVersion(44); const confirmed = h.render(); const pending = confirmed.handleRestore(); await confirmed.handleRestore(); assert.equal(h.requests.filter(r => r.kind === "restore").length, 1); h.navigate({ documentId: "doc-b" }); h.render(); h.requests[1].resolve({ status: "queued" }); await pending; assert.equal(h.restored(), 0); assert.equal(h.notices.length, 0);
});

test("retained page-A restore callback cannot re-arm confirmation during or after page B", async () => {
  const h = await ready(); const pageA = h.render(); const pending = pageA.loadPage(20);
  pageA.setRestoreVersion(44); assert.equal(h.render().restoreVersion, null);
  h.requests[1].resolve(page(45, 20)); await pending;
  pageA.setRestoreVersion(44); assert.equal(h.render().restoreVersion, null);
  await h.render().handleRestore(); assert.equal(h.requests.filter(r => r.kind === "restore").length, 0);
});

test("a second shrink or inconsistent count stops after one correction and offers retry", async () => {
  const h = await ready(); const pending = h.render().loadPage(40); h.requests[1].resolve(page(25, 40)); await settle(); assert.equal(h.requests[2].args[3], 20);
  h.requests[2].resolve({ ...page(0, 20), current_version: 1 }); await pending; assert.equal(h.render().loadError, true); assert.equal(h.requests.length, 3);
  const retry = h.render().loadPage(0); h.requests[3].resolve({ ...page(45), total: 2 }); await retry; assert.equal(h.render().loadError, true);
});
