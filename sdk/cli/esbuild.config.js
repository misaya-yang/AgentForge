import { build, context } from "esbuild";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const isWatch = process.argv.includes("--watch");

// Plugin to stub out optional peer deps that Ink imports but doesn't need at runtime
const stubPlugin = {
  name: "stub-optional",
  setup(build) {
    const stubs = ["react-devtools-core", "yoga-wasm-web"];
    for (const mod of stubs) {
      build.onResolve({ filter: new RegExp(`^${mod}$`) }, () => ({
        path: mod,
        namespace: "stub",
      }));
    }
    build.onLoad({ filter: /.*/, namespace: "stub" }, () => ({
      contents: "export default undefined;",
      loader: "js",
    }));
  },
};

const options = {
  entryPoints: ["src/cli.tsx"],
  bundle: true,
  platform: "node",
  target: "node22",
  format: "esm",
  outfile: "dist/cli.js",
  sourcemap: false,
  minify: !isWatch,
  banner: {
    js: '#!/usr/bin/env node\nimport { createRequire } from "module"; const require = createRequire(import.meta.url);',
  },
  plugins: [stubPlugin],
  external: [
    // Node builtins only
    "node:*",
    "child_process",
    "fs",
    "path",
    "os",
    "readline",
    "stream",
    "util",
    "crypto",
    "events",
    "net",
    "tls",
    "http",
    "https",
    "url",
    "zlib",
    "buffer",
    "string_decoder",
    "assert",
    "process",
  ],
  define: {
    "process.env.NODE_ENV": isWatch ? '"development"' : '"production"',
  },
};

if (isWatch) {
  const ctx = await context(options);
  await ctx.watch();
  console.log("Watching for changes...");
} else {
  await build(options);
  writeNativeSourceIdentity();
  console.log("Built dist/cli.js");
}

function writeNativeSourceIdentity() {
  const cliRoot = dirname(fileURLToPath(import.meta.url));
  const repositoryRoot = resolve(cliRoot, "../..");
  const sourceReceipt = readJson(resolve(repositoryRoot, "deploy/agent-runtime-source/source-receipt.json"));
  const overlayManifest = readJson(resolve(repositoryRoot, "rust/agent-runtime-overlay/manifest.json"));
  const sourceLock = readJson(resolve(repositoryRoot, "deploy/agent-runtime-source/lock.json"));
  const upstreamSha = sourceReceipt.source?.upstream_sha;
  const overlaySha = overlayManifest.sha256;
  if (sourceReceipt.overlay?.sha256 !== overlaySha || sourceLock.build?.overlay_sha256 !== overlaySha) {
    throw new Error("Agent Runtime source receipt, overlay manifest, and lock disagree");
  }
  if (overlayManifest.upstream_sha !== upstreamSha || sourceLock.source?.upstream_sha !== upstreamSha) {
    throw new Error("Agent Runtime upstream identity disagrees across source records");
  }
  writeFileSync(resolve(cliRoot, "dist/native-source.json"), `${JSON.stringify({
    schema_version: "ai-gateway-cli/native-source/v1",
    upstream_sha: upstreamSha,
    overlay_sha256: overlaySha,
  }, null, 2)}\n`);
}

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}
