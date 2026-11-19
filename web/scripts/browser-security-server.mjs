// Production-built browser fixture: the same generated header values as nginx.
import { createServer } from "node:http";
import { readFile, stat } from "node:fs/promises";
import { mkdirSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { build } from "vite";

const root = fileURLToPath(new URL("..", import.meta.url));
const output = process.env.SECURITY_TEST_OUTPUT || mkdtempSync(path.join(tmpdir(), "agentforge-csp-"));
mkdirSync(output, { recursive: true });
const headersPath = path.join(output, "headers.conf");
execFileSync("sh", [path.join(root, "docker-entrypoint.d/45-security-headers.sh")], {
  env: { ...process.env, FRONTEND_CSP_MODE: process.env.FRONTEND_CSP_MODE || "enforce", FRONTEND_SECURITY_HEADERS_OUTPUT_PATH: headersPath },
});
const headers = Object.fromEntries([...readFileSync(headersPath, "utf8").matchAll(/^add_header ([\w-]+) "([^"]*)" always;$/gm)].map(match => [match[1], match[2]]));
await build({
  root, configFile: path.join(root, "vite.config.ts"),
  build: { outDir: path.join(output, "dist"), emptyOutDir: true, rollupOptions: { input: { main: path.join(root, "index.html"), security: path.join(root, "e2e/fixtures/console-security.html") } } },
});
const types = { ".html": "text/html", ".js": "application/javascript", ".css": "text/css", ".json": "application/json", ".svg": "image/svg+xml", ".png": "image/png", ".woff2": "font/woff2" };
createServer(async (req, res) => {
  try {
    const pathname = decodeURIComponent(new URL(req.url, "http://127.0.0.1").pathname);
    const target = path.resolve(output, "dist", `.${pathname}`);
    if (!target.startsWith(path.join(output, "dist") + path.sep)) { res.writeHead(403); return res.end(); }
    let file = target;
    try { if (!(await stat(file)).isFile()) file = path.join(output, "dist/index.html"); }
    catch { file = path.join(output, "dist/index.html"); }
    res.writeHead(200, { ...headers, "Content-Type": types[path.extname(file)] || "application/octet-stream", "Cache-Control": "no-store" });
    res.end(await readFile(file));
  } catch { res.writeHead(500); res.end(); }
}).listen(4182, "127.0.0.1");
