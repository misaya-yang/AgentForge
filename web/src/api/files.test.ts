import assert from "node:assert/strict";
import { registerHooks } from "node:module";
import { test, type TestContext } from "node:test";

let fixtureId = 0;

// Exercise the actual upload module and native dynamic-import cache. Only the
// HTTP transport and browser-only image codec are replaced; no browser claim.
async function fixture(t: TestContext) {
  const id = ++fixtureId;
  const moduleUrl = new URL(`./files.ts?fixture=${id}`, import.meta.url).href;
  const apiUrl = `data:text/javascript,${encodeURIComponent(`
    export const calls = [];
    export const response = { file_id: 'uploaded-${id}' };
    export const api = { post: async (...args) => {
      calls.push(args);
      args[2].onUploadProgress({ loaded: 3, total: 4 });
      return { data: response };
    } };
  `)}`;
  const codecUrl = `data:text/javascript,${encodeURIComponent(`
    export const state = { id: ${id}, calls: [], error: null, wait: null };
    export default async function imageCompression(file, options) {
      state.calls.push({ file, options });
      if (state.error) throw state.error;
      if (state.wait) await state.wait;
      return new Blob(['compressed'], { type: 'image/webp' });
    }
  `)}`;
  let codecLoads = 0;
  let codecResolves = 0;
  let importFailure = false;
  const hooks = registerHooks({
    resolve(specifier, context, nextResolve) {
      if (context.parentURL === moduleUrl) {
        if (specifier === "@/lib/api") return { url: apiUrl, shortCircuit: true };
        if (specifier === "@/lib/format") {
          return nextResolve(new URL("../lib/format.ts", import.meta.url).href, context);
        }
        if (specifier === "browser-image-compression") {
          codecResolves++;
          if (importFailure) throw new Error("fixture codec chunk unavailable");
          return { url: codecUrl, shortCircuit: true };
        }
      }
      return nextResolve(specifier, context);
    },
    load(url, context, nextLoad) {
      if (url === codecUrl) codecLoads++;
      return nextLoad(url, context);
    },
  });
  t.after(() => hooks.deregister());
  const http = await import(apiUrl);
  const files = await import(moduleUrl) as typeof import("./files.ts");
  return {
    ...files, http,
    codecLoads: () => codecLoads,
    codecResolves: () => codecResolves,
    failImport: () => { importFailure = true; },
    codec: () => import(codecUrl),
  };
}

function file(size: number, type = "image/png", name = "image.png") {
  return new File([new Uint8Array(size)], name, { type });
}

function uploaded(f: Awaited<ReturnType<typeof fixture>>, index = 0): File {
  const [path, body, options] = f.http.calls[index];
  assert.equal(path, "/api/v1/files/upload");
  assert.equal(options.headers, undefined, "the generated codec must own the multipart boundary");
  assert.deepEqual(Object.keys(body), ["file"]);
  return body.file;
}

test("file utilities do not load the image codec", async t => {
  const f = await fixture(t);
  assert.equal(f.isImageFile(file(1)), true);
  assert.equal(f.isFileTypeSupported(file(1)), true);
  assert.equal(f.formatFileSize(1024), "1.0 KB");
  assert.equal(f.codecResolves(), 0);
  assert.equal(f.codecLoads(), 0);
});

for (const [name, size, type, compress] of [
  ["non-image", 3 * 1024 * 1024, "application/pdf", true],
  ["small image", 32, "image/png", true],
  ["threshold-sized image", 2 * 1024 * 1024, "image/png", true],
  ["explicitly disabled compression", 3 * 1024 * 1024, "image/png", false],
] as const) {
  test(`${name} uploads unchanged without loading the codec`, async t => {
    const f = await fixture(t);
    const original = file(size, type);
    const progress: unknown[] = [];
    assert.equal(await f.uploadFileWithProgress(original, event => progress.push(event), compress), f.http.response);
    assert.equal(uploaded(f), original);
    assert.deepEqual(progress, [{ loaded: 3, total: 4, percent: 75 }]);
    assert.equal(f.codecResolves(), 0);
    assert.equal(f.codecLoads(), 0);
  });
}

test("large image lazily compresses with unchanged options, filename and MIME", async t => {
  const f = await fixture(t);
  const original = file(2 * 1024 * 1024 + 1, "image/png", "original.png");
  assert.equal(f.codecLoads(), 0);
  await f.uploadFileWithProgress(original);
  const { state } = await f.codec();
  assert.equal(f.codecLoads(), 1);
  assert.equal(state.calls.length, 1);
  assert.equal(state.calls[0].file, original);
  assert.deepEqual(state.calls[0].options, {
    maxSizeMB: 2, maxWidthOrHeight: 2048, initialQuality: 0.85, useWebWorker: false,
  });
  assert.equal(uploaded(f).name, "original.png");
  assert.equal(uploaded(f).type, "image/webp");
  assert.equal(await uploaded(f).text(), "compressed");
});

test("codec import failure still uploads the original file", async t => {
  const f = await fixture(t);
  f.failImport();
  const original = file(2 * 1024 * 1024 + 1);
  assert.equal(await f.uploadFileWithProgress(original), f.http.response);
  assert.equal(uploaded(f), original);
  assert.equal(f.codecResolves(), 1);
  assert.equal(f.codecLoads(), 0);
});

test("codec failure still uploads the original file", async t => {
  const f = await fixture(t);
  const { state } = await f.codec();
  state.error = new Error("fixture compression failure");
  const original = file(2 * 1024 * 1024 + 1);
  await f.uploadFileWithProgress(original);
  assert.equal(uploaded(f), original);
});

test("simultaneous image uploads share the native module cache and keep separate compression work", async t => {
  const f = await fixture(t);
  const originals = [file(2 * 1024 * 1024 + 1, "image/png", "one.png"), file(2 * 1024 * 1024 + 1, "image/png", "two.png")];
  // The imports start together without preloading the codec in this fixture.
  const uploads = originals.map(original => f.uploadFileWithProgress(original));
  await Promise.all(uploads);
  const { state } = await f.codec();
  assert.equal(f.codecLoads(), 1);
  assert.equal(state.calls.length, 2);
  assert.deepEqual(state.calls.map((call: { file: File }) => call.file), originals);
  assert.deepEqual(f.http.calls.map((_call: unknown, index: number) => uploaded(f, index).name).sort(), ["one.png", "two.png"]);
});

test("an in-progress compression does not block another uncompressed attachment", async t => {
  const f = await fixture(t);
  const { state } = await f.codec();
  let release!: () => void;
  state.wait = new Promise<void>(resolve => { release = resolve; });
  const compressing = f.uploadFileWithProgress(file(2 * 1024 * 1024 + 1));
  const small = file(32);
  await f.uploadFileWithProgress(small);
  assert.equal(f.http.calls.length, 1);
  assert.equal(uploaded(f), small);
  release();
  await compressing;
  assert.equal(f.http.calls.length, 2);
});
