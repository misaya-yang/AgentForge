import type { Plugin } from "vite";
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
    async generateBundle(_options, bundle) {
      for (const [fileName, asset] of Object.entries(bundle)) {
        if (asset.type === "asset" && typeof asset.source === "string") {
          const buffer = Buffer.from(asset.source, "utf-8");
          this.emitFile({
            type: "asset",
            fileName: `${fileName}.gz`,
            source: gzipSync(buffer, { level: 9 }),
          });
          this.emitFile({
            type: "asset",
            fileName: `${fileName}.br`,
            source: brotliCompressSync(buffer),
          });
          this.emitFile({
            type: "asset",
            fileName: `${fileName}.zst`,
            source: zstdCompressSync(buffer),
          });
        } else if (asset.type === "chunk") {
          const buffer = Buffer.from(asset.code, "utf-8");
          this.emitFile({
            type: "asset",
            fileName: `${fileName}.gz`,
            source: gzipSync(buffer, { level: 9 }),
          });
          this.emitFile({
            type: "asset",
            fileName: `${fileName}.br`,
            source: brotliCompressSync(buffer),
          });
          this.emitFile({
            type: "asset",
            fileName: `${fileName}.zst`,
            source: zstdCompressSync(buffer),
          });
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
