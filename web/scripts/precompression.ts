import type { Plugin } from "vite";
import { writeFileSync } from "fs";
import { dirname, join } from "path";
import { gzipSync, brotliCompressSync, zstdCompressSync } from "zlib";

/**
 * Vite plugin for precompressing build assets with gzip, brotli, and zstd.
 * Generates .gz, .br, and .zst files alongside originals for static serving.
 */
export function precompression(): Plugin {
  return {
    name: "precompression",
    apply: "build",
    enforce: "post",
    // Compress what was actually written. In generateBundle, chunks can still
    // carry placeholders (for example __VITE_PRELOAD__) that later core hooks
    // rewrite, and nginx prefers these .br/.zst/.gz files over the originals.
    writeBundle(options, bundle) {
      const outDir = options.dir ?? (options.file ? dirname(options.file) : "dist");
      for (const [fileName, output] of Object.entries(bundle)) {
        const content = output.type === "chunk" ? output.code : output.source;
        if (typeof content !== "string") continue;
        for (const [suffix, bytes] of Object.entries(compressAsset(content))) {
          writeFileSync(join(outDir, `${fileName}${suffix}`), bytes);
        }
      }
    },
  };
}

/**
 * Compress a single asset for testing purposes.
 * Returns buffers keyed by file extension including the dot.
 */
export function compressAsset(content: string | Buffer): { ".gz": Buffer; ".br": Buffer; ".zst": Buffer } {
  const buffer = typeof content === "string" ? Buffer.from(content, "utf-8") : content;
  return {
    ".gz": gzipSync(buffer, { level: 9 }),
    ".br": brotliCompressSync(buffer),
    ".zst": zstdCompressSync(buffer),
  };
}
