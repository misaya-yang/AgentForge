import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

// eslint-disable-next-line @typescript-eslint/no-explicit-any -- JSX primitive boundary in the controlled renderer.
type Tree = { type: string | ((props: Record<string, unknown>) => Tree); props: Record<string, any> };
const artifactId = "art_history_fixture";
const href = `/api/v1/assistant/artifacts/${artifactId}/download`;
const artifact = { id: artifactId, url: href, filename: "report.pdf", title: "Report" };

// Complete production modules with controlled React, Markdown parser, HTTP and
// browser primitives. These are component contract tests, not browser acceptance.
function host(language = "en-US") {
  const requests: unknown[][] = [];
  const opened: unknown[][] = [];
  const anchors: Array<{ href?: string; download?: string }> = [];
  const notifications: string[][] = [];
  const payload = new Blob(["%PDF fixture"], { type: "application/pdf" });
  const blobs: Blob[] = [];
  const timers: Array<() => void> = [];
  const revoked: string[] = [];
  let response: (...args: unknown[]) => Promise<{ data: unknown }> = async () => ({ data: payload });
  const api = { get: async (...args: unknown[]) => { requests.push(args); return response(...args); } };
  const jsx = (type: Tree["type"], props: Tree["props"]) => ({ type, props });
  const refs: Array<{ current: unknown }> = [];
  let refCursor = 0;
  let memoCursor = 0;
  const memos: Array<{ dependencies: unknown[]; value: unknown }> = [];
  const react = {
    memo: (fn: unknown) => fn,
    lazy: () => "lazy",
    useMemo: (fn: () => unknown, dependencies: unknown[]) => {
      const index = memoCursor++;
      const previous = memos[index];
      if (!previous || dependencies.length !== previous.dependencies.length || dependencies.some((value, i) => value !== previous.dependencies[i])) {
        memos[index] = { dependencies, value: fn() };
      }
      return memos[index].value;
    },
    useRef: (initial: unknown) => refs[refCursor++] ?? (refs[refCursor - 1] = { current: initial }),
    useEffect: () => {},
    useState: (initial: unknown) => [initial, () => {}],
    Suspense: "suspense",
  };
  const globals = {
    Blob,
    URL: { createObjectURL: (blob: Blob) => { blobs.push(blob); return "blob:fixture"; }, revokeObjectURL: (url: string) => revoked.push(url) },
    window: { open: (...args: unknown[]) => { opened.push(args); return {}; }, setTimeout: (fn: () => void) => timers.push(fn) },
    document: { createElement: () => {
      const anchor = { href: "", download: "", click() { anchors.push(anchor); }, remove() {} }; return anchor;
    }, body: { appendChild() {} } },
  };
  const load = (relative: string, deps: Record<string, unknown>, appended = "") => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any -- Complete transpiled modules expose dynamically named functions.
    const exports: Record<string, any> = {};
    const source = readFileSync(new URL(relative, import.meta.url), "utf8")
      .replace("import.meta.env.VITE_ASSISTANT_UI_V2", '"true"') + appended;
    vm.runInNewContext(ts.transpileModule(source, { compilerOptions: {
      module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, esModuleInterop: true, target: ts.ScriptTarget.ES2022,
    } }).outputText, { exports, require: (name: string) => {
      assert.ok(name in deps, `Unexpected import ${name}`); return deps[name];
    }, ...globals });
    return exports;
  };
  const helper = load("../lib/authenticatedDownload.ts", { "@/lib/api": { api } });
  const getArtifactDownloadUrl = (id: string) => `/api/v1/assistant/artifacts/${id}/download`;
  const resolver = load("../features/chat/sessionArtifactHydration.ts", {});
  const hydration = load("../pages/assistant/artifactHydration.ts", {
    "@/api/assistant": { getArtifactDownloadUrl }, "@/features/chat/sessionArtifactHydration": resolver,
  });
  const locale = JSON.parse(readFileSync(new URL(`../i18n/locales/${language}.json`, import.meta.url), "utf8"));
  const t = (key: string) => key.split(".").reduce((value, part) => value?.[part], locale) ?? key;
  const production = load("./StreamOutput.tsx", {
    react, "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-markdown": { __esModule: true, default: "markdown", defaultUrlTransform: (value: string) => value },
    "lucide-react": { ImageIcon: "image", Download: "download", ExternalLink: "external", FileDown: "file" },
    "@/hooks/useLatexCopy": { useLatexCopy() {} }, "react-i18next": { useTranslation: () => ({ t }) },
    "@/lib/api": { api }, "@/api/assistant": { getArtifactDownloadUrl },
    "@/lib/authenticatedDownload": helper, "@/hooks/use-toast": { toast: { error: (...args: string[]) => notifications.push(args) } },
    "./gfmDetection": { needsGfmPlugin: (text: string) => text.includes("| table |") },
    "./streamingMarkdown": { splitStreamingMarkdownBlocks: (text: string) => [text] },
  });
  function render(artifacts: unknown[] = [artifact], text = `[report](${href})`, artifactIds?: unknown[]) {
    refCursor = 0; memoCursor = 0;
    const tree = production.StreamOutput({ text, isStreaming: false, artifacts, artifactIds });
    const block = tree.props.children[0][0];
    return block.type === "suspense"
      ? block.props.children.props.components
      : block.type(block.props).props.components;
  }
  function link(mapping: { a: (props: { href: string; children: string }) => Tree }, url = href) {
    const component = mapping.a({ href: url, children: "Untrusted filename.exe" });
    assert.equal(typeof component.type, "function");
    return (component.type as (props: Record<string, unknown>) => Tree)(component.props);
  }
  function click(anchor: Tree) {
    let prevented = false;
    anchor.props.onClick({ preventDefault() { prevented = true; } });
    return prevented;
  }
  function historyMessage(metadata: Record<string, unknown> = { artifact_ids: [artifactId] }, listed: unknown[] = []) {
    const source = readFileSync(new URL("../pages/assistant/hooks/useChatSession.ts", import.meta.url), "utf8");
    const parsed = ts.createSourceFile("useChatSession.ts", source, ts.ScriptTarget.Latest);
    const deps: Record<string, unknown> = {};
    for (const statement of parsed.statements) {
      if (ts.isImportDeclaration(statement) && ts.isStringLiteral(statement.moduleSpecifier)) {
        deps[statement.moduleSpecifier.text] = {};
      }
    }
    deps["../sse-events"] = { SSEEventType: {} };
    deps["../sourceVisibility"] = load("../pages/assistant/sourceVisibility.ts", {});
    // Test-only export seam appended to the entire unchanged production module.
    // No implementation is extracted or re-created by this fixture.
    const restored = load("../pages/assistant/hooks/useChatSession.ts", deps,
      "\nexport { restoreMessageMetadata as restoreHistoryFixture };"
    ).restoreHistoryFixture({ role: "assistant", content: `[report](${href})`, timestamp: "2026-10-12T12:00:00Z", metadata }, 0, "session_fixture");
    return hydration.hydrateMessageArtifacts([restored], listed)[0];
  }
  function renderHistory(message: Record<string, unknown>) {
    let lazyIndex = 0;
    const componentReact = { ...react, lazy: () => ["approval", "stream-output", "preview"][lazyIndex++] };
    const chat = load("../pages/assistant/components/ChatMessage.tsx", {
      react: componentReact, "react/jsx-runtime": { jsx, jsxs: jsx },
      "react-i18next": { useTranslation: () => ({ t }) },
      "framer-motion": { motion: { div: "motion-div" } }, "lucide-react": {},
      "../thinkingPreview": {}, "@/lib/utils": { cn: () => "" }, "./ContextDisplay": {},
      "./CitationDisplay": {}, "./Quiz": {}, "./ActivityPill": {},
      "./rightPanelContext": { useRightPanel: () => ({}) },
      "@/features/chat/messageRenderPerformance": load("../features/chat/messageRenderPerformance.ts", {}),
      "../assistantOutcome": load("../pages/assistant/assistantOutcome.ts", {}),
      "./activityTerminalState": load("../pages/assistant/components/activityTerminalState.ts", {}),
      "@/lib/authenticatedDownload": helper, "@/hooks/use-toast": { toast: { error() {} } },
    });
    const tree = chat.ChatMessage({ message });
    function find(value: unknown): Tree | undefined {
      if (Array.isArray(value)) {
        for (const child of value) { const found = find(child); if (found) return found; }
      } else if (value && typeof value === "object" && "props" in value) {
        const node = value as Tree;
        if (node.type === "stream-output") return node;
        return find(node.props.children);
      }
      return undefined;
    }
    const stream = find(tree);
    if (!stream) return undefined;
    return render(stream.props.artifacts ?? [], stream.props.text, stream.props.artifactIds);
  }
  return { requests, opened, anchors, notifications, blobs, payload, timers, revoked, render, link, click, hydration, t, historyMessage, renderHistory,
    setResponse(fn: typeof response) { response = fn; } };
}
const settle = () => new Promise<void>((resolve) => setImmediate(resolve));

for (const language of ["en-US", "zh-CN"]) {
  for (const [status, key] of [[200, null], [404, "artifact.errors.unavailableForAccount"], [410, "artifact.errors.expired"], [503, "artifact.errors.serviceUnavailable"]] as const) {
    test(`bound history artifact ${status} uses the existing download path (${language})`, async () => {
      const h = host(language);
      if (status !== 200) h.setResponse(async () => { throw { response: { status, data: "PRIVATE BODY" } }; });
      const message = h.hydration.hydrateMessageArtifacts([{ id: "message", role: "assistant", _artifactIds: [artifactId] }],
        [{ artifact_id: artifactId, filename: "report.pdf", format: "pdf", type: "document", size_bytes: 12, ready: true, source: "ai" }])[0];
      assert.equal(h.click(h.link(h.render(message.generatedArtifacts))), true);
      await settle();
      assert.equal(h.requests.length, 1); assert.equal(h.requests[0][0], href);
      assert.equal((h.requests[0][1] as { responseType: string }).responseType, "blob");
      assert.equal(h.opened.length, 0);
      if (key) {
        assert.deepEqual(h.notifications, [[h.t("artifact.errors.downloadTitle"), h.t(key)]]);
        assert.equal(h.anchors.length, 0); assert.equal(h.blobs.length, 0);
        assert.ok(!JSON.stringify(h.notifications).includes("PRIVATE BODY"));
      } else {
        assert.equal(h.anchors.length, 1); assert.equal(h.anchors[0].download, "report.pdf");
        assert.equal(h.blobs[0], h.payload); assert.equal(h.notifications.length, 0);
        h.timers.forEach((fn) => fn()); assert.deepEqual(h.revoked, ["blob:fixture"]);
      }
    });
  }
}

test("overlapping links for the same artifact share an in-flight guard; failure permits manual retry", async () => {
  const h = host();
  let reject!: (error: unknown) => void;
  h.setResponse(() => new Promise((_, rejectResponse) => { reject = rejectResponse; }));
  const mapping = h.render();
  h.click(h.link(mapping)); h.click(h.link(mapping));
  assert.equal(h.requests.length, 1);
  // A normal render while the request remains pending must retain the guard.
  h.click(h.link(h.render())); assert.equal(h.requests.length, 1);
  reject({ response: { status: 503 } }); await settle();
  assert.equal(h.notifications.length, 1); assert.equal(h.requests.length, 1);
  h.setResponse(async () => ({ data: h.payload }));
  h.click(h.link(h.render())); await settle();
  assert.equal(h.requests.length, 2); assert.equal(h.anchors.length, 1);
});

test("success permits a later fresh manual download", async () => {
  const h = host();
  h.click(h.link(h.render())); await settle();
  h.click(h.link(h.render())); await settle();
  assert.equal(h.requests.length, 2); assert.equal(h.anchors.length, 2);
});

test("late history hydration updates link mapping", async () => {
  const h = host();
  h.click(h.link(h.render([]))); assert.equal(h.requests.length, 0); assert.equal(h.opened.length, 1);
  h.click(h.link(h.render([artifact]))); await settle();
  assert.equal(h.requests.length, 1); assert.equal(h.anchors.length, 1);
});

test("unmatched, noncanonical and external URLs retain ordinary navigation without API reads", async () => {
  const h = host();
  for (const url of ["https://example.test/report.pdf", "https://example.test/api/v1/assistant/artifacts/art_history_fixture/download",
    "//example.test/artifacts/file/download", "/api/v1/assistant/artifacts/art_other/download", `${href}?extra=1`, "/api/v1/other/download"]) {
    const before = h.opened.length;
    h.click(h.link(h.render(), url));
    assert.equal(h.opened.length, before + 1); assert.equal(h.opened.at(-1)?.[0], url);
  }
  assert.equal(h.click(h.link(h.render(), "#section")), false);
  // Missing metadata, even for an old canonical prose link, is not permission
  // to discover identity or route an arbitrary URL through the helper.
  h.click(h.link(h.render([])));
  h.click(h.link(h.render([{ ...artifact, id: "different" }])));
  h.click(h.link(h.render([{ ...artifact, url: "https://example.test/file.pdf" }])));
  await settle(); assert.equal(h.requests.length, 0); assert.equal(h.notifications.length, 0);
});

for (const text of [`[report](${href}) | table |`, `$x$ [report](${href})`]) {
  test(`GFM/math renderer receives the same bound-artifact link mapping: ${text}`, async () => {
    const h = host(); h.click(h.link(h.render([artifact], text))); await settle();
    assert.equal(h.requests.length, 1); assert.equal(h.anchors.length, 1); assert.equal(h.opened.length, 0);
  });
}

const metadataHref = `/api/v1/assistant/artifacts/${artifactId}`;
const detail = { artifact_id: artifactId, download_url: href, filename: "server-report.pdf", size_bytes: 12, ready: true, source: "ai" };

for (const language of ["en-US", "zh-CN"]) {
  test(`retained historical ID resolves on click through actual restore, hydration and ChatMessage (${language})`, async () => {
    const h = host(language);
    const message = h.historyMessage();
    assert.equal(message.generatedArtifacts, undefined);
    const mapping = h.renderHistory(message)!;
    assert.equal(h.requests.length, 0);
    h.setResponse(async (url) => ({ data: url === metadataHref ? detail : h.payload }));
    h.click(h.link(mapping)); await settle();
    assert.deepEqual(h.requests.map((request) => request[0]), [metadataHref, href]);
    assert.equal(h.anchors[0].download, detail.filename);
    assert.equal(h.opened.length, 0);
    assert.equal(h.blobs[0], h.payload);
    h.timers.forEach((fn) => fn()); assert.deepEqual(h.revoked, ["blob:fixture"]);
  });
  for (const status of [401, 403, 404, 410, 503]) {
    test(`historical detail ${status} stops download and reports safe feedback (${language})`, async () => {
      const h = host(language);
      h.setResponse(async () => { throw { response: { status, data: "PRIVATE BODY" } }; });
      h.click(h.link(h.renderHistory(h.historyMessage())!)); await settle();
      assert.deepEqual(h.requests.map((request) => request[0]), [metadataHref]);
      const keys: Record<number, string> = { 401: "signIn", 403: "accessDenied", 404: "unavailableForAccount", 410: "expired", 503: "serviceUnavailable" };
      assert.deepEqual(h.notifications, [[h.t("artifact.errors.downloadTitle"), h.t(`artifact.errors.${keys[status]}`)]]);
      assert.ok(!JSON.stringify(h.notifications).includes("PRIVATE BODY"));
      assert.equal(h.opened.length, 0); assert.equal(h.anchors.length, 0);
    });
  }
}

test("fresh metadata success does not bypass a later revoked download", async () => {
  const h = host();
  h.setResponse(async (url) => { if (url === metadataHref) return { data: detail }; throw { response: { status: 403 } }; });
  h.click(h.link(h.renderHistory(h.historyMessage())!)); await settle();
  assert.deepEqual(h.requests.map((request) => request[0]), [metadataHref, href]);
  assert.deepEqual(h.notifications, [[h.t("artifact.errors.downloadTitle"), h.t("artifact.errors.accessDenied")]]);
  assert.equal(h.anchors.length, 0); assert.equal(h.opened.length, 0);
});

for (const changed of [null, { ...detail, artifact_id: "different" }, { ...detail, download_url: "https://example.test/report.pdf" },
  { ...detail, download_url: `${href}?signed=1` }, { ...detail, filename: "" }, { ...detail, source: "user" },
  { ...detail, ready: false }, { ...detail, size_bytes: 0 }, { ...detail, size_bytes: "12" }]) {
  test(`invalid or unfinished detail never downloads: ${JSON.stringify(changed)}`, async () => {
    const h = host(); h.setResponse(async () => ({ data: changed }));
    h.click(h.link(h.renderHistory(h.historyMessage())!)); await settle();
    assert.equal(h.requests.length, 1); assert.equal(h.anchors.length, 0); assert.equal(h.notifications.length, 1);
  });
}

test("pending detail is coalesced across render; each later manual click reads fresh detail", async () => {
  const h = host(); let reject!: (error: unknown) => void;
  h.setResponse(() => new Promise((_, fail) => { reject = fail; }));
  h.click(h.link(h.renderHistory(h.historyMessage())!));
  h.click(h.link(h.renderHistory(h.historyMessage())!));
  assert.equal(h.requests.length, 1);
  reject({ response: { status: 503 } }); await settle();
  h.setResponse(async (url) => ({ data: url === metadataHref ? detail : h.payload }));
  h.click(h.link(h.renderHistory(h.historyMessage())!)); await settle();
  h.click(h.link(h.renderHistory(h.historyMessage())!)); await settle();
  assert.deepEqual(h.requests.map((request) => request[0]), [metadataHref, metadataHref, href, metadataHref, href]);
  assert.equal(h.anchors.length, 2);
});

test("fully missing binding, redacted sources, unrelated and noncanonical prose never discover metadata", async () => {
  const h = host();
  h.click(h.link(h.renderHistory(h.historyMessage({}))!));
  assert.equal(h.opened.length, 1);
  assert.equal(h.renderHistory(h.historyMessage({ artifact_ids: [artifactId], source_access_revoked: true })), undefined);
  for (const url of ["https://example.test/report.pdf", `${href}?extra=1`, "/api/v1/assistant/artifacts/other/download", "//example.test/artifacts/x/download"]) {
    h.click(h.link(h.renderHistory(h.historyMessage())!, url));
  }
  for (const id of ["..", "a/b", "a%2Fb", null, 12]) {
    h.click(h.link(h.render([], `[report](${href})`, [id])));
  }
  await settle(); assert.equal(h.requests.length, 0); assert.equal(h.notifications.length, 0);
});


test("partial hydration preserves missing explicit IDs beside a hydrated file", async () => {
  const h = host(); const presentId = "art_present_fixture";
  const presentHref = `/api/v1/assistant/artifacts/${presentId}/download`;
  const message = h.historyMessage({ artifact_ids: [artifactId, presentId] }, [{
    artifact_id: presentId, filename: "present.pdf", format: "pdf", type: "document",
    size_bytes: 10, ready: true, source: "ai",
  }]);
  assert.deepEqual(Array.from(message._artifactIds), [artifactId, presentId]);
  assert.deepEqual(Array.from(message.generatedArtifacts, (item: { id: string }) => item.id), [presentId]);
  h.setResponse(async (url) => ({ data: url === metadataHref ? detail : h.payload }));
  const mapping = h.renderHistory(message)!;
  assert.equal(h.requests.length, 0);
  h.click(h.link(mapping, presentHref)); await settle();
  assert.deepEqual(h.requests.map((request) => request[0]), [presentHref]);
  assert.equal(h.anchors[0].download, "present.pdf");
  h.click(h.link(mapping)); await settle();
  assert.deepEqual(h.requests.map((request) => request[0]), [presentHref, metadataHref, href]);
  assert.equal(h.anchors[1].download, detail.filename); assert.equal(h.opened.length, 0);
});

test("partial hydration keeps denied file identity without inventing output metadata", async () => {
  const h = host(); const presentId = "art_present_fixture";
  const listed = [{ artifact_id: presentId, filename: "present.pdf", format: "pdf", type: "document", size_bytes: 10, ready: true, source: "ai" }];
  const message = h.historyMessage({ artifact_ids: [artifactId, presentId, artifactId] }, listed);
  assert.deepEqual(Array.from(message._artifactIds), [artifactId, presentId]);
  assert.deepEqual(Array.from(message.generatedArtifacts, (item: { id: string }) => item.id), [presentId]);
  const outputs = h.hydration.buildLatestRunOutputFilesFromArtifacts([message], listed);
  assert.deepEqual(Array.from(outputs, (item: { artifact_id: string }) => item.artifact_id), [presentId]);
  const mapping = h.renderHistory(message)!;
  assert.equal(h.requests.length, 0);
  h.setResponse(async () => { throw { response: { status: 403, data: "PRIVATE BODY" } }; });
  h.click(h.link(mapping)); await settle();
  assert.deepEqual(h.requests.map((request) => request[0]), [metadataHref]);
  assert.deepEqual(h.notifications, [[h.t("artifact.errors.downloadTitle"), h.t("artifact.errors.accessDenied")]]);
  assert.equal(h.anchors.length, 0); assert.equal(h.opened.length, 0);
});
