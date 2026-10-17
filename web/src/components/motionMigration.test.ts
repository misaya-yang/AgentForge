import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import vm from "node:vm";
import * as React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import ts from "typescript";

const require = createRequire(import.meta.url);
type Props = Record<string, unknown>;
function load(path: string, modules: Record<string, unknown>) {
  const exports: Record<string, React.ComponentType<Props>> = {};
  const code = ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  vm.runInNewContext(code, { exports, require: (name: string) => modules[name] ?? require(name) });
  return exports;
}
function renderMotionComponent(path: string, name: string, reduced: boolean, props: Props = {}) {
  const transitions: Props[] = [];
  const motion = new Proxy({}, { get: (_, tag: string) => (props: Props) => {
    transitions.push(props);
    const domProps = Object.fromEntries(Object.entries(props).filter(([key]) => !["initial", "animate", "exit", "transition", "whileHover", "whileTap", "layout"].includes(key)));
    return React.createElement(tag, domProps);
  } });
  const components = load(path, {
    "motion/react": { motion, useReducedMotion: () => reduced },
    "lucide-react": { Bot: () => null, Check: () => null, X: () => null },
    "react-i18next": { useTranslation: () => ({ t: (_key: string, fallback: string) => fallback }) },
    "@/lib/utils": { cn: (...values: unknown[]) => values.filter(Boolean).join(" ") },
  });
  return { html: renderToStaticMarkup(React.createElement(components[name], props)), transitions };
}

for (const reduced of [true, false]) {
  test(`Motion successor preserves welcome content and reduced-motion=${reduced}`, () => {
    const { html, transitions } = renderMotionComponent("../pages/assistant/components/WelcomeScreen.tsx", "WelcomeScreen", reduced);
    assert.match(html, /How can I help you today\?/);
    assert.match(html, /Select a model and knowledge bases/);
    assert.equal((transitions[0].transition as Props).duration, reduced ? 0 : 0.22);
    if (reduced) assert.equal(transitions[0].initial, false);
    else assert.equal((transitions[0].initial as Props).y, 12);
    assert.equal((transitions[0].animate as Props).opacity, 1);
  });

  test(`Motion successor preserves quiz answers, explanation and reduced-motion=${reduced}`, () => {
    const { html, transitions } = renderMotionComponent("../pages/assistant/components/Quiz/QuizQuestion.tsx", "QuizQuestion", reduced, {
      question: { question_type: "mc_single", question_text: "Choose one", options: [{ label: "A", text: "Answer A" }, { label: "B", text: "Answer B" }] },
      selectedAnswer: "A", onSelect: () => {}, result: { correct: true, correct_answer: "A", explanation: "Evidence retained" },
    });
    assert.match(html, /Choose one/); assert.match(html, /Answer A/); assert.match(html, /Evidence retained/);
    assert.match(html, /disabled/);
    for (const transition of transitions) {
      assert.equal((transition.transition as Props).duration, reduced ? 0 : 0.15);
      if (reduced) assert.equal(transition.initial, false);
    }
  });
}

test("production source imports only the successor motion entry", () => {
  let imports = 0;
  function scan(directory: URL) {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = new URL(entry.name + (entry.isDirectory() ? "/" : ""), directory);
      if (entry.isDirectory()) scan(path);
      else if (/\.tsx?$/.test(entry.name) && !entry.name.endsWith(".test.ts")) {
        const source = readFileSync(path, "utf8");
        assert.doesNotMatch(source, /from\s+["']framer-motion(?:\/[^"']*)?["']/);
        imports += (source.match(/from\s+["']motion\/react["']/g) ?? []).length;
      }
    }
  }
  scan(new URL("../", import.meta.url));
  assert.ok(imports >= 20, `expected the existing animation consumers, got ${imports}`);
  const manifest = JSON.parse(readFileSync(new URL("../../package.json", import.meta.url), "utf8"));
  assert.equal(manifest.dependencies["framer-motion"], undefined);
  assert.equal(manifest.dependencies.motion, "^12.40.0");
});

test("active class-merging dependency preserves real button and dialog overrides", () => {
  const utilities = load("../lib/utils.ts", {});
  const cn = utilities.cn as unknown as (...values: unknown[]) => string;
  const button = load("./ui/button.tsx", { "@/lib/utils": utilities, "./composition": {} });
  const variants = button.buttonVariants as unknown as (options: Props) => string;
  // These are existing defaults and caller overrides, not a replacement merge implementation.
  const buttonClasses = variants({ size: "default", className: "h-8 px-0" }).split(" ");
  assert.ok(buttonClasses.includes("h-8")); assert.ok(buttonClasses.includes("px-0"));
  assert.ok(!buttonClasses.includes("h-9")); assert.ok(!buttonClasses.includes("px-4"));
  const dialogClasses = cn("max-w-lg p-5 sm:p-6", "max-w-2xl p-0 sm:p-0").split(" ");
  assert.deepEqual(dialogClasses, ["max-w-2xl", "p-0", "sm:p-0"]);
});

test("installed Motion successor exports the used React API and renders an accessible element", async () => {
  const { motion, AnimatePresence, useReducedMotion } = await import("motion/react");
  assert.equal(typeof AnimatePresence, "function"); assert.equal(typeof useReducedMotion, "function");
  const html = renderToStaticMarkup(React.createElement(motion.button, {
    initial: false, animate: { opacity: 1 }, type: "button", "aria-label": "Continue", disabled: true,
  }, "Continue"));
  assert.match(html, /<button/); assert.match(html, /aria-label="Continue"/); assert.match(html, /disabled/);
  assert.match(html, /opacity:1/); assert.match(html, />Continue<\/button>/);
});
