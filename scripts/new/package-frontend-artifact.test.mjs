import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import path from "node:path";
import os from "node:os";
import { fileURLToPath } from "node:url";
import { brotliCompressSync, gzipSync, zstdCompressSync } from "node:zlib";
import { inspectArtifact, packageArtifact, verifyPackage, validateDist } from "./package-frontend-artifact.mjs";

const repo = fileURLToPath(new URL("../..", import.meta.url));
const publicFiles = () => new Map([["runtime-config.json", Buffer.from("{}\n")]]);
function distFiles() {
  const manifest = {
    "index.html": { file: "assets/main-123.js", isEntry: true, css: ["assets/main-123.css"], imports: ["shared"] },
    shared: { file: "assets/shared-123.js", assets: ["assets/font-123.woff2"] },
  };
  const files = new Map([
    [".vite/manifest.json", Buffer.from(JSON.stringify(manifest))],
    ["index.html", Buffer.from('<html><script type="module" src="/assets/main-123.js"></script></html>')],
    ["assets/main-123.js", Buffer.from('console.log("built");')],
    ["assets/shared-123.js", Buffer.from('export const shared="built";')],
    ["assets/main-123.css", Buffer.from("body{color:black}")],
    ["assets/font-123.woff2", Buffer.from([0, 1, 2])], ...publicFiles(),
  ]);
  for (const [name, bytes] of [...files]) if (/^assets\/.*\.(?:js|css)$/.test(name)) {
    for (const [suffix, encode] of [[".br", brotliCompressSync], [".gz", gzipSync], [".zst", zstdCompressSync]]) files.set(name + suffix, encode(bytes));
  }
  return files;
}
async function putTree(root, files) {
  for (const [name, bytes] of files) { const file = path.join(root, name); await fs.mkdir(path.dirname(file), {recursive:true}); await fs.writeFile(file, bytes); }
}
async function fixture(run) {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "frontend-package-")), checkout = path.join(root, "checkout"), dist = path.join(root, "dist"), out = path.join(root, "package");
  try {
    await putTree(dist, distFiles());
    await putTree(checkout, new Map([
      ["web/src/main.tsx", "export const UI = true;"], ["web/build/plugin.ts", "export const build = true;"],
      ["web/public/runtime-config.json", "{}\n"], ["web/index.html", "<html></html>"],
      ["web/vite.config.ts", "export default {};"], ["web/package.json", '{"version":"2.0.0"}'],
      ["web/pnpm-lock.yaml", "lockfileVersion: '9.0'"], ["web/pnpm-workspace.yaml", "packages: []"],
      ["sdk/typescript/generated.ts", "export type Generated = true;"],
    ]));
    for (const name of ["Dockerfile.artifact", "nginx.conf", "nginx-precompressed-assets.conf", "docker-entrypoint.d/40-runtime-config.sh", "docker-entrypoint.d/45-security-headers.sh"]) await putTree(checkout, new Map([[`web/${name}`, await fs.readFile(path.join(repo, "web", name))]]));
    await run({root,checkout,dist,out});
  } finally { await fs.rm(root, {recursive:true,force:true}); }
}

test("real manifest, static references and all encoding bytes pass", () => {
  assert.deepEqual(validateDist(distFiles(), publicFiles()), {manifestEntries:2,staticAssets:4,representations:9,sourceMaps:0});
});
for (const [name, mutate, message] of [
  ["missing manifest", f => f.delete(".vite/manifest.json"), /Missing artifact/],
  ["unresolved module", f => { const m = JSON.parse(f.get(".vite/manifest.json")); m.shared.imports = ["missing"]; f.set(".vite/manifest.json", Buffer.from(JSON.stringify(m))); }, /Unresolved/],
  ["path traversal", f => { const m = JSON.parse(f.get(".vite/manifest.json")); m.shared.file = "../private.env"; f.set(".vite/manifest.json", Buffer.from(JSON.stringify(m))); }, /Invalid artifact path/],
  ["missing compression", f => f.delete("assets/main-123.js.zst"), /Missing artifact/],
  ["mismatched final compressed code", f => f.set("assets/main-123.js", Buffer.from('console.log("changed after compression");')), /Compressed bytes differ/],
  ["invalid compressed bytes", f => f.set("assets/main-123.js.gz", Buffer.from("invalid gzip")), /Invalid compressed/],
  ["source map", f => f.set("assets/main.js.map", Buffer.from("private source")), /Forbidden artifact/],
  ["compressed source map", f => f.set("assets/main.js.map.gz", gzipSync("private source")), /Forbidden artifact/],
  ["private env", f => f.set(".env.local", Buffer.from("PRIVATE_ENV=do-not-copy")), /Forbidden artifact/],
  ["unrecognized source", f => f.set("main.tsx", Buffer.from("private source")), /Unrecognized/],
  ["changed public runtime config", f => f.set("runtime-config.json", Buffer.from('{"apiBaseUrl":"https://private.invalid"}')), /changed public/],
  ["unrecognized compressed output", f => f.set("private.json.gz", gzipSync("private")), /Unexpected compressed/],
]) test(`packaging rejects ${name}`, () => { const files=distFiles(); mutate(files); assert.throws(() => validateDist(files,publicFiles()), message); });

test("inline source maps fail even when regenerated compression matches", () => {
  const files=distFiles(), code=Buffer.from("console.log(1);\n//# sourceMappingURL=data:application/json;base64,e30=");
  files.set("assets/main-123.js",code);
  for (const [ext,encode] of [[".br",brotliCompressSync],[".gz",gzipSync],[".zst",zstdCompressSync]]) files.set("assets/main-123.js"+ext,encode(code));
  assert.throws(() => validateDist(files,publicFiles()),/Source-map reference/);
});

test("package contains only compiled files and exact security configuration, with one FRONTEND_IMAGE interface", async () => fixture(async ({checkout,dist,out}) => {
  const options={repo:checkout}, {summary}=await inspectArtifact(dist,options);
  const receipt=await packageArtifact(dist,out,{artifact:summary.artifactSha256,source:summary.checkoutSha256},options);
  const verified=await verifyPackage(out);
  assert.equal(verified.image,receipt.image);
  assert.match(receipt.image,/^agentforge-web-dev:[a-f0-9]{24}$/);
  assert.equal(await fs.readFile(path.join(out,"image.env"),"utf8"),`FRONTEND_IMAGE=${receipt.image}\n`);
  assert.equal(receipt.dockerBuildArgv.at(-1),path.join(await fs.realpath(out),"context"));
  assert.ok(receipt.dockerBuildArgv.includes("--pull=false"));
  assert.equal(receipt.dockerBuildArgv[receipt.dockerBuildArgv.indexOf("--tag")+1],receipt.image);
  for (const name of ["nginx.conf","nginx-precompressed-assets.conf","docker-entrypoint.d/40-runtime-config.sh","docker-entrypoint.d/45-security-headers.sh"]) assert.deepEqual(await fs.readFile(path.join(out,"context",name)),await fs.readFile(path.join(checkout,"web",name)));
  const files=receipt.contextFiles.map(item=>item.path);
  assert.equal(files.some(name=>/node_modules|\.vite|\.env|\.tsx?$|\.map(?:\.|$)/.test(name)),false);
  const dockerfile=await fs.readFile(path.join(out,"context/Dockerfile"),"utf8");
  assert.match(dockerfile,/USER 101:101/); assert.match(dockerfile,/RUNTIME_CONFIG_OUTPUT_PATH=\/tmp\/runtime-config.json/);
  assert.doesNotMatch(dockerfile,/RUN |node:|pnpm|npm install/);
  await fs.writeFile(path.join(out,"context/nginx.conf"),"tampered");
  await assert.rejects(verifyPackage(out),/no longer matches/);
}));

test("expected hashes and fresh external output are mandatory and fail before writing", async () => fixture(async ({checkout,dist,out}) => {
  const options={repo:checkout},{summary}=await inspectArtifact(dist,options),expected={artifact:summary.artifactSha256,source:summary.checkoutSha256};
  await assert.rejects(packageArtifact(dist,out,{},options),/Both expected/);
  await assert.rejects(packageArtifact(dist,out,{...expected,artifact:"0".repeat(64)},options),/identity changed/);
  await assert.rejects(packageArtifact(dist,out,{...expected,source:"0".repeat(64)},options),/identity changed/);
  await assert.rejects(fs.stat(out),{code:"ENOENT"});
  await assert.rejects(packageArtifact(dist,path.join(checkout,"bad"),expected,options),/outside the checkout/);
  await assert.rejects(packageArtifact(dist,path.join(dist,"bad"),expected,options),/inside the dist/);
  await fs.mkdir(out); await fs.writeFile(path.join(out,"preserve"),"existing");
  await assert.rejects(packageArtifact(dist,out,expected,options),{code:"EEXIST"});
  assert.equal(await fs.readFile(path.join(out,"preserve"),"utf8"),"existing");
}));

test("symlinks cannot introduce source or private env into a context", async () => fixture(async ({checkout,dist,root}) => {
  await fs.writeFile(path.join(root,"private.env"),"PRIVATE=do-not-copy");
  await fs.symlink(path.join(root,"private.env"),path.join(dist,"assets/private.js"));
  await assert.rejects(inspectArtifact(dist,{repo:checkout}),/Non-regular artifact/);
}));

test("source bytes and base reference change identity independently of unchanged dist", async () => fixture(async ({checkout,dist}) => {
  const original=await inspectArtifact(dist,{repo:checkout});
  await fs.writeFile(path.join(checkout,"web/src/main.tsx"),"source changed");
  const next=await inspectArtifact(dist,{repo:checkout,baseImage:"local/frontend-base:verified"});
  assert.equal(next.summary.artifactSha256,original.summary.artifactSha256);
  assert.notEqual(next.summary.checkoutSha256,original.summary.checkoutSha256);
  assert.equal(next.summary.baseImage,"local/frontend-base:verified");
  assert.match(next.summary.provenance,/independently/);
}));


for (const [name, mutate, message] of [
  ["build argv", async (out,receipt) => { receipt.dockerBuildArgv[0]="not-docker"; await fs.writeFile(path.join(out,"artifact-receipt.json"),JSON.stringify(receipt)); }, /Docker build arguments/],
  ["image env", async out => { await fs.writeFile(path.join(out,"image.env"),"FRONTEND_IMAGE=wrong:tag\n"); }, /FRONTEND_IMAGE/],
  ["source inventory", async out => { await fs.writeFile(path.join(out,"source-manifest.json"),"[]\n"); }, /source inventory/],
]) test(`verify-context rejects changed ${name}`, async () => fixture(async ({checkout,dist,out}) => {
  const options={repo:checkout},{summary}=await inspectArtifact(dist,options);
  const receipt=await packageArtifact(dist,out,{artifact:summary.artifactSha256,source:summary.checkoutSha256},options);
  await mutate(out,receipt);
  await assert.rejects(verifyPackage(out),message);
}));
