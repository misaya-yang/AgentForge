import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import i18next from "i18next";
import ts from "typescript";

const readLocale = (name: string) => JSON.parse(readFileSync(new URL(`./locales/${name}.json`, import.meta.url), "utf8"));

// Run the production loader and queue against a fresh real i18next instance.
// Only chunk delivery/storage and the React binding plugin are doubled; no browser is claimed.
function fixture(language = "en-US") {
  const instance = i18next.createInstance();
  const imports: string[] = [];
  const failures = new Set<string>();
  const gates = new Map<string, Promise<void>>();
  const storage = new Map([["i18nextLng", language]]);
  const document = { documentElement: { lang: "" } };
  const queueExports: Record<string, unknown> = {};
  const compile = (name: string) => ts.transpileModule(
    readFileSync(new URL(name, import.meta.url), "utf8").replace(/\bimport\(/g, "importModule("),
    { compilerOptions: { module: ts.ModuleKind.CommonJS } },
  ).outputText;
  vm.runInNewContext(compile("./localeOperationQueue.ts"), { exports: queueExports });
  const exports: Record<string, unknown> = {};
  vm.runInNewContext(compile("./index.ts"), {
    exports,
    require: (name: string) => {
      if (name === "i18next") return { __esModule: true, default: instance };
      if (name === "react-i18next") return { initReactI18next: { type: "3rdParty", init() {} } };
      if (name === "./localeOperationQueue") return queueExports;
      throw new Error(`Unexpected dependency: ${name}`);
    },
    async importModule(path: string) {
      imports.push(path);
      await gates.get(path);
      if (failures.has(path)) throw new Error(`Chunk unavailable: ${path}`);
      return { default: JSON.parse(readFileSync(new URL(path, import.meta.url), "utf8")) };
    },
    window: {}, document,
    localStorage: {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => storage.set(key, value),
    },
  });
  return {
    instance, imports, failures, gates, storage, document,
    initialize: exports.initializeI18n as () => Promise<unknown>,
    load: exports.loadTranslationNamespace as (namespace: string) => Promise<void>,
    loadMany: exports.loadTranslationNamespaces as (...namespaces: string[]) => Promise<void>,
    change: exports.changeAppLanguage as (language: string) => Promise<void>,
  };
}

function expectedKnowledge(language: string) {
  return { ...readLocale(language).knowledge, ...readLocale(`knowledge-${language}`) };
}

test("public bootstrap loads only the selected main locale and retains Knowledge shell translations", async () => {
  for (const language of ["en-US", "zh-CN"]) {
    const f = fixture(language);
    await f.initialize();
    assert.deepEqual(f.imports, [`./locales/${language}.json`]);
    assert.deepEqual(f.instance.getResourceBundle(language, "translation"), readLocale(language));
    assert.equal(f.instance.t("knowledge.datasets.title"), readLocale(language).knowledge.datasets.title);
    assert.equal(f.instance.exists("knowledge.detail.failureReviewLoading"), false);
    assert.equal(f.instance.language, language);
    assert.equal(f.document.documentElement.lang, language);
  }
});

test("Knowledge chunks deep-merge once without replacing eager keys, in both locales", async () => {
  for (const language of ["en-US", "zh-CN"]) {
    const f = fixture(language);
    await Promise.all([f.load("knowledge"), f.load("knowledge")]);
    assert.equal(f.imports.filter(path => path === `./locales/knowledge-${language}.json`).length, 1);
    assert.deepEqual(f.instance.getResourceBundle(language, "translation").knowledge, expectedKnowledge(language));
    assert.equal(f.instance.t("knowledge.detail.failureReviewLoading"), expectedKnowledge(language).detail.failureReviewLoading);
    assert.equal(f.instance.t("knowledge.datasets.title"), expectedKnowledge(language).datasets.title);
  }
});

test("an in-flight Knowledge import and language change remain serialized with complete target resources", async () => {
  const f = fixture();
  let release!: () => void;
  f.gates.set("./locales/knowledge-en-US.json", new Promise<void>(resolve => { release = resolve; }));
  const loading = f.load("knowledge");
  const changing = f.change("zh-CN");
  await new Promise<void>(resolve => setImmediate(resolve));
  assert.equal(f.instance.language, "en-US");
  assert.equal(f.imports.includes("./locales/zh-CN.json"), false);
  release();
  await Promise.all([loading, changing]);
  assert.equal(f.instance.language, "zh-CN");
  assert.deepEqual(f.instance.getResourceBundle("zh-CN", "translation").knowledge, expectedKnowledge("zh-CN"));
  assert.equal(f.storage.get("i18nextLng"), "zh-CN");
  await f.change("en-US");
  assert.deepEqual(f.instance.getResourceBundle("en-US", "translation").knowledge, expectedKnowledge("en-US"));
  assert.equal(f.imports.filter(path => path.includes("knowledge-")).length, 2);
});

test("a language change queued before Knowledge loading selects only the new locale chunk", async () => {
  const f = fixture();
  await f.initialize();
  await Promise.all([f.change("zh-CN"), f.load("knowledge")]);
  assert.equal(f.imports.includes("./locales/knowledge-en-US.json"), false);
  assert.equal(f.imports.includes("./locales/knowledge-zh-CN.json"), true);
  assert.deepEqual(f.instance.getResourceBundle("zh-CN", "translation").knowledge, expectedKnowledge("zh-CN"));
});

test("failed Knowledge loading remains retryable and does not activate a broken namespace", async () => {
  const f = fixture();
  f.failures.add("./locales/knowledge-en-US.json");
  await assert.rejects(f.load("knowledge"), /Chunk unavailable/);
  await f.change("zh-CN");
  assert.equal(f.imports.includes("./locales/knowledge-zh-CN.json"), false);
  await f.load("knowledge");
  assert.equal(f.instance.t("knowledge.datasets.title"), expectedKnowledge("zh-CN").datasets.title);
  f.failures.clear();
  await f.change("en-US");
  assert.deepEqual(f.instance.getResourceBundle("en-US", "translation").knowledge, expectedKnowledge("en-US"));
});

test("a failed active-namespace language switch keeps the old language and can retry", async () => {
  const f = fixture();
  await f.load("knowledge");
  f.failures.add("./locales/knowledge-zh-CN.json");
  await assert.rejects(f.change("zh-CN"), /Chunk unavailable/);
  assert.equal(f.instance.language, "en-US");
  assert.equal(f.storage.get("i18nextLng"), "en-US");
  f.failures.clear();
  await f.change("zh-CN");
  assert.equal(f.instance.language, "zh-CN");
  assert.deepEqual(f.instance.getResourceBundle("zh-CN", "translation").knowledge, expectedKnowledge("zh-CN"));
});

test("Knowledge references outside its deferred routes resolve from the eager locale", () => {
  const root = new URL("../", import.meta.url);
  const eager = readLocale("en-US");
  const files = readdirSync(root, { recursive: true, withFileTypes: true });
  let checked = 0;
  for (const file of files) {
    if (!file.isFile() || !/\.tsx?$/.test(file.name) || /\.(test|spec)\./.test(file.name)) continue;
    const path = `${file.parentPath}/${file.name}`;
    if (/\/pages\/(knowledge|eval)\/|\/i18n\//.test(path) && !path.endsWith("/pages/knowledge/Datasets.tsx")) continue;
    const source = ts.createSourceFile(path, readFileSync(path, "utf8"), ts.ScriptTarget.Latest, true);
    const visit = (node: ts.Node) => {
      if ((ts.isStringLiteralLike(node) || ts.isTemplateHead(node)) && node.text.startsWith("knowledge.")) {
        const key = node.text.replace(/\.$/, "");
        const value = key.split(".").reduce((current, part) => current?.[part], eager);
        assert.notEqual(value, undefined, `${path}: deferred key ${key} needs a route namespace boundary`);
        checked++;
      }
      ts.forEachChild(node, visit);
    };
    visit(source);
  }
  assert.ok(checked >= 2, "AppLayout's eager title and subtitle must be inspected");
});


test("multiple route namespaces start together inside one serialized locale operation", async () => {
  const f = fixture();
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  f.gates.set("./locales/knowledge-en-US.json", gate);
  f.gates.set("./locales/eval-en-US.json", gate);
  const loading = f.loadMany("knowledge", "eval", "knowledge");
  await new Promise<void>(resolve => setImmediate(resolve));
  assert.deepEqual(f.imports, ["./locales/en-US.json", "./locales/knowledge-en-US.json", "./locales/eval-en-US.json"]);
  const changing = f.change("zh-CN");
  await new Promise<void>(resolve => setImmediate(resolve));
  assert.equal(f.instance.language, "en-US");
  release();
  await Promise.all([loading, changing]);
  assert.equal(f.instance.language, "zh-CN");
  assert.deepEqual(f.instance.getResourceBundle("zh-CN", "translation").knowledge, expectedKnowledge("zh-CN"));
  assert.deepEqual(f.instance.getResourceBundle("zh-CN", "translation").eval, readLocale("eval-zh-CN"));
});

test("a partially failed namespace batch does not become active and retries the missing chunk", async () => {
  const f = fixture();
  f.failures.add("./locales/eval-en-US.json");
  await assert.rejects(f.loadMany("knowledge", "eval"), /Chunk unavailable/);
  assert.deepEqual(f.instance.getResourceBundle("en-US", "translation").knowledge, expectedKnowledge("en-US"));
  await f.change("zh-CN");
  assert.equal(f.imports.some(path => /(?:knowledge|eval)-zh-CN/.test(path)), false);
  f.failures.clear();
  await f.change("en-US");
  await f.loadMany("knowledge", "eval");
  assert.equal(f.imports.filter(path => path === "./locales/knowledge-en-US.json").length, 1);
  assert.equal(f.imports.filter(path => path === "./locales/eval-en-US.json").length, 2);
  await f.change("zh-CN");
  assert.deepEqual(f.instance.getResourceBundle("zh-CN", "translation").knowledge, expectedKnowledge("zh-CN"));
  assert.deepEqual(f.instance.getResourceBundle("zh-CN", "translation").eval, readLocale("eval-zh-CN"));
});


test("a rejected batch cannot activate a late sibling after a language switch or lose existing namespaces", async () => {
  const f = fixture();
  await f.load("agents");
  let release!: () => void;
  f.gates.set("./locales/knowledge-en-US.json", new Promise<void>(resolve => { release = resolve; }));
  f.failures.add("./locales/eval-en-US.json");
  await assert.rejects(f.loadMany("knowledge", "eval"), /Chunk unavailable/);
  await f.change("zh-CN");
  assert.deepEqual(f.instance.getResourceBundle("zh-CN", "translation").agents, readLocale("agents-zh-CN"));
  release();
  await new Promise<void>(resolve => setImmediate(resolve));
  assert.equal(f.instance.language, "zh-CN");
  assert.equal(f.instance.getResource("zh-CN", "translation", "knowledge.detail.failureReviewLoading"), undefined);
  assert.equal(f.imports.some(path => /(?:knowledge|eval)-zh-CN/.test(path)), false);
  await f.change("en-US");
  assert.equal(f.imports.filter(path => path === "./locales/eval-en-US.json").length, 1);
  assert.deepEqual(f.instance.getResourceBundle("en-US", "translation").knowledge, expectedKnowledge("en-US"));
  f.failures.clear();
  await f.loadMany("knowledge", "eval");
  await f.change("zh-CN");
  assert.deepEqual(f.instance.getResourceBundle("zh-CN", "translation").knowledge, expectedKnowledge("zh-CN"));
  assert.deepEqual(f.instance.getResourceBundle("zh-CN", "translation").eval, readLocale("eval-zh-CN"));
});
