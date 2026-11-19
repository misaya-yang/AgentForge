/** Owner-authenticated session reads, isolated from the API-key model transport. */
import { isIP } from "node:net";

const MAX_RESPONSE_BYTES = 8 * 1024 * 1024;

export class GatewaySessionClient {
  constructor(
    private readonly origin: string,
    private readonly token: string | (()=>Promise<string>) | undefined,
    private readonly timeoutSeconds: number,
  ) {}

  list(): Promise<any> {
    return this.read("/api/v1/assistant/sessions?limit=20");
  }

  history(sessionId: string): Promise<any> {
    if (!sessionId || sessionId === "." || sessionId === ".." || sessionId.length > 1024 || /[\r\n\0]/.test(sessionId)) {
      return Promise.reject(new Error("Invalid session ID"));
    }
    return this.read(`/api/v1/assistant/sessions/${encodeURIComponent(sessionId)}/history`);
  }

  private async read(path: string): Promise<any> {
    let origin: URL;
    try { origin = new URL(this.origin); } catch {
      throw new Error("Session reads require an HTTPS Gateway origin or explicit loopback IP over HTTP, such as http://127.0.0.1:8080");
    }
    const host = origin.hostname.replace(/^\[|\]$/g, "");
    const loopback = host === "::1" || (isIP(host) === 4 && host.startsWith("127."));
    if (origin.username || origin.password || origin.search || origin.hash || origin.pathname !== "/"
      || !(origin.protocol === "https:" || (origin.protocol === "http:" && loopback))) {
      throw new Error("Session reads require an HTTPS Gateway origin or explicit loopback IP over HTTP, such as http://127.0.0.1:8080");
    }
    const url = new URL(path, origin);
    if (url.origin !== origin.origin) throw new Error("Invalid session read destination");
    const token=typeof this.token==="function" ? await this.token() : this.token;
    if (!token || token.length > 16_384 || /\s/.test(token)) {
      throw new Error("Use Gateway device login or AI_GATEWAY_SESSION_TOKEN for /history and /resume; API keys cannot authenticate session reads");
    }
    const controller = new AbortController();
    const timeout = Number.isFinite(this.timeoutSeconds)
      ? Math.min(300, Math.max(1, this.timeoutSeconds)) : 30;
    const timer = setTimeout(() => controller.abort(), timeout * 1000);
    try {
      let response: Response;
      try {
        response = await fetch(url, {
          method: "GET",
          headers: { Authorization: `Bearer ${token}`, Accept: "application/json" },
          redirect: "error",
          signal: controller.signal,
        });
      } catch {
        throw new Error("Session read failed; check the Gateway connection and account session token");
      }
      if (!response.ok) {
        await response.body?.cancel().catch(() => {});
        throw new Error(`Gateway session read returned HTTP ${response.status}`);
      }
      if (!response.body) throw new Error("Invalid session response");
      const reader = response.body.getReader();
      const chunks: Uint8Array[] = [];
      let size = 0;
      try {
        while (true) {
          const { done, value } = await reader.read();
          if (done) break;
          size += value.byteLength;
          if (size > MAX_RESPONSE_BYTES) throw new Error("response limit");
          chunks.push(value);
        }
        const bytes = new Uint8Array(size);
        let offset = 0;
        for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
        return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
      } catch {
        await reader.cancel().catch(() => {});
        throw new Error("Invalid or oversized session response");
      } finally {
        reader.releaseLock();
      }
    } finally {
      clearTimeout(timer);
    }
  }
}
