import { readFile } from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { execSync } from "node:child_process";

function flatten(obj, prefix = "", out = new Set()) {
  if (obj && typeof obj === "object" && !Array.isArray(obj)) {
    for (const [key, value] of Object.entries(obj)) {
      const next = prefix ? `${prefix}.${key}` : key;
      flatten(value, next, out);
    }
    return out;
  }
  if (prefix) out.add(prefix);
  return out;
}

function difference(source, target) {
  const missing = [];
  for (const key of source) {
    if (!target.has(key)) missing.push(key);
  }
  return missing.sort();
}

async function readJson(filePath) {
  const raw = await readFile(filePath, "utf8");
  return JSON.parse(raw);
}

/**
 * Collect every static dotted key passed to t()/i18n.t() in src/.
 * Skips test files and template-literal/dynamic keys (those are checked
 * by hand at review time).
 */
async function collectCodeKeys() {
  const files = execSync(
    `find src -name "*.ts" -o -name "*.tsx" | grep -v -E "\\.test\\.|\\.spec\\."`,
    { encoding: "utf8" },
  ).trim().split("\n");
  const staticKey = /\b(?:i18n\.)?t\(\s*["']([a-zA-Z0-9_][\w]*(\.[a-zA-Z0-9_][\w]*)+)["']/g;
  const keys = new Set();
  for (const file of files) {
    const src = await readFile(file, "utf8");
    for (const match of src.matchAll(staticKey)) keys.add(match[1]);
  }
  return keys;
}

async function main() {
  const baseDir = path.resolve("src/i18n/locales");
  const [en, zh, evalEn, evalZh, agentsEn, agentsZh, knowledgeEn, knowledgeZh, tasksEn, tasksZh] = await Promise.all([
    readJson(path.join(baseDir, "en-US.json")),
    readJson(path.join(baseDir, "zh-CN.json")),
    readJson(path.join(baseDir, "eval-en-US.json")),
    readJson(path.join(baseDir, "eval-zh-CN.json")),
    readJson(path.join(baseDir, "agents-en-US.json")),
    readJson(path.join(baseDir, "agents-zh-CN.json")),
    readJson(path.join(baseDir, "knowledge-en-US.json")),
    readJson(path.join(baseDir, "knowledge-zh-CN.json")),
    readJson(path.join(baseDir, "tasks-en-US.json")),
    readJson(path.join(baseDir, "tasks-zh-CN.json")),
  ]);

  const enKeys = flatten({ ...en, tasks: tasksEn, eval: evalEn, agents: agentsEn, knowledge: { ...en.knowledge, ...knowledgeEn } });
  const zhKeys = flatten({ ...zh, tasks: tasksZh, eval: evalZh, agents: agentsZh, knowledge: { ...zh.knowledge, ...knowledgeZh } });

  const missingInZh = difference(enKeys, zhKeys);
  const missingInEn = difference(zhKeys, enKeys);

  let failed = false;

  if (missingInZh.length > 0) {
    failed = true;
    console.error("\nMissing in zh-CN:");
    for (const key of missingInZh) console.error(`  - ${key}`);
  }

  if (missingInEn.length > 0) {
    failed = true;
    console.error("\nMissing in en-US:");
    for (const key of missingInEn) console.error(`  - ${key}`);
  }

  // Code-reference gate: every static t() key must exist in the bundles,
  // even when the call site passes a defaultValue — the default is a fallback,
  // not an excuse to skip locale entries (this is how untranslated strings
  // have historically slipped in).
  const codeKeys = await collectCodeKeys();
  const missingInLocale = difference(codeKeys, enKeys);
  if (missingInLocale.length > 0) {
    failed = true;
    console.error("\nCode references keys missing from locale files:");
    for (const key of missingInLocale) console.error(`  - ${key}`);
  }

  if (!failed) {
    console.log("i18n check passed: en-US and zh-CN keys are in sync and all code keys resolve.");
    return;
  }
  process.exitCode = 1;
}

main().catch((error) => {
  console.error("Failed to check i18n keys:", error);
  process.exit(1);
});
