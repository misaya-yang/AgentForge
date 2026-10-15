import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { renderToStaticMarkup } from "react-dom/server";
import type { ChatMessage } from "../ChatWindow.tsx";
const require = createRequire(import.meta.url);
function render(stats: NonNullable<ChatMessage["stats"]>, locale = "en-US") {
  const dictionary = JSON.parse(readFileSync(new URL(`../../i18n/locales/${locale}.json`, import.meta.url), "utf8"));
  const t = (key: string) => key.split(".").reduce((value, part) => value[part], dictionary);
  const modules: Record<string, unknown> = {
    "react/jsx-runtime": require("react/jsx-runtime"),
    "framer-motion": { motion: { div: "div" } }, "react-i18next": { useTranslation: () => ({ t }) },
    "lucide-react": { Clock: () => null, Zap: () => null, MessageSquare: () => null },
  };
  const exports: { StatsBadge?: (props: { stats: NonNullable<ChatMessage["stats"]> }) => React.ReactNode } = {};
  const source = readFileSync(new URL("./StatsBadge.tsx", import.meta.url), "utf8");
  vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText,
    { exports, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; } });
  return renderToStaticMarkup(exports.StatsBadge!({ stats }));
}
test("PG02 actual badge renders unknown usage even with no duration or numeric total", () => {
  const html = render({ tokenUsageSource: "unknown" });
  assert.match(html, /data-token-usage-source="unknown"/); assert.match(html, /tokens: unknown/);
});
test("PG02 actual badge preserves reported zero without showing an estimate as real usage", () => {
  const html = render({ tokenUsageSource: "reported", inputTokens: 0, outputTokens: 0, totalTokens: 0,
    textTokenEstimate: { inputTokens: 1, outputTokens: 2, totalTokens: 3 } });
  assert.match(html, /Server-reported/); assert.match(html, /tokens: 0/); assert.match(html, /input: 0/);
  assert.match(html, /Unlabeled measurement sources are unknown/); assert.doesNotMatch(html, /data-token-usage-source="estimated"/);
});
test("PG02 actual badge visibly separates missing usage from a local text estimate", () => {
  const html = render({ tokenUsageSource: "reported", inputTokens: 9,
    textTokenEstimate: { inputTokens: 1, outputTokens: 2, totalTokens: 3 } });
  assert.match(html, /tokens: unknown/); assert.match(html, /output: unknown/);
  assert.match(html, /data-token-usage-source="estimated"/); assert.match(html, /Local text estimate: ~3/);
  assert.match(html, /not reported usage or a charge/);
});
test("PG02 actual badge preserves legacy recorded counts with unknown source and both locales", () => {
  assert.match(render({ tokenUsageSource: "unknown", totalTokens: 5 }), /Source unknown/);
  assert.match(render({ tokenUsageSource: "unknown", totalTokens: 5 }), /tokens: 5/);
  assert.match(render({ tokenUsageSource: "unknown" }, "zh-CN"), /来源未知/);
});
test("existing consumers without provenance retain their original badge presentation", () => {
  const html = render({ inputTokens: 3, outputTokens: 2, totalTokens: 5 });
  assert.match(html, />5<\/span>/); assert.doesNotMatch(html, /data-token-usage-source/);
});
test("actual badge labels mixed upstream, Gateway-estimated and derived fields separately", () => {
  const html = render({ tokenUsageSource: "reported", inputTokens: 0, outputTokens: 3, totalTokens: 3,
    tokenProvenance: { input_tokens: "upstream", output_tokens: "estimated", total_tokens: "derived" } });
  assert.match(html, /input: 0 \(upstream-reported\)/); assert.match(html, /output: 3 \(server estimate\)/);
  assert.match(html, /tokens: 3 \(derived\)/);
});

test("reported routing badge labels observation without claiming verified execution", () => {
 const stats = { reportedRouting: { source: "upstream_reported" as const, status: "selected" as const, provider_id: "provider-b", model_id: "model-b" } };
 const html = render(stats); assert.match(html, /Latest upstream-reported selection/); assert.match(html, /provider-b/); assert.match(html, /model-b/); assert.match(html, /not verified execution or billing/);
 assert.match(render(stats, "zh-CN"), /上游报告的最近模型选择/);
});
