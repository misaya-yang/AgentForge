import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import vm from "node:vm";
import { createElement, type ComponentType } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import ts from "typescript";
import type { ChatMessage } from "../ChatWindow.tsx";

const require = createRequire(import.meta.url);
function load(path: string, modules: Record<string, unknown>) {
  const exports: Record<string, unknown> = {};
  const source = readFileSync(new URL(path, import.meta.url), "utf8");
  vm.runInNewContext(ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, { exports, require: (name: string) => {
    assert.ok(name in modules, `Unexpected dependency: ${name}`); return modules[name];
  } });
  return exports;
}
function render(message: Partial<ChatMessage> = {}, locale = "en-US", visible = true) {
  const dictionary = JSON.parse(readFileSync(new URL(`../../i18n/locales/${locale}.json`, import.meta.url), "utf8"));
  const t = (key: string) => key.split(".").reduce((value, part) => value[part], dictionary);
  const utils = load("../../lib/utils.ts", {
    clsx: require("clsx"), "tailwind-merge": require("tailwind-merge"), axios: require("axios"),
  });
  const realReact = require("react");
  // Real hooks and rendering; lazy children suspend so their existing fallback is visible.
  const modules = {
    react: { ...realReact, lazy: () => () => { throw new Promise(() => {}); } },
    "react/jsx-runtime": require("react/jsx-runtime"),
    "react-i18next": { useTranslation: () => ({ t }) },
    "framer-motion": { motion: { div: ({ children }: { children: React.ReactNode }) => createElement("div", null, children) } },
    "@/lib/utils": utils,
    "@/features/chat/messageRenderPerformance": load("../../features/chat/messageRenderPerformance.ts", {}),
    "./MessageAvatar": { MessageAvatar: () => null },
    "./StatsBadge": { StatsBadge: () => createElement("span", null, "stats retained") },
    "./TimelineSection": { TimelineSection: () => createElement("span", null, "timeline retained") },
    "./ArtifactsSection": { ArtifactsSection: () => createElement("span", null, "artifacts retained") },
    "./WorkflowBeat": { WorkflowBeat: () => createElement("span", null, "tools retained") },
    "lucide-react": { Copy: () => null, Share2: () => null, RefreshCw: () => null, Check: () => null },
  };
  const Component = load("./ChatMessageItem.tsx", modules).ChatMessageItem as ComponentType<Record<string, unknown>>;
  return renderToStaticMarkup(createElement(Component, {
    message: { role: "assistant", status: "completed", completionConfirmed: true, content: "", ...message },
    index: 0, showToolCalls: visible, showTimeline: visible,
    onShare: () => {}, onRegenerate: () => {},
  }));
}
for (const locale of ["en-US", "zh-CN"]) {
  for (const content of ["", " \n\t "]) test(`completed ${JSON.stringify(content)} displays localized empty outcome in ${locale}`, () => {
    const html = render({ content }, locale);
    assert.match(html, /role="status" data-message-outcome="empty"/);
    assert.match(html, locale === "en-US" ? /Completed with no text to display\./ : /已完成，无可显示的正文。/);
    assert.doesNotMatch(html, /data-message-text|title="Copy"|title="Share conversation"|title="Regenerate response"/);
  });
}
for (const status of [undefined, "idle", "streaming", "failed", "cancelled"] as const) {
  test(`empty ${status ?? "absent-status"} row is not promoted to completed`, () => {
    const html = render({ status });
    assert.doesNotMatch(html, /data-message-outcome/);
    if (status === "failed") assert.match(html, /role="alert"/);
  });
}
for (const flags of [{ isStreaming: true }, { isThinking: true }, { role: "user" as const }]) {
  test(`completed row with ${JSON.stringify(flags)} does not display empty success`, () => {
    assert.doesNotMatch(render(flags), /data-message-outcome/);
  });
}
for (const status of ["completed", "failed", "cancelled"] as const) {
  test(`${status} partial text preserves display and actions`, () => {
    const html = render({ status, content: "Existing answer" });
    assert.match(html, /Existing answer/); assert.match(html, /data-message-text="true"/);
    assert.match(html, /title="Copy"/); assert.doesNotMatch(html, /data-message-outcome/);
  });
}
test("filtered process-only response is outside the raw empty outcome scope", () => {
  const html = render({ content: "Process (brief):\nInternal trace\nActions & observations:\n- Action: inspected" });
  assert.doesNotMatch(html, /data-message-outcome/);
  assert.doesNotMatch(html, /Internal trace|data-message-text|title="Copy"/);
});
for (const visible of [true, false]) test(`tool and artifact-only completion retains supplemental visibility ${visible}`, () => {
  const html = render({ toolCalls: [{ toolCall: { tool_call_id: "tool", name: "lookup", arguments: "{}", status: "completed" } }],
    artifacts: [{ id: "artifact" }] as unknown as ChatMessage["artifacts"], stats: { tokenUsageSource: "unknown" } }, "en-US", visible);
  assert.doesNotMatch(html, /data-message-outcome/); assert.match(html, /artifacts retained/); assert.match(html, /stats retained/);
  assert.equal(html.includes("tools retained"), visible);
  assert.doesNotMatch(html, /data-message-text|title="Copy"/);
});
test("timeline-only completion retains timeline without inventing tool results", () => {
  const html = render({ timeline: { status: "completed", steps: [{ id: "step" }] } as unknown as ChatMessage["timeline"] });
  assert.match(html, /timeline retained/); assert.doesNotMatch(html, /data-message-outcome/);
  assert.doesNotMatch(html, /tools retained|data-message-text/);
});

for (const completionConfirmed of [undefined, false]) test(`completed status without confirmed terminal ${completionConfirmed} remains unchanged`, () => {
  assert.doesNotMatch(render({ completionConfirmed }), /data-message-outcome/);
});
test("SDK tool-only completion retains lazy tool slot without empty notice", () => {
  const html = render({ sdkToolActivity: [{ id: "activity" }] as unknown as ChatMessage["sdkToolActivity"] });
  assert.doesNotMatch(html, /data-message-outcome|data-message-text|title="Copy"/);
});
