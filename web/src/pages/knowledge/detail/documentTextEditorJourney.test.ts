import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

type Node = { type: unknown; props: Record<string, unknown> };
type Request = { kind: string; args: unknown[]; resolve: (value: unknown) => void; reject: (error: unknown) => void };

function nodes(value: unknown, type?: string): Node[] {
  if (Array.isArray(value)) return value.flatMap(item => nodes(item, type));
  if (!value || typeof value !== "object") return [];
  const node = value as Node;
  return [...(!type || node.type === type ? [node] : []), ...nodes(node.props?.children, type)];
}

function text(value: unknown): string {
  if (typeof value === "string" || typeof value === "number") return String(value);
  if (Array.isArray(value)) return value.map(text).join(" ");
  if (!value || typeof value !== "object") return "";
  return text((value as Node).props?.children);
}

function source(version: number, content: string, changes: Record<string, unknown> = {}) {
  return {
    dataset_id: "dataset-a", document_id: "document-a", source_type: "text",
    status: "completed", current_version: version, enabled: true, archived: false, content,
    ...changes,
  };
}

function host() {
  const slots: unknown[] = [];
  const cleanups = new Map<number, () => void>();
  let cursor = 0, dirty = true, effects: Array<() => void> = [], tree: unknown;
  let props = { datasetId: "dataset-a", documentId: "document-a", documentTitle: "Notes.txt", open: true,
    onOpenChange(open: boolean) { props.open = open; }, onPublished() { published += 1; } };
  let published = 0;
  const requests: Request[] = [];
  const auth = { token: "session-a", user: { user_id: "owner-a", tenant_id: "tenant-a", roles: ["owner"] } };
  const same = (left: unknown[] | undefined, right: unknown[]) => left?.length === right.length && left.every((v, i) => Object.is(v, right[i]));
  const react = {
    useState(initial: unknown) {
      const index = cursor++;
      if (!(index in slots)) slots[index] = typeof initial === "function" ? (initial as () => unknown)() : initial;
      return [slots[index], (next: unknown) => {
        const value = typeof next === "function" ? (next as (value: unknown) => unknown)(slots[index]) : next;
        if (!Object.is(value, slots[index])) { slots[index] = value; dirty = true; }
      }];
    },
    useRef(initial: unknown) { const index = cursor++; return slots[index] ?? (slots[index] = { current: initial }); },
    useMemo(make: () => unknown, deps: unknown[]) {
      const index = cursor++;
      const previous = slots[index] as { deps: unknown[]; value: unknown } | undefined;
      if (!previous || !same(previous.deps, deps)) slots[index] = { deps, value: make() };
      return (slots[index] as { value: unknown }).value;
    },
    useEffect(run: () => void | (() => void), deps: unknown[]) {
      const index = cursor++;
      if (!same(slots[index] as unknown[] | undefined, deps)) {
        slots[index] = deps;
        effects.push(() => { cleanups.get(index)?.(); const cleanup = run(); if (cleanup) cleanups.set(index, cleanup); else cleanups.delete(index); });
      }
    },
  };
  const jsx = (type: unknown, elementProps: Record<string, unknown>) => ({ type, props: elementProps });
  const named = new Proxy({}, { get: (_target, name) => String(name) });
  const request = (kind: string) => (...args: unknown[]) => new Promise((resolve, reject) => requests.push({ kind, args, resolve, reject }));
  const modules: Record<string, unknown> = {
    react: { ...react, useCallback: (fn: unknown, deps: unknown[]) => react.useMemo(() => fn, deps) },
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "lucide-react": named,
    "@/api/knowledge": {
      getActiveDocumentSource: request("source"),
      getDocument: request("document"),
      replaceDocumentContent: request("replace"),
    },
    "@/store/useAuthStore": {
      useAuthStore: (select: (state: typeof auth) => unknown) => select(auth),
    },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope: (user: typeof auth.user) => user.user_id },
    "@/components/ui/button": named,
    "@/components/ui/label": named,
    "@/components/ui/textarea": named,
    "@/components/ui/dialog": named,
  };
  function load(path: string) {
    const exports: Record<string, unknown> = {};
    const output = ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
    }).outputText;
    vm.runInNewContext(output, {
      exports,
      require: (name: string) => modules[name] ?? named,
      AbortController,
      setTimeout,
      clearTimeout,
    });
    return exports;
  }
  modules["./documentTextEditing"] = load("./documentTextEditing.ts");
  const { DocumentTextEditorDialog } = load("./DocumentTextEditorDialog.tsx") as {
    DocumentTextEditorDialog: (props: Record<string, unknown>) => unknown;
  };
  function render(next = props) {
    props = next; cursor = 0; dirty = false;
    tree = DocumentTextEditorDialog(props);
    const queued = effects; effects = []; queued.forEach(run => run());
  }
  async function flush() {
    for (let i = 0; i < 20; i++) {
      await new Promise<void>(resolve => setImmediate(resolve));
      if (dirty) render();
      if (effects.length) { const queued = effects; effects = []; queued.forEach(run => run()); }
    }
  }
  function button(testId: string) {
    const found = nodes(tree, "Button").find(node => node.props["data-testid"] === testId
      || text(node).includes(testId));
    assert.ok(found, testId);
    return found;
  }
  return {
    requests, render, flush, button, tree: () => tree, text: () => text(tree), props: () => props,
    request(kind: string, index = 0) { const found = requests.filter(item => item.kind === kind)[index]; assert.ok(found, kind); return found; },
    textarea() { const found = nodes(tree, "Textarea")[0]; assert.ok(found); return found; },
    published: () => published,
    unmount() { cleanups.forEach(cleanup => cleanup()); },
  };
}

async function loadedHost(initial = source(1, "v1 source")) {
  const h = host(); h.render(); await h.flush();
  h.request("source").resolve(initial); await h.flush();
  return h;
}

test("text edit submits the expected version and waits for publication before refreshing the source", async () => {
  const h = await loadedHost();
  (h.textarea().props.onChange as (event: unknown) => void)({ target: { value: "v2 source" } });
  await h.flush();
  (h.button("document-text-source-save").props.onClick as () => void)();
  await h.flush();
  const write = h.request("replace");
  assert.equal(write.args[0], "dataset-a");
  assert.equal(write.args[1], "document-a");
  assert.equal((write.args[2] as Record<string, unknown>).content, "v2 source");
  assert.equal((write.args[2] as Record<string, unknown>).expected_current_version, 1);
  write.resolve({ status: "pending", document_id: "document-a", previous_version: 1, candidate_version: 2, execution_id: "hidden-execution-id" });
  await h.flush();
  assert.ok(h.text().includes("knowledge.documentTextEditor.processing"));
  assert.ok(!h.text().includes("hidden-execution-id"));
  h.request("document").resolve({ dataset_id: "dataset-a", document_id: "document-a", status: "completed", current_version: 2, progress: 100 });
  await h.flush();
  h.request("source", 1).resolve(source(2, "v2 source"));
  await h.flush();
  assert.ok(h.text().includes("knowledge.documentTextEditor.success"));
  assert.equal(h.textarea().props.value, "v2 source");
  assert.equal(h.published(), 2);
  h.unmount();
});

test("409 reloads the current version without resubmitting the draft", async () => {
  const h = await loadedHost();
  (h.textarea().props.onChange as (event: unknown) => void)({ target: { value: "stale v2" } });
  await h.flush();
  (h.button("document-text-source-save").props.onClick as () => void)(); await h.flush();
  h.request("replace").reject({ response: { status: 409 } }); await h.flush();
  assert.ok(h.text().includes("knowledge.documentTextEditor.conflict"));
  (h.button("knowledge.documentTextEditor.reloadSource").props.onClick as () => void)(); await h.flush();
  h.request("source", 1).resolve(source(2, "current v2")); await h.flush();
  assert.equal(h.textarea().props.value, "current v2");
  assert.equal(h.requests.filter(item => item.kind === "replace").length, 1);
  h.unmount();
});

test("a processing failure keeps the previous published source and reports no raw error", async () => {
  const h = await loadedHost();
  (h.textarea().props.onChange as (event: unknown) => void)({ target: { value: "candidate v2" } });
  await h.flush();
  (h.button("document-text-source-save").props.onClick as () => void)(); await h.flush();
  h.request("replace").resolve({ status: "pending", document_id: "document-a", previous_version: 1, candidate_version: 2 });
  await h.flush();
  h.request("document").resolve({ dataset_id: "dataset-a", document_id: "document-a", status: "failed", current_version: 1, error: "private worker detail" });
  await h.flush();
  assert.ok(h.text().includes("knowledge.documentTextEditor.failed"));
  assert.ok(!h.text().includes("private worker detail"));
  assert.equal(h.textarea().props.value, "candidate v2");
  h.unmount();
});

test("non-text source responses never populate the editor", async () => {
  const h = host(); h.render(); await h.flush();
  h.request("source").resolve(source(1, "binary content", { source_type: "upload" }));
  await h.flush();
  assert.ok(h.text().includes("knowledge.documentTextEditor.notEditable"));
  assert.equal(nodes(h.tree(), "Textarea").length, 0);
  h.unmount();
});
