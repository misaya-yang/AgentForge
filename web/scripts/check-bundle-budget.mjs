import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { gzipSync } from "node:zlib";

const DIST_DIR = path.resolve("dist");
const MANIFEST_PATH = path.join(DIST_DIR, ".vite", "manifest.json");
const PUBLIC_GZIP_LIMIT = 330_000;
const ASSISTANT_INCREMENT_GZIP_LIMIT = 220_000;
const ASSISTANT_RESTORED_GZIP_LIMIT = 220_000;
const ASSISTANT_GFM_GZIP_LIMIT = 220_000;
const ROUTE_RAW_LIMIT = 500_000;
const ROUTE_SOURCES = new Set([
  "src/pages/dashboard/index.tsx",
  "src/pages/Services.tsx",
  "src/pages/playground/index.tsx",
  "src/pages/tasks/index.tsx",
  "src/pages/knowledge/index.ts",
  "src/pages/assistant/index.tsx",
  "src/pages/eval/index.tsx",
  "src/pages/agents/index.ts",
]);
const EXISTING_ROUTE_EXCEPTIONS = new Set([
  "src/pages/dashboard/index.tsx",
  "src/pages/agents/index.ts",
  "src/pages/eval/index.tsx",
  "src/pages/knowledge/index.ts",
]);

if (!fs.existsSync(MANIFEST_PATH)) {
  throw new Error("Vite manifest missing; run `pnpm -C web build` first");
}

const manifest = JSON.parse(fs.readFileSync(MANIFEST_PATH, "utf8"));

function entryForSource(source) {
  const direct = manifest[source];
  if (direct) return direct;
  return Object.values(manifest).find((entry) => entry.src === source);
}

function collectStaticClosure(entry) {
  const files = new Set();
  const visit = (candidate) => {
    if (!candidate || files.has(candidate.file)) return;
    files.add(candidate.file);
    for (const cssFile of candidate.css || []) files.add(cssFile);
    for (const importedKey of candidate.imports || []) visit(manifest[importedKey]);
  };
  visit(entry);
  return files;
}

function addSourceClosure(files, source) {
  const entry = entryForSource(source);
  if (!entry) throw new Error(`Manifest entry missing for ${source}`);
  for (const file of collectStaticClosure(entry)) files.add(file);
}

function addSourceSuffixClosure(files, suffix) {
  const entry = Object.values(manifest).find((candidate) => candidate.src?.endsWith(suffix));
  if (!entry) throw new Error(`Manifest entry missing for *${suffix}`);
  for (const file of collectStaticClosure(entry)) files.add(file);
}

function sizeOf(files) {
  let raw = 0;
  let gzip = 0;
  for (const file of files) {
    const data = fs.readFileSync(path.join(DIST_DIR, file));
    raw += data.byteLength;
    gzip += gzipSync(data, { level: 9 }).byteLength;
  }
  return { raw, gzip };
}

const publicEntry = manifest["index.html"];
if (!publicEntry) throw new Error("Manifest entry missing for index.html");
const publicFiles = collectStaticClosure(publicEntry);
// The selected main locale is awaited before React mounts. Count the larger
// language so the entry budget remains valid for both supported locales.
const localeClosures = [
  ["src/i18n/locales/en-US.json", "/antd/locale/en_US.js"],
  ["src/i18n/locales/zh-CN.json", "/antd/locale/zh_CN.js"],
].map(([source, antLocaleSuffix]) => {
    const files = new Set(publicFiles);
    addSourceClosure(files, source);
    addSourceSuffixClosure(files, antLocaleSuffix);
    return { files, size: sizeOf(files) };
  });
const publicBudget = localeClosures.reduce(
  (largest, candidate) => candidate.size.gzip > largest.size.gzip ? candidate : largest,
);
const assistantFiles = collectStaticClosure(entryForSource("src/pages/assistant/index.tsx"));
const assistantIncrementFiles = new Set(
  [...assistantFiles].filter((file) => !publicFiles.has(file)),
);
const assistantIncrement = sizeOf(assistantIncrementFiles);
const streamOutputEntry = Object.values(manifest).find(
  (entry) => entry.name === "StreamOutput",
);
if (!streamOutputEntry) throw new Error("Manifest entry missing for StreamOutput");
const restoredConversationFiles = new Set(assistantIncrementFiles);
for (const file of collectStaticClosure(streamOutputEntry)) {
  if (!publicFiles.has(file)) restoredConversationFiles.add(file);
}
const assistantRestoredConversation = sizeOf(restoredConversationFiles);
const gfmEntry = entryForSource("src/components/GfmMarkdownBlock.tsx");
if (!gfmEntry) throw new Error("Manifest entry missing for GfmMarkdownBlock");
const gfmConversationFiles = new Set(restoredConversationFiles);
for (const file of collectStaticClosure(gfmEntry)) {
  if (!publicFiles.has(file)) gfmConversationFiles.add(file);
}
const assistantGfmConversation = sizeOf(gfmConversationFiles);
const mathEntry = entryForSource("src/components/MathMarkdownBlock.tsx");
const optionalMathFiles = new Set(restoredConversationFiles);
if (mathEntry) {
  for (const file of collectStaticClosure(mathEntry)) {
    if (!publicFiles.has(file)) optionalMathFiles.add(file);
  }
}
const assistantMathConversation = sizeOf(optionalMathFiles);

const oversizedRoutes = Object.entries(manifest)
  .filter(([, entry]) => entry.isDynamicEntry && entry.file?.endsWith(".js"))
  .map(([source, entry]) => ({
    source: entry.src || source,
    file: entry.file,
    raw: sizeOf(
      new Set(
        [...collectStaticClosure(entry)].filter((file) => !publicFiles.has(file)),
      ),
    ).raw,
  }))
  .filter((entry) => ROUTE_SOURCES.has(entry.source))
  .filter((entry) => entry.raw > ROUTE_RAW_LIMIT && !EXISTING_ROUTE_EXCEPTIONS.has(entry.source));

const report = {
  publicEntry: publicBudget.size,
  assistantIncrement,
  assistantRestoredConversation,
  assistantGfmConversation,
  assistantMathConversation,
  limits: {
    publicGzip: PUBLIC_GZIP_LIMIT,
    assistantIncrementGzip: ASSISTANT_INCREMENT_GZIP_LIMIT,
    assistantRestoredGzip: ASSISTANT_RESTORED_GZIP_LIMIT,
    assistantGfmGzip: ASSISTANT_GFM_GZIP_LIMIT,
    routeRaw: ROUTE_RAW_LIMIT,
  },
  oversizedRoutes,
};
console.log(JSON.stringify(report, null, 2));

const failures = [];
if (publicBudget.size.gzip > PUBLIC_GZIP_LIMIT) {
  failures.push(`public entry gzip ${publicBudget.size.gzip} > ${PUBLIC_GZIP_LIMIT}`);
}
if (assistantIncrement.gzip > ASSISTANT_INCREMENT_GZIP_LIMIT) {
  failures.push(
    `Assistant increment gzip ${assistantIncrement.gzip} > ${ASSISTANT_INCREMENT_GZIP_LIMIT}`,
  );
}
if (assistantRestoredConversation.gzip > ASSISTANT_RESTORED_GZIP_LIMIT) {
  failures.push(
    `Assistant restored conversation gzip ${assistantRestoredConversation.gzip} > ${ASSISTANT_RESTORED_GZIP_LIMIT}`,
  );
}
if (assistantGfmConversation.gzip > ASSISTANT_GFM_GZIP_LIMIT) {
  failures.push(`Assistant GFM conversation gzip ${assistantGfmConversation.gzip} > ${ASSISTANT_GFM_GZIP_LIMIT}`);
}
if (oversizedRoutes.length > 0) {
  failures.push(`new route chunks exceed ${ROUTE_RAW_LIMIT} raw bytes`);
}
if (failures.length > 0) {
  console.error(failures.join("\n"));
  process.exitCode = 1;
}
