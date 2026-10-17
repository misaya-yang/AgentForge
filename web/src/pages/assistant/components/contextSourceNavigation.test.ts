import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { mergeKnowledgeContexts } from "../../../features/chat/knowledgeContexts.ts";
import { citationSource } from "./citationSource.ts";
import type { KnowledgeContextChunk } from "../../../features/chat/knowledgeContexts.ts";

type Tree = { type: unknown; props: Record<string, unknown> };
function all(tree: unknown, type: string): Tree[] {
  if (!tree || typeof tree !== "object") return [];
  const node = tree as Tree;
  return [...(node.type === type && node.props ? [node] : []), ...Object.values(tree).flatMap((value) => all(value, type))];
}
const hash = "a".repeat(64);
const chunk = (extra: Partial<KnowledgeContextChunk> = {}): KnowledgeContextChunk => ({
  dataset_id: "dataset-A", document_id: "document-A", segment_id: "segment-A",
  content: "共同段落 数量 42", score: 0.8, source_version: 1, source_hash: hash, ...extra,
});
function merged(...chunks: KnowledgeContextChunk[]) {
  return chunks.reduce((current, hit) => mergeKnowledgeContexts(current, {
    dataset_id: "dataset-A", chunks: [hit],
  }, ["dataset-A"]), [] as ReturnType<typeof mergeKnowledgeContexts>);
}
function panel(contexts: ReturnType<typeof merged>) {
  const slots: unknown[] = [];
  let cursor = 0;
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const jsx = (type: unknown, props: Record<string, unknown>) => ({ type, props });
  const modules: Record<string, unknown> = {
    react: { useState: (initial: unknown) => {
      const index = cursor++;
      if (!(index in slots)) slots[index] = initial;
      return [slots[index], (value: unknown) => { slots[index] = typeof value === "function" ? value(slots[index]) : value; }];
    } },
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-i18next": { useTranslation: () => ({ t: (_key: string, fallback: string) => fallback }) },
    "motion/react": { motion: named, AnimatePresence: "AnimatePresence" },
    "@/lib/utils": { cn: (...values: unknown[]) => values.filter(Boolean).join(" ") },
    "./citationSource": { citationSource },
  };
  const exports: { ContextDisplay?: (props: unknown) => unknown } = {};
  const code = ts.transpileModule(readFileSync(new URL("./ContextDisplay.tsx", import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText;
  vm.runInNewContext(code, { exports, require: (id: string) => modules[id] ?? named });
  const render = (next = contexts) => { contexts = next; cursor = 0; return exports.ContextDisplay!({ contexts }); };
  (all(render(), "button")[0].props.onClick as () => void)();
  return { render };
}
function targets(contexts: ReturnType<typeof merged>) {
  return contexts[0].chunks.map((hit) => citationSource(contexts[0].dataset_id, hit));
}

test("repeated searches retain both historical versions of an unchanged excerpt", () => {
  const contexts = merged(chunk(), chunk({ source_version: 2, source_hash: "b".repeat(64) }));
  assert.equal(contexts[0].chunks.length, 2);
  assert.deepEqual(targets(contexts).map((source) => source.sourceVersion), [1, 2]);
});

test("same excerpt and version with distinct hashes cannot erase a source target", () => {
  assert.equal(merged(chunk(), chunk({ source_hash: "b".repeat(64) }))[0].chunks.length, 2);
});

test("metadata-only historical document targets survive aggregation", () => {
  const base = { document_id: undefined, segment_id: undefined, source_version: undefined, source_hash: undefined };
  const contexts = merged(
    chunk({ ...base, metadata: { document_id: "document-A", source_version: 1, source_hash: hash } }),
    chunk({ ...base, metadata: { document_id: "document-B", source_version: 2, source_hash: hash } }),
  );
  assert.deepEqual(targets(contexts).map((source) => source.documentId), ["document-A", "document-B"]);
  assert.deepEqual(targets(contexts).map((source) => source.sourceVersion), [1, 2]);
});

test("missing identity fields and embedded delimiters cannot collide", () => {
  assert.equal(merged(chunk({ document_id: "same", segment_id: undefined }), chunk({ document_id: undefined, segment_id: "same" }))[0].chunks.length, 2);
  assert.equal(merged(chunk({ document_id: "a\u0000b", segment_id: "c" }), chunk({ document_id: "a", segment_id: "b\u0000c" }))[0].chunks.length, 2);
});

test("exact repeated targets still collapse and conflicting provenance is not replaced", () => {
  assert.equal(merged(chunk(), chunk({ score: 0.9 }))[0].chunks.length, 1);
  const contexts = merged(chunk(), chunk({ metadata: { document_id: "other" } }));
  assert.deepEqual(targets(contexts).map((source) => source.provenance), ["versioned", "invalid"]);
});

test("every source, including fourth and later saved-history hits, opens its exact target", () => {
  // JSON round-trip matches saved context data, without claiming history API acceptance.
  const contexts = JSON.parse(JSON.stringify(merged(...Array.from({ length: 6 }, (_, index) => chunk({
    document_id: `document-${index}`, segment_id: `segment-${index}`, source_version: index + 1,
  })))));
  const app = panel(contexts);
  const more = all(app.render(), "button").find((button) => button.props["data-testid"] === "assistant-source-show-more");
  assert.ok(more, "the remainder must have a working reveal action");
  (more.props.onClick as () => void)();
  const buttons = all(app.render(), "button").filter((button) => button.props["aria-expanded"] === undefined);
  assert.equal(buttons.length, 6, "the count must not advertise sources with no opening action");
  buttons.forEach((button, index) => {
    (button.props.onClick as () => void)();
    const selected = all(app.render(), "SourceDocumentDialog")[0].props.source;
    assert.deepEqual(selected, citationSource("dataset-A", contexts[0].chunks[index]));
  });
  (all(app.render(), "SourceDocumentDialog")[0].props.onClose as () => void)();
  assert.equal(all(app.render(), "SourceDocumentDialog")[0].props.source, null);
});


test("explicit metadata dataset conflicts survive beside valid hits in either order", () => {
  const valid = chunk();
  const conflicting = chunk({ metadata: { dataset_id: "dataset-B" } });
  assert.deepEqual(targets(merged(valid, conflicting)).map((source) => source.provenance), ["versioned", "invalid"]);
  assert.deepEqual(targets(merged(conflicting, valid)).map((source) => source.provenance), ["invalid", "versioned"]);
  assert.equal(merged(valid, chunk({ metadata: { dataset_id: "dataset-A" } }))[0].chunks.length, 1);
  assert.equal(merged(valid, chunk({ metadata: { dataset_id: null } }))[0].chunks.length, 1);
  assert.deepEqual(targets(merged(valid, chunk({ metadata: { dataset_id: "" } }))).map((source) => source.provenance), ["versioned", "invalid"]);
});


test("1,000 accumulated hits mount progressively and every target remains reachable without growing animation delay", () => {
  const contexts = merged(...Array.from({ length: 1000 }, (_, index) => chunk({ document_id: `document-${index}` })));
  const app = panel(contexts);
  const opened = new Set<string>();
  let expectedCount = 3;
  while (true) {
    const tree = app.render();
    const cards = all(tree, "div").filter((node) => {
      const transition = node.props.transition as { delay?: number } | undefined;
      return typeof transition?.delay === "number";
    });
    assert.equal(cards.length, Math.min(expectedCount, 1000));
    assert.ok(cards.every((node) => (node.props.transition as { delay: number }).delay <= 0.1));
    const buttons = all(tree, "button").filter((button) => button.props["aria-expanded"] === undefined);
    const more = buttons.find((button) => button.props["data-testid"] === "assistant-source-show-more");
    const sourceButtons = buttons.filter((button) => button !== more);
    for (let index = opened.size; index < sourceButtons.length; index++) {
      (sourceButtons[index].props.onClick as () => void)();
      const selected = all(app.render(), "SourceDocumentDialog")[0].props.source as { documentId: string };
      assert.equal(selected.documentId, `document-${index}`);
      opened.add(selected.documentId);
    }
    if (!more) break;
    assert.equal(more.props.type, "button");
    const before = all(app.render(), "SourceDocumentDialog")[0].props.source;
    (more.props.onClick as () => void)();
    assert.equal(all(app.render(), "SourceDocumentDialog")[0].props.source, before);
    expectedCount += 20;
  }
  assert.equal(opened.size, 1000);
});


test("reveal counts stay dataset-scoped across reordered and appended contexts", () => {
  const contexts = merged(...Array.from({ length: 30 }, (_, index) => chunk({ document_id: `document-${index}` })));
  const other = { ...contexts[0], dataset_id: "dataset-B", dataset_name: "B",
    chunks: contexts[0].chunks.map((hit) => ({ ...hit, dataset_id: "dataset-B" })) };
  const app = panel(contexts);
  const firstMore = all(app.render(), "button").find((button) => button.props["data-testid"] === "assistant-source-show-more")!;
  (firstMore.props.onClick as () => void)();
  assert.equal(all(app.render(), "SourceDocumentDialog")[0].props.source, null);
  const next = [other, { ...contexts[0], chunks: [...contexts[0].chunks, chunk({ document_id: "new-hit" })] }];
  const tree = app.render(next);
  const sourceButtons = all(tree, "button").filter((button) => button.props["aria-expanded"] === undefined && button.props["data-testid"] !== "assistant-source-show-more");
  assert.equal(sourceButtons.length, 26, "new B retains 3; expanded A retains 23");
  (sourceButtons[2].props.onClick as () => void)();
  assert.equal((all(app.render(), "SourceDocumentDialog")[0].props.source as { datasetId: string }).datasetId, "dataset-B");
  (sourceButtons[25].props.onClick as () => void)();
  assert.equal((all(app.render(), "SourceDocumentDialog")[0].props.source as { datasetId: string }).datasetId, "dataset-A");
  const replacement = [{ ...contexts[0], chunks: contexts[0].chunks.map((hit) => ({ ...hit, content: "replacement excerpt" })) }];
  const sourceCount = (view: unknown) => all(view, "button").filter((button) => button.props["aria-expanded"] === undefined && button.props["data-testid"] !== "assistant-source-show-more").length;
  assert.equal(sourceCount(app.render(replacement)), 23, "same mounted dataset retains its revealed count");
  assert.equal(sourceCount(panel(replacement).render()), 3, "fresh component begins with three");
});
