#!/usr/bin/env node
// Prepare a minimal local image context. Never invokes Docker, package installs,
// a frontend build, application services, private env loading or network access.
import fs from "node:fs/promises";
import { constants } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { brotliDecompressSync, gunzipSync, zstdDecompressSync } from "node:zlib";

const REPO = fileURLToPath(new URL("../..", import.meta.url));
const BASE_IMAGE = "nginxinc/nginx-unprivileged:1.30-alpine";
const MAX_BYTES = 128 * 1024 * 1024;
const COMPRESSIBLE = /\.(?:js|css|json|svg|wasm)$/;
const DECODERS = { ".br": brotliDecompressSync, ".gz": gunzipSync, ".zst": zstdDecompressSync };
const hash = bytes => createHash("sha256").update(bytes).digest("hex");
const json = value => Buffer.from(JSON.stringify(value, null, 2) + "\n");
const within = (parent, child) => child === parent || child.startsWith(parent + path.sep);
const metadata = files => [...files].sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0).map(([name, bytes]) => ({ path: name, bytes: bytes.length, sha256: hash(bytes) }));
const identity = files => hash(json(metadata(files)));
function fail(message) { throw new Error(message); }
function safeName(name) {
  if (typeof name !== "string" || !name || path.posix.isAbsolute(name) || name.includes("\\") || /[\x00-\x20]/.test(name)
    || name.split("/").some(part => !part || part === "." || part === "..")) fail("Invalid artifact path");
  return name;
}
async function readTree(root) {
  if (!(await fs.lstat(root)).isDirectory()) fail("Artifact root must be a regular directory");
  const files = new Map(); let total = 0;
  async function walk(relative = "") {
    for (const item of await fs.readdir(path.join(root, relative), { withFileTypes: true })) {
      const name = relative ? `${relative}/${item.name}` : item.name;
      safeName(name);
      if (item.isSymbolicLink() || (!item.isFile() && !item.isDirectory())) fail(`Non-regular artifact input: ${name}`);
      if (item.isDirectory()) { await walk(name); continue; }
      const handle = await fs.open(path.join(root, name), constants.O_RDONLY | constants.O_NOFOLLOW);
      try {
        const stat = await handle.stat();
        if (!stat.isFile()) fail(`Non-regular artifact input: ${name}`);
        if (total + stat.size > MAX_BYTES) fail("Artifact exceeds the local packaging size limit");
        const bytes = await handle.readFile(); total += bytes.length;
        if (total > MAX_BYTES) fail("Artifact exceeds the local packaging size limit");
        files.set(name, bytes);
      } finally { await handle.close(); }
    }
  }
  await walk(); return files;
}
function requireFile(files, name) {
  safeName(name); const bytes = files.get(name);
  if (!bytes) fail(`Missing artifact file: ${name}`);
  return bytes;
}
function parseObject(bytes, name) {
  let value; try { value = JSON.parse(bytes); } catch { fail(`Invalid JSON: ${name}`); }
  if (!value || typeof value !== "object" || Array.isArray(value)) fail(`Expected JSON object: ${name}`);
  return value;
}
export function validateDist(files, publicFiles) {
  const manifest = parseObject(requireFile(files, ".vite/manifest.json"), "Vite manifest");
  if (!manifest["index.html"]?.isEntry) fail("Vite manifest is missing the HTML entry");
  const references = new Set();
  for (const entry of Object.values(manifest)) {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) fail("Invalid Vite manifest entry");
    for (const key of ["imports", "dynamicImports"]) {
      if (entry[key] !== undefined && !Array.isArray(entry[key])) fail("Invalid Vite import inventory");
      for (const reference of entry[key] ?? []) if (typeof reference !== "string" || !Object.hasOwn(manifest, reference)) fail("Unresolved Vite manifest import");
    }
    for (const key of ["css", "assets"]) if (entry[key] !== undefined && !Array.isArray(entry[key])) fail("Invalid Vite asset inventory");
    for (const name of [entry.file, ...(entry.css ?? []), ...(entry.assets ?? [])]) {
      safeName(name);
      if (!name.startsWith("assets/") || !/\.(?:js|css|json|svg|wasm|woff2?|ttf|otf|png|jpe?g|gif|webp|avif|ico)$/.test(name)) fail("Vite asset is outside the static output allowlist");
      requireFile(files, name); references.add(name);
    }
  }
  const html = requireFile(files, "index.html").toString("utf8");
  if (!html.includes(`src="/${manifest["index.html"].file}"`)) fail("HTML entry does not match the Vite manifest");
  let representations = 0;
  for (const [name, bytes] of files) {
    if (/\.map(?:\.(?:br|gz|zst))?$/.test(name) || /(?:^|\/)(?:\.env(?:\..*)?|node_modules|src|\.git)(?:\/|$)/.test(name)) fail(`Forbidden artifact input: ${name}`);
    const suffix = Object.keys(DECODERS).find(extension => name.endsWith(extension));
    if (suffix) {
      const original = name.slice(0, -suffix.length);
      if ((!references.has(original) || !COMPRESSIBLE.test(original)) && original !== ".vite/manifest.json") fail(`Unexpected compressed artifact: ${name}`);
      let decoded; try { decoded = DECODERS[suffix](bytes, { maxOutputLength: MAX_BYTES }); } catch { fail(`Invalid compressed artifact: ${name}`); }
      if (!decoded.equals(requireFile(files, original))) fail(`Compressed bytes differ from final asset: ${name}`);
      representations++; continue;
    }
    if (name.endsWith(".js") && /\/\/[#@]\s*sourceMappingURL\s*=/.test(bytes.toString("utf8"))) fail(`Source-map reference in artifact: ${name}`);
    if (references.has(name)) {
      if (COMPRESSIBLE.test(name)) for (const extension of Object.keys(DECODERS)) requireFile(files, name + extension);
    } else if (name !== "index.html" && name !== ".vite/manifest.json") {
      if (!publicFiles.has(name) || !bytes.equals(publicFiles.get(name))) fail(`Unrecognized or changed public artifact: ${name}`);
      if (name === "runtime-config.json" && Object.keys(parseObject(bytes, name)).length !== 0) fail("Build artifact must not embed deployment runtime configuration");
    }
  }
  for (const name of publicFiles.keys()) requireFile(files, name);
  return { manifestEntries: Object.keys(manifest).length, staticAssets: references.size, representations, sourceMaps: 0 };
}
async function sourceSnapshot(repo) {
  const files = new Map();
  for (const dir of ["web/src", "web/public", "web/build", "web/patches"]) {
    let tree; try { tree = await readTree(path.join(repo, dir)); } catch (error) { if (error.code === "ENOENT" && dir === "web/patches") continue; throw error; }
    for (const [name, bytes] of tree) files.set(`${dir}/${name}`, bytes);
  }
  for (const name of ["web/index.html", "web/vite.config.ts", "web/package.json", "web/pnpm-lock.yaml", "web/pnpm-workspace.yaml", "sdk/typescript/generated.ts"]) files.set(name, await fs.readFile(path.join(repo, name)));
  return files;
}
export async function inspectArtifact(dist, { repo = REPO, baseImage = BASE_IMAGE } = {}) {
  if (typeof zstdDecompressSync !== "function") fail("Use the pinned Node 26 runtime for artifact verification");
  if (!/^[A-Za-z0-9][A-Za-z0-9._/@:+-]*$/.test(baseImage)) fail("Invalid nginx base image reference");
  const resolvedDist = await fs.realpath(dist), resolvedRepo = await fs.realpath(repo);
  if (within(resolvedRepo, resolvedDist)) fail("Use an external verified dist directory");
  const files = await readTree(resolvedDist), publicFiles = await readTree(path.join(repo, "web/public"));
  const validation = validateDist(files, publicFiles);
  const sourceFiles = await sourceSnapshot(repo), runtimeFiles = new Map();
  for (const name of ["nginx.conf", "nginx-precompressed-assets.conf", "docker-entrypoint.d/40-runtime-config.sh", "docker-entrypoint.d/45-security-headers.sh"]) runtimeFiles.set(name, await fs.readFile(path.join(repo, "web", name)));
  const dockerfile = await fs.readFile(path.join(repo, "web/Dockerfile.artifact"));
  const version = JSON.parse(sourceFiles.get("web/package.json")).version;
  let revision = "unknown";
  try { revision = execFileSync("git", ["-C", repo, "rev-parse", "HEAD"], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim(); } catch { /* Checkouts without Git still have the byte snapshot. */ }
  const summary = { schema: "agentforge/frontend-artifact/v1", artifactSha256: identity(files), checkoutSha256: identity(sourceFiles), runtimeSha256: identity(runtimeFiles), dockerfileSha256: hash(dockerfile), sdkSha256: hash(sourceFiles.get("sdk/typescript/generated.ts")), revision, version, baseImage, validation,
    provenance: "artifact bytes and checkout snapshot are measured independently; no historical rebuild equivalence is asserted" };
  return { summary, files, sourceFiles, runtimeFiles, dockerfile };
}
function buildArgv(summary, image, context) {
  return ["docker", "build", "--pull=false", "--build-arg", `NGINX_BASE_IMAGE=${summary.baseImage}`, "--build-arg", `APP_VERSION=${summary.version}`, "--build-arg", `VCS_REF=${summary.revision}`, "--build-arg", `FRONTEND_ARTIFACT_SHA256=${summary.artifactSha256}`, "--build-arg", `FRONTEND_SOURCE_SHA256=${summary.checkoutSha256}`, "--tag", image, context];
}
export async function packageArtifact(dist, out, expected, options = {}) {
  const inspected = await inspectArtifact(dist, options), { summary } = inspected;
  if (!/^[a-f0-9]{64}$/.test(expected?.artifact ?? "") || !/^[a-f0-9]{64}$/.test(expected?.source ?? "")) fail("Both expected artifact and checkout SHA256 are required");
  if (summary.artifactSha256 !== expected.artifact || summary.checkoutSha256 !== expected.source) fail("Verified artifact or checkout identity changed");
  const parent = await fs.realpath(path.dirname(path.resolve(out)));
  const repo = await fs.realpath(options.repo ?? REPO);
  const target = path.join(parent, path.basename(out));
  if (within(repo, target)) fail("Packaging output must be outside the checkout");
  if (within(await fs.realpath(dist), target)) fail("Packaging output must not be inside the dist input");
  // Refuse replacement, including symlink destinations. No source tree is a context.
  await fs.mkdir(target);
  const contextFiles = new Map(inspected.runtimeFiles);
  contextFiles.set("Dockerfile", inspected.dockerfile);
  for (const [name, bytes] of inspected.files) if (!name.startsWith(".vite/")) contextFiles.set(`dist/${name}`, bytes);
  contextFiles.set("artifact-identity.json", json(summary));
  contextFiles.set(".dockerignore", Buffer.from("**\n!Dockerfile\n!nginx.conf\n!nginx-precompressed-assets.conf\n!artifact-identity.json\n!docker-entrypoint.d/\n!docker-entrypoint.d/**\n!dist/\n!dist/**\n"));
  const contextSha256 = identity(contextFiles), image = `agentforge-web-dev:${contextSha256.slice(0, 24)}`;
  for (const [name, bytes] of contextFiles) {
    const file = path.join(target, "context", name); await fs.mkdir(path.dirname(file), { recursive: true });
    await fs.writeFile(file, bytes, { flag: "wx", mode: name.endsWith(".sh") ? 0o755 : 0o644 });
  }
  const argv = buildArgv(summary, image, path.join(target, "context"));
  const receipt = { ...summary, contextSha256, image, dockerBuildArgv: argv, contextFiles: metadata(contextFiles), imageId: null, note: "Image tag identifies this packaging context; resolve the actual image ID after the separately authorized Docker build." };
  await fs.writeFile(path.join(target, "artifact-receipt.json"), json(receipt), { flag: "wx" });
  await fs.writeFile(path.join(target, "source-manifest.json"), json(metadata(inspected.sourceFiles)), { flag: "wx" });
  await fs.writeFile(path.join(target, "image.env"), `FRONTEND_IMAGE=${image}\n`, { flag: "wx" });
  return receipt;
}
export async function verifyPackage(out) {
  const receipt = parseObject(await fs.readFile(path.join(out, "artifact-receipt.json")), "packaging receipt");
  const context = await readTree(path.join(out, "context"));
  if (identity(context) !== receipt.contextSha256 || JSON.stringify(metadata(context)) !== JSON.stringify(receipt.contextFiles)
    || receipt.image !== `agentforge-web-dev:${receipt.contextSha256.slice(0, 24)}`) fail("Packaging context no longer matches its receipt/image tag");
  const summary = parseObject(requireFile(context, "artifact-identity.json"), "artifact identity");
  if (Object.entries(summary).some(([key, value]) => JSON.stringify(value) !== JSON.stringify(receipt[key]))) fail("Packaging identity mismatch");
  if (hash(await fs.readFile(path.join(out, "source-manifest.json"))) !== summary.checkoutSha256) fail("Checkout source inventory no longer matches its identity");
  if (JSON.stringify(receipt.dockerBuildArgv) !== JSON.stringify(buildArgv(summary, receipt.image, path.join(await fs.realpath(out), "context")))) fail("Docker build arguments no longer match the verified image/context");
  if (await fs.readFile(path.join(out, "image.env"), "utf8") !== `FRONTEND_IMAGE=${receipt.image}\n`) fail("FRONTEND_IMAGE no longer matches the verified image/context");
  return { image: receipt.image, contextSha256: receipt.contextSha256, files: context.size };
}
async function main(argv) {
  const args = new Map();
  for (let index = 0; index < argv.length; index += 2) {
    const key = argv[index];
    if (!["--dist", "--out", "--expected-dist-sha256", "--expected-source-sha256", "--base-image", "--verify-context"].includes(key) || !argv[index + 1] || args.has(key)) fail("Usage: package-frontend-artifact.mjs --dist /external/dist [--out /external/new-package --expected-dist-sha256 SHA --expected-source-sha256 SHA] [--base-image IMAGE], or --verify-context /external/package");
    args.set(key, argv[index + 1]);
  }
  if (args.has("--verify-context")) {
    if (args.size !== 1) fail("Context verification accepts no packaging arguments");
    console.log(JSON.stringify(await verifyPackage(args.get("--verify-context")), null, 2)); return;
  }
  if (!args.has("--dist")) fail("An external verified dist input is required");
  const options = { baseImage: args.get("--base-image") ?? BASE_IMAGE };
  if (!args.has("--out")) {
    if (args.has("--expected-dist-sha256") || args.has("--expected-source-sha256")) fail("Expected hashes require a packaging output");
    console.log(JSON.stringify((await inspectArtifact(args.get("--dist"), options)).summary, null, 2)); return;
  }
  const receipt = await packageArtifact(args.get("--dist"), args.get("--out"), { artifact: args.get("--expected-dist-sha256"), source: args.get("--expected-source-sha256") }, options);
  console.log(JSON.stringify({ image: receipt.image, artifactSha256: receipt.artifactSha256, checkoutSha256: receipt.checkoutSha256, contextSha256: receipt.contextSha256, dockerBuildArgv: receipt.dockerBuildArgv }, null, 2));
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main(process.argv.slice(2)).catch(error => { console.error(`Frontend artifact packaging failed: ${error.message}`); process.exitCode = 1; });
