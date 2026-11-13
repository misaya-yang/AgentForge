import type { Plugin } from "vite";
import { gzipSync, brotliCompressSync } from "zlib";

/**
 * Vite plugin for precompressing build assets with gzip and brotli.
 * Generates .gz and .br files alongside originals for static serving.
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
        }
      }
    },
  };
}

/**
 * Compress a single asset for testing purposes.
 */
export function compressAsset(content: string | Buffer): { gzip: Buffer; brotli: Buffer } {
  const buffer = typeof content === "string" ? Buffer.from(content, "utf-8") : content;
  return {
    gzip: gzipSync(buffer, { level: 9 }),
    brotli: brotliCompressSync(buffer),
  };
}
