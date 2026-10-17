import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

type DownloadModule = typeof import("./authenticatedDownload.ts");
type Tree = { type: string | ((props: Record<string, unknown>) => Tree); props: Record<string, unknown> };
const artifactUrl = "/api/v1/assistant/artifacts/art_fixture/download";

// Run the complete production functions. Only HTTP/browser primitives and the
// component renderer are replaced. These tests do not claim browser acceptance.
function host(language = "en-US") {
  const requests: unknown[][] = [];
  const links: Array<{ href: string; download: string; rel: string; clicked: boolean; removed: boolean }> = [];
  const blobs: Blob[] = [];
  const revoked: string[] = [];
  const timers: Array<() => void> = [];
  const opened: unknown[][] = [];
  const notifications: Array<{ kind: string; text: string[] }> = [];
  let failure: unknown;
  let blocked = false;
  let previewClosed = 0;
  let previewLocation = "";
  const bytes = new Blob(["%PDF-fixture"], { type: "application/pdf" });
  const api = { get: async (...args: unknown[]) => {
    requests.push(args);
    if (failure) throw failure;
    return { data: bytes };
  } };
  const browser = {
    Blob,
    URL: {
      createObjectURL(blob: Blob) { blobs.push(blob); return `blob:fixture-${blobs.length}`; },
      revokeObjectURL(href: string) { revoked.push(href); },
    },
    document: {
      createElement(tag: string) {
        assert.equal(tag, "a");
        const link = { href: "", download: "", rel: "", clicked: false, removed: false,
          click() { this.clicked = true; }, remove() { this.removed = true; } };
        links.push(link); return link;
      },
      body: { appendChild() {} },
    },
    window: {
      open(...args: unknown[]) {
        opened.push(args);
        return blocked ? null : {
          location: { replace(href: string) { previewLocation = href; } },
          close() { previewClosed++; },
        };
      },
      setTimeout(fn: () => void, delay: number) { assert.equal(delay, 60_000); timers.push(fn); },
    },
  };
  const download = {} as DownloadModule;
  const source = readFileSync(new URL("./authenticatedDownload.ts", import.meta.url), "utf8");
  vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText,
    { exports: download, require: (name: string) => { assert.equal(name, "@/lib/api"); return { api }; }, ...browser });

  const locale = JSON.parse(readFileSync(new URL(`../i18n/locales/${language}.json`, import.meta.url), "utf8"));
  const translate = (key: string, fallback?: string) => key.split(".").reduce((value, part) => value?.[part], locale) ?? fallback ?? key;
  let state: unknown[] = [];
  let cursor = 0;
  const effects: Array<() => void | (() => void)> = [];
  const react = {
    useMemo: (fn: () => unknown) => fn(),
    useCallback: (fn: unknown) => fn,
    useState(initial: unknown) {
      const index = cursor++;
      if (!(index in state)) state[index] = typeof initial === "function" ? initial() : initial;
      return [state[index], (value: unknown) => { state[index] = value; }];
    },
    useEffect(fn: () => void | (() => void), dependencies: unknown[]) {
      const index = cursor++;
      const previous = state[index] as unknown[] | undefined;
      if (!previous || dependencies.some((value, i) => value !== previous[i])) {
        state[index] = dependencies;
        effects.push(fn);
      }
    },
  };
  const jsx = (type: Tree["type"], props: Tree["props"]) => ({ type, props });
  const modules: Record<string, unknown> = {
    react, "react/jsx-runtime": { jsx, jsxs: jsx, Fragment: "fragment" },
    "react-i18next": { useTranslation: () => ({ t: translate }) },
    "motion/react": { motion: new Proxy({}, { get: (_, key) => `motion.${String(key)}` }), AnimatePresence: "presence" },
    "lucide-react": new Proxy({}, { get: (_, key) => String(key) }),
    "@/lib/utils": { cn: () => "" },
    "@/hooks/use-toast": { toast: {
      error: (...text: string[]) => notifications.push({ kind: "error", text }),
      success: (...text: string[]) => notifications.push({ kind: "success", text }),
    } },
    "@/lib/format": { formatFileSize: String, getFormatLabel: (format: string) => format },
    "@/lib/clipboard": { copyToClipboard: async () => {} },
    "@/lib/api": { api },
    "@/lib/authenticatedDownload": download,
  };
  const panel: { ArtifactsPanel?: (props: Record<string, unknown>) => Tree } = {};
  const panelSource = readFileSync(new URL("../components/artifacts/ArtifactsPanel.tsx", import.meta.url), "utf8")
    .replaceAll("import.meta.env.VITE_ASSISTANT_UI_V2", '"true"');
  vm.runInNewContext(ts.transpileModule(panelSource, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText,
    { exports: panel, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; }, ...browser });

  return { download, requests, links, blobs, bytes, revoked, timers, opened, notifications, effects, translate,
    fail(error: unknown) { failure = error; }, block() { blocked = true; },
    previewClosed: () => previewClosed, previewLocation: () => previewLocation,
    renderPanel(artifacts: unknown[], outputFiles: unknown[] = []) {
      state = []; cursor = 0;
      return panel.ArtifactsPanel!({ isOpen: true, artifacts, outputFiles, onClose() {}, executionStatus: "success", executionOutput: "" });
    },
    renderCard(card: Tree, reset = true) {
      if (reset) state = [];
      cursor = 0;
      assert.equal(typeof card.type, "function");
      return (card.type as (props: Tree["props"]) => Tree)(card.props);
    },
  };
}

function nodes(tree: unknown): Tree[] {
  if (Array.isArray(tree)) return tree.flatMap(nodes);
  if (!tree || typeof tree !== "object") return [];
  const node = tree as Tree;
  return [node, ...nodes(node.props?.children)];
}
function card(tree: Tree, name = "ArtifactCard"): Tree {
  const result = nodes(tree).find(node => typeof node.type === "function" && node.type.name === name);
  assert.ok(result); return result;
}
function pdf() {
  return { id: "art_fixture", type: "document", format: "pdf", title: "Fixture", filename: "fixture.pdf",
    url: artifactUrl, mimeType: "application/pdf", sizeBytes: 12, ready: true, createdAt: new Date(0) };
}
async function settle() { for (let i = 0; i < 12; i++) await Promise.resolve(); }

for (const [status, reason] of [
  [401, "signIn"], [403, "accessDenied"], [404, "unavailableForAccount"], [410, "expired"],
  [409, "unfinished"], [413, "tooLarge"], [429, "rateLimited"], [500, "serviceUnavailable"], [503, "serviceUnavailable"],
] as const) {
  test(`HTTP ${status} preserves the rejection and shows a safe actionable reason without retry`, async () => {
    const h = host();
    const error = { response: { status, data: new Blob(["private-source-url and secret response"]) }, message: "private-secret" };
    h.fail(error);
    await assert.rejects(h.download.downloadAssistantArtifact(artifactUrl, "fixture.pdf"), observed => observed === error);
    assert.equal(h.download.artifactAccessErrorKey(error), `artifact.errors.${reason}`);
    assert.equal(h.requests.length, 1);
    assert.equal(h.links.length, 0);
    assert.equal(h.blobs.length, 0);
    assert.equal(h.timers.length, 0);
    assert.equal(h.translate(h.download.artifactAccessErrorKey(error)).includes("private"), false);
  });
}

test("concealed 404 is never presented as proof of expiration or deletion in either language", () => {
  for (const language of ["en-US", "zh-CN"]) {
    const h = host(language);
    const key = h.download.artifactAccessErrorKey({ response: { status: 404, data: { detail: "expired" } } });
    assert.equal(key, "artifact.errors.unavailableForAccount");
    assert.doesNotMatch(h.translate(key), /expired|deleted|过期|删除/i);
    for (const status of [401, 403, 404, 409, 410, 413, 429, 503]) {
      const errorKey = h.download.artifactAccessErrorKey({ response: { status } });
      assert.notEqual(h.translate(errorKey), errorKey);
    }
  }
});

test("structured network failures and unknown errors stay separate without displaying raw bodies", () => {
  const h = host();
  for (const code of ["ERR_NETWORK", "ECONNABORTED", "ETIMEDOUT"]) {
    assert.equal(h.download.artifactAccessErrorKey({ code, message: "private" }), "artifact.errors.connectionFailed");
  }
  for (const error of [null, "HTTP 403 private", new Error("expired private"), { response: { status: "403" } }]) {
    assert.equal(h.download.artifactAccessErrorKey(error), "artifact.errors.downloadFailed");
    assert.equal(h.download.artifactAccessErrorKey(error, "preview"), "artifact.errors.previewFailed");
  }
});

test("an authenticated download saves the exact blob once and revokes only its temporary URL", async () => {
  const h = host();
  await h.download.downloadAssistantArtifact(artifactUrl, "报告.pdf");
  assert.equal(h.requests[0][0], artifactUrl);
  assert.equal((h.requests[0][1] as { responseType: string }).responseType, "blob");
  assert.equal(h.blobs[0], h.bytes);
  assert.equal(h.links[0].download, "报告.pdf");
  assert.equal(h.links[0].clicked, true);
  assert.equal(h.links[0].removed, true);
  assert.equal(h.revoked.length, 0);
  h.timers[0]();
  assert.deepEqual(h.revoked, ["blob:fixture-1"]);
});

test("a failed preview closes its blank window and does not prevent a later manual download", async () => {
  const h = host(); const error = { response: { status: 503 } };
  h.fail(error);
  await assert.rejects(h.download.openAssistantArtifact(artifactUrl), observed => observed === error);
  assert.equal(h.previewClosed(), 1);
  assert.equal(h.previewLocation(), "");
  assert.equal(h.links.length, 0);
  h.fail(undefined);
  await h.download.downloadAssistantArtifact(artifactUrl, "fixture.pdf");
  assert.equal(h.requests.length, 2);
  assert.equal(h.links[0].clicked, true);
});

test("a blocked preview makes no GET, reports a download alternative, and leaves download usable", async () => {
  const h = host(); h.block();
  let previewError: unknown;
  await assert.rejects(h.download.openAssistantArtifact(artifactUrl), error => {
    previewError = error;
    return true;
  });
  assert.equal(h.requests.length, 0);
  assert.equal(h.download.artifactAccessErrorKey(previewError, "preview"), "artifact.errors.popupBlocked");
  await h.download.downloadAssistantArtifact(artifactUrl, "fixture.pdf");
  assert.equal(h.requests.length, 1);
  assert.equal(h.links[0].clicked, true);
});

test("protected preview fetches once while external URLs never receive authenticated API transport", async () => {
  const h = host();
  await h.download.openAssistantArtifact(artifactUrl);
  assert.equal(h.previewLocation(), "blob:fixture-1");
  assert.equal(h.requests.length, 1);
  await h.download.downloadAssistantArtifact("https://storage.example/file", "external.pdf");
  await h.download.openAssistantArtifact("https://storage.example/file");
  assert.equal(h.requests.length, 1);
  assert.equal(h.links[0].href, "https://storage.example/file");
  await assert.rejects(h.download.downloadAssistantArtifact("javascript:alert(1)", "unsafe"));
  assert.equal(h.links.length, 1);
});

test("the real drawer download callback shows the classified localized failure and no success", async () => {
  const h = host("zh-CN"); h.fail({ response: { status: 403, data: "private-source" } });
  const fileCard = card(h.renderPanel([pdf()]));
  await (fileCard.props.onDownload as () => Promise<void>)();
  assert.deepEqual(h.notifications, [{ kind: "error", text: [h.translate("artifact.errors.downloadTitle"), h.translate("artifact.errors.accessDenied")] }]);
  assert.equal(h.links.length, 0);
  assert.equal(h.requests.length, 1);
});

test("the real drawer PDF preview failure leaves its independent download button enabled", async () => {
  const h = host(); h.fail({ response: { status: 404 } });
  const buttons = nodes(h.renderCard(card(h.renderPanel([pdf()])))).filter(node => node.type === "button");
  (buttons[0].props.onClick as () => void)(); await settle();
  assert.equal(h.notifications[0].text[1], h.translate("artifact.errors.unavailableForAccount"));
  assert.equal(buttons[1].props.disabled, false);
  h.fail(undefined);
  await (buttons[1].props.onClick as () => Promise<void>)();
  assert.equal(h.links[0].clicked, true);
});

test("the real image card explains an authenticated preview denial and keeps manual download available", async () => {
  const h = host(); h.fail({ response: { status: 403 } });
  const imageCard = card(h.renderPanel([{ ...pdf(), type: "image", mimeType: "image/png", format: "png" }]), "ImageCard");
  h.renderCard(imageCard);
  h.effects.splice(0).forEach(effect => effect()); await settle();
  const tree = h.renderCard(imageCard, false);
  const alert = nodes(tree).find(node => node.props?.role === "alert");
  assert.equal(alert?.props.children, h.translate("artifact.errors.accessDenied"));
  const download = nodes(tree).find(node => node.type === "button" && node.props["aria-label"] === h.translate("artifact.download"));
  assert.equal(download?.props.disabled, false);
  assert.equal(h.requests.length, 1);
});


test("the real drawer preserves per-file readiness and de-duplicates restored output identifiers", () => {
  const h = host();
  const tree = h.renderPanel([pdf(), { ...pdf(), id: "art_unfinished", filename: "unfinished.pdf", ready: false, sizeBytes: 0 }], [{
    artifact_id: "art_fixture", filename: "fixture.pdf", content_base64: "", download_url: artifactUrl, mime_type: "application/pdf", size_bytes: 12,
  }]);
  const cards = nodes(tree).filter(node => typeof node.type === "function" && node.type.name === "ArtifactCard");
  assert.equal(cards.length, 2);
  const readyButtons = nodes(h.renderCard(cards[0])).filter(node => node.type === "button");
  assert.equal(readyButtons.at(-1)?.props.disabled, false);
  const unfinishedButtons = nodes(h.renderCard(cards[1])).filter(node => node.type === "button");
  assert.equal(unfinishedButtons.every(node => node.props.disabled === true), true);
  assert.equal(h.requests.length, 0);
});

test("a prior successful preview does not authorize a later download or reuse its cached blob", async () => {
  const h = host();
  await h.download.openAssistantArtifact(artifactUrl);
  assert.equal(h.previewLocation(), "blob:fixture-1");
  const revokedAccess = { response: { status: 403 } };
  h.fail(revokedAccess);
  await assert.rejects(h.download.downloadAssistantArtifact(artifactUrl, "fixture.pdf"), error => error === revokedAccess);
  assert.equal(h.requests.length, 2);
  assert.equal(h.links.length, 0);
  assert.equal(h.blobs.length, 1);
});


test("the real image card replaces a decoded-load failure with an alert without disabling download", async () => {
  const h = host();
  const imageCard = card(h.renderPanel([{ ...pdf(), type: "image", mimeType: "image/png", format: "png" }]), "ImageCard");
  h.renderCard(imageCard);
  h.effects.splice(0).forEach(effect => effect()); await settle();
  const loaded = nodes(h.renderCard(imageCard, false)).find(node => node.type === "img");
  assert.equal(loaded?.props.src, "blob:fixture-1");
  (loaded?.props.onError as () => void)();
  const failed = nodes(h.renderCard(imageCard, false));
  assert.equal(failed.some(node => node.type === "img"), false);
  assert.equal(failed.find(node => node.props?.role === "alert")?.props.children, h.translate("artifact.errors.previewFailed"));
  const button = failed.find(node => node.type === "button" && node.props["aria-label"] === h.translate("artifact.download"));
  assert.equal(button?.props.disabled, false);
  await (button?.props.onClick as () => Promise<void>)();
  assert.equal(h.requests.length, 2);
  assert.equal(h.links[0].clicked, true);
});

test("the real image card never carries a previous blob into a replacement file's pending or denied read", async () => {
  const h = host();
  const imageCard = card(h.renderPanel([{ ...pdf(), type: "image", mimeType: "image/png", format: "png" }]), "ImageCard");
  h.renderCard(imageCard);
  const cleanup = h.effects.splice(0)[0](); await settle();
  assert.equal(nodes(h.renderCard(imageCard, false)).find(node => node.type === "img")?.props.src, "blob:fixture-1");
  const nextUrl = "/api/v1/assistant/artifacts/art_replacement/download";
  imageCard.props = { ...imageCard.props, item: { ...(imageCard.props.item as object), url: nextUrl } };
  h.fail({ response: { status: 403 } });
  // The new render itself is source-bound; it need not wait for an effect.
  const pending = nodes(h.renderCard(imageCard, false));
  assert.equal(pending.some(node => node.type === "img"), false);
  assert.equal(pending.some(node => node.props?.role === "alert"), false);
  if (typeof cleanup === "function") cleanup();
  h.effects.splice(0).forEach(effect => effect()); await settle();
  const denied = nodes(h.renderCard(imageCard, false));
  assert.equal(denied.some(node => node.type === "img"), false);
  assert.equal(denied.find(node => node.props?.role === "alert")?.props.children, h.translate("artifact.errors.accessDenied"));
  assert.deepEqual(h.requests.map(request => request[0]), [artifactUrl, nextUrl]);
  assert.deepEqual(h.revoked, ["blob:fixture-1"]);
});
