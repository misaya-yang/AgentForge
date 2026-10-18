import https from 'node:https';
import { loadConfig } from './config.mjs';
import { Store } from './store.mjs';
import { createAdapter } from './adapter.mjs';
import { canonical, reject } from './contract.mjs';
export function validatedHeaders(raw) { const headers = {}; for (let i = 0; i < raw.length; i += 2) { const name = raw[i].toLowerCase(); if (Object.hasOwn(headers, name)) reject(400, 'DUPLICATE_HEADER'); headers[name] = raw[i + 1]; } return headers; }
export function bootstrap(file) {
  process.umask(0o077);
  const config = loadConfig(file); const store = new Store(config.stateDirectory); const adapter = createAdapter(config, store);
  const server = https.createServer({ ...config.tlsMaterial, minVersion: 'TLSv1.3', maxHeaderSize: 32768, requestTimeout: 15000, headersTimeout: 10000 }, async (req, res) => {
    res.setHeader('content-type', 'application/json'); res.setHeader('cache-control', 'no-store'); res.setHeader('x-content-type-options', 'nosniff');
    try { const headers = validatedHeaders(req.rawHeaders); const chunks = []; let size = 0; for await (const chunk of req) { size += chunk.length; if (size > 262144) reject(413, 'REQUEST_TOO_LARGE'); chunks.push(chunk); } const result = await adapter.handle({ method: req.method, path: req.url, headers, body: Buffer.concat(chunks) }); res.writeHead(result.status); res.end(canonical(result.body)); }
    catch (e) { if (!res.headersSent) res.writeHead(Number.isInteger(e.status) ? e.status : 400); res.end(canonical({ error: { code: typeof e.code === 'string' && /^[A-Z_]+$/.test(e.code) ? e.code : 'INVALID_REQUEST' } })); }
  });
  server.maxRequestsPerSocket = 100; server.keepAliveTimeout = 5000;
  let closing = false; async function close() { if (closing) return; closing = true; server.close(); const forced = setTimeout(() => process.exit(1), config.limits.maxRunMs + 10000); await adapter.shutdown(); store.close(); clearTimeout(forced); server.closeAllConnections(); }
  server.once('error', () => { process.stderr.write('EXTERNAL_A2A_LISTENER_FAILED\n'); process.exitCode = 1; void close(); });
  process.once('SIGTERM', close); process.once('SIGINT', close); server.listen(config.port, config.bind); return { server, close };
}
if (import.meta.url === `file://${process.argv[1]}`) { if (process.argv.length !== 4 || process.argv[2] !== '--config') { process.stderr.write('usage: node src/server.mjs --config /absolute/private/config.json\n'); process.exitCode = 2; } else { try { bootstrap(process.argv[3]); } catch { process.stderr.write('EXTERNAL_A2A_STARTUP_REJECTED\n'); process.exitCode = 1; } } }
