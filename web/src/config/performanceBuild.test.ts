import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { brotliDecompressSync, gunzipSync, zstdDecompressSync } from "node:zlib";
import { compressAsset } from "../../build/precompression.ts";
test("all precompressed representations retain exact UTF-8 bytes", () => {
  const value = "日本語 中文 🌍\nconsole();".repeat(50), bytes = compressAsset(value);
  for (const [suffix, decode] of [[".br", brotliDecompressSync], [".gz", gunzipSync], [".zst", zstdDecompressSync]] as const) assert.equal(decode(bytes[suffix]).toString(), value);
});
test("edge negotiates only accepted encodings, preserves MIME/cache/headers, and blocks source maps", () => {
  const nginx = readFileSync(new URL("../../nginx.conf", import.meta.url), "utf8");
  const include = readFileSync(new URL("../../nginx-precompressed-assets.conf", import.meta.url), "utf8");
  const rows = [...nginx.matchAll(/"~\*(.*?)" \.(br|zst|gz);/g)].map(([, expression, suffix]) => [new RegExp(expression, "i"), suffix] as const);
  const negotiate = (header: string) => rows.find(([re]) => re.test(header))?.[1] ?? "";
  assert.equal(rows.length, 3); assert.equal(negotiate("br, gzip"), "br"); assert.equal(negotiate("zstd"), "zst");
  assert.equal(negotiate("br;q=0, zstd;q=0.0, gzip"), "gz"); assert.equal(negotiate("br;q=0, gzip;q=0"), "");
  assert.equal(negotiate("br;q=0.25"), "br"); assert.equal(negotiate("foobar"), "");
  assert.match(include, /Content-Encoding \$asset_encoding/); assert.match(include, /Vary "Accept-Encoding" always/);
  assert.match(include, /frontend-security-headers/); assert.match(include, /immutable/); assert.match(include, /if \(!-f \$request_filename\$asset_suffix\)/);
  assert.ok(nginx.includes("\\.map(?:\\.(?:br|zst|gz))?$"));
  for (const mime of ["application/javascript", "text/css", "application/json", "image/svg+xml", "application/wasm"]) assert.ok(nginx.includes(mime));
});
test("route budgets keep original limits, derive every router chunk, and contain no exceptions", () => {
  const gate = readFileSync(new URL("../../scripts/check-bundle-budget.mjs", import.meta.url), "utf8");
  assert.match(gate, /ROUTE_RAW_LIMIT = 500_000/); assert.match(gate, /PUBLIC_GZIP_LIMIT = 330_000/);
  assert.match(gate, /ASSISTANT_INCREMENT_GZIP_LIMIT = 220_000/);
  assert.doesNotMatch(gate, /EXISTING_ROUTE_EXCEPTIONS|routeException|\.has\(entry\.source\).*\|\|/);
  assert.match(gate, /src\/router\.tsx/); assert.match(gate, /Budgeted route missing from manifest/);
});
