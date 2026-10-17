/** The wire format deliberately has no free-text message, URL, prompt, user or tenant fields. */
export type RouteName = "/" | "/assistant" | "/playground" | "/knowledge/:id" | "/agents/:id" | "/eval/:id" | "/settings" | "/dashboard" | "/tasks" | "/other";
export type MetricName = "INP" | "LCP" | "CLS" | "route_commit" | "api_headers" | "chat_first_token" | "chat_duration";
export type ErrorName = "Error" | "TypeError" | "RangeError" | "ReferenceError" | "SyntaxError" | "UnhandledRejection" | "RenderError";
export interface ErrorFrame { asset: string; line: number; column: number }
export type TelemetryRecord =
  | { kind: "metric"; name: MetricName; route: RouteName; value: number; quality?: "native" | "observed"; trace_id?: string; span_id?: string; http_status?: number }
  | { kind: "error"; name: ErrorName; route: RouteName; frames: ErrorFrame[]; trace_id?: string };
export interface TelemetryAuthority { key: string; token: string | null; authenticated: boolean; sessionType?: "cookie"; csrfToken?: string | null }
export const TELEMETRY_PATH = "/api/v1/telemetry/browser";
const ERROR_NAMES = new Set(["Error", "TypeError", "RangeError", "ReferenceError", "SyntaxError"]);
export function routeName(path: string): RouteName {
  // Only fixed product route families leave the browser. No dynamic identifiers/query/hash.
  const root = path.split(/[/?#]/).filter(Boolean)[0] ?? "";
  if (!root) return "/";
  if (["assistant", "playground", "settings", "dashboard", "tasks"].includes(root)) return `/${root}` as RouteName;
  if (["knowledge", "agents", "eval"].includes(root)) return `/${root}/:id` as RouteName;
  return "/other";
}
export function scrubError(error: unknown, origin: string, kind?: "UnhandledRejection" | "RenderError"): Pick<Extract<TelemetryRecord, { kind: "error" }>, "name" | "frames"> {
  const value = error instanceof Error ? error : null;
  const name = kind ?? (value && ERROR_NAMES.has(value.name) ? value.name as ErrorName : "Error");
  const frames: ErrorFrame[] = [];
  // Error messages, function names, component stacks, arbitrary rejection values and queries are never serialized.
  for (const match of (value?.stack ?? "").slice(0, 8192).split("\n").slice(1).join("\n").matchAll(/(https?:\/\/[^\s)]+?):(\d+):(\d+)/g)) {
    try {
      const url = new URL(match[1]);
      if (url.origin !== origin || !/^\/assets\/[A-Za-z0-9_.-]{1,120}\.js$/.test(url.pathname)) continue;
      const line = Number(match[2]), column = Number(match[3]);
      if (line > 0 && column > 0 && line <= 10_000_000 && column <= 10_000_000) frames.push({ asset: url.pathname, line, column });
      if (frames.length === 5) break;
    } catch { /* Unparseable frames carry no reportable location. */ }
  }
  return { name, frames };
}

/** Core Web Vitals aggregation. INP reports its precision when native interactionCount is unavailable. */
export class VitalAccumulator {
  lcp = 0;
  cls = 0;
  private clsStart = 0;
  private clsLast = 0;
  private clsValue = 0;
  private interactions = new Map<number, number>();
  private firstInteraction = Infinity;
  private lastInteraction = 0;
  addLcp(startTime: number) { if (Number.isFinite(startTime) && startTime >= 0) this.lcp = startTime; }
  addLayoutShift(time: number, value: number, recentInput: boolean) {
    if (recentInput || !Number.isFinite(value) || value < 0) return;
    if (!this.clsValue || time - this.clsLast >= 1000 || time - this.clsStart >= 5000) { this.clsStart = time; this.clsValue = 0; }
    this.clsLast = time; this.clsValue += value; this.cls = Math.max(this.cls, this.clsValue);
  }
  addInteraction(id: number, duration: number) {
    if (!id || !Number.isFinite(duration) || duration <= 0) return;
    this.firstInteraction = Math.min(this.firstInteraction, id); this.lastInteraction = Math.max(this.lastInteraction, id);
    this.interactions.set(id, Math.max(this.interactions.get(id) ?? 0, duration));
    // Only the 10 slowest interactions can contribute to a 98th percentile for ordinary page sessions.
    if (this.interactions.size > 10) {
      const smallest = [...this.interactions].sort((a, b) => a[1] - b[1])[0]; this.interactions.delete(smallest[0]);
    }
  }
  inp(nativeCount?: number): { value: number; quality: "native" | "observed" } | undefined {
    if (!this.interactions.size) return;
    const count = nativeCount ?? Math.max(1, (this.lastInteraction - this.firstInteraction) / 7 + 1);
    const values = [...this.interactions.values()].sort((a, b) => b - a);
    return { value: values[Math.min(values.length - 1, Math.floor(count / 50))], quality: nativeCount === undefined ? "observed" : "native" };
  }
}

const ROUTES = new Set<RouteName>(["/", "/assistant", "/playground", "/knowledge/:id", "/agents/:id", "/eval/:id", "/settings", "/dashboard", "/tasks", "/other"]);
const METRICS = new Set<MetricName>(["INP", "LCP", "CLS", "route_commit", "api_headers", "chat_first_token", "chat_duration"]);
/** Runtime allowlist: extra fields cannot hitchhike through a typed call or legacy event. */
export function sanitizeRecord(input: TelemetryRecord): TelemetryRecord | undefined {
  if (!ROUTES.has(input.route)) return;
  if (input.kind === "metric") {
    if (!METRICS.has(input.name) || !Number.isFinite(input.value) || input.value < 0 || input.value > (input.name === "CLS" ? 10 : 3_600_000)) return;
    return { kind: "metric", name: input.name, route: input.route, value: input.value,
      ...(["native", "observed"].includes(input.quality ?? "") ? { quality: input.quality } : {}),
      ...(/^(?!0{16}$)[a-f0-9]{16}$/.test(input.span_id ?? "") ? { span_id: input.span_id } : {}),
      ...(Number.isInteger(input.http_status) && input.http_status! >= 100 && input.http_status! <= 599 ? { http_status: input.http_status } : {}) };
  }
  if (input.kind !== "error" || ![...ERROR_NAMES, "UnhandledRejection", "RenderError"].includes(input.name)) return;
  const frames = (Array.isArray(input.frames) ? input.frames : []).slice(0, 5).filter(frame => /^\/assets\/[A-Za-z0-9_.-]{1,120}\.js$/.test(frame.asset) && Number.isInteger(frame.line) && frame.line > 0 && frame.line <= 10_000_000 && Number.isInteger(frame.column) && frame.column > 0 && frame.column <= 10_000_000).map(frame => ({ asset: frame.asset, line: frame.line, column: frame.column }));
  return { kind: "error", name: input.name, route: input.route, frames };
}

export function createTelemetryClient(options: { origin: string; authority: () => TelemetryAuthority; fetch: typeof fetch; now?: () => number }) {
  const now = options.now ?? Date.now;
  let epoch = 0;
  let session: { id: string; key: string; expires: number } | undefined;
  let configuring = false;
  let refreshAfter = 0;
  let queue: TelemetryRecord[] = [];
  let minuteStart = now(), count = 0;
  const controllers = new Set<AbortController>();
  const valid = () => Boolean(session && options.authority().authenticated && options.authority().token && session.key === options.authority().key && session.expires > now());
  function reset() { epoch++; session = undefined; queue = []; configuring = false; refreshAfter = 0; for (const controller of controllers) controller.abort(); controllers.clear(); }
  function authHeaders(authority: TelemetryAuthority, write = false): Record<string, string> {
    if (authority.sessionType === "cookie") {
      if (write && !authority.csrfToken) throw new Error("Console session CSRF bootstrap required");
      return write ? { "X-CSRF-Token": authority.csrfToken! } : {};
    }
    return { Authorization: `Bearer ${authority.token}` };
  }
  async function configure() {
    if (configuring) return;
    const authority = options.authority();
    reset();
    if (!authority.authenticated || !authority.token) return;
    configuring = true; const generation = epoch; const controller = new AbortController(); controllers.add(controller);
    const timeout = setTimeout(() => controller.abort(), 5000);
    try {
      const response = await options.fetch(`${options.origin}${TELEMETRY_PATH}/config`, { headers: authHeaders(authority), credentials: "same-origin", mode: "same-origin", redirect: "error", cache: "no-store", signal: controller.signal });
      if (!response.ok) return;
      const config = await response.json();
      if (generation !== epoch || authority.key !== options.authority().key) return;
      if (typeof config.enabled !== "boolean" || typeof config.sampled !== "boolean" || typeof config.sessionId !== "string" || !/^(?!0{32}$)[a-f0-9]{32}$/.test(config.sessionId)) return;
      if (typeof config.sampleRate !== "number" || !(config.sampleRate >= 0 && config.sampleRate <= 0.1) || typeof config.ttlSeconds !== "number" || !(config.ttlSeconds >= 30 && config.ttlSeconds <= 900)) return;
      refreshAfter = now() + config.ttlSeconds * 1000;
      if (!config.enabled || !config.sampled || config.sampleRate === 0) return;
      session = { id: config.sessionId, key: authority.key, expires: now() + config.ttlSeconds * 1000 };
      minuteStart = now(); count = 0;
    } catch { /* A collector outage must not affect the product or trigger reporting loops. */ }
    finally { clearTimeout(timeout); controllers.delete(controller); if (generation === epoch) { configuring = false; if (!refreshAfter) refreshAfter = now() + 60_000; } }
  }
  function record(record: TelemetryRecord) {
    if (!valid()) return;
    if (now() - minuteStart >= 60_000) { minuteStart = now(); count = 0; }
    if (record.kind === "metric" && (!Number.isFinite(record.value) || record.value < 0 || record.value > 3_600_000)) return;
    const safe = sanitizeRecord(record);
    if (!safe) return;
    const priority = safe.kind === "error" || ["INP", "LCP", "CLS"].includes(safe.name);
    if (safe.kind === "metric" && ["INP", "LCP", "CLS"].includes(safe.name)) {
      const existing = queue.findIndex(item => item.kind === "metric" && item.name === safe.name && item.route === safe.route);
      if (existing >= 0) { queue[existing] = { ...safe, trace_id: session!.id }; return; }
    }
    if (priority && (count >= 60 || queue.length >= 20)) {
      const discard = queue.findIndex(item => item.kind === "metric" && !["INP", "LCP", "CLS"].includes(item.name));
      if (discard >= 0) { queue.splice(discard, 1); count--; }
    }
    if (count >= 60 || queue.length >= 20) return;
    count++; queue.push({ ...safe, trace_id: session!.id });
  }
  async function flush() {
    if (!valid()) { queue = []; return; }
    if (!queue.length) return;
    const batch = queue; queue = []; const active = session!; const authority = options.authority();
    let body = JSON.stringify({ session_id: active.id, records: batch });
    // The allowlisted wire format is ASCII; cap below the server's 16 KiB body limit.
    while (body.length > 15 * 1024 && batch.length > 1) {
      queue.unshift(batch.pop()!); body = JSON.stringify({ session_id: active.id, records: batch });
    }
    const controller = new AbortController(); controllers.add(controller);
    const timeout = setTimeout(() => controller.abort(), 5000);
    try {
      await options.fetch(`${options.origin}${TELEMETRY_PATH}`, { method: "POST", headers: { "Content-Type": "application/json", ...authHeaders(authority, true) }, credentials: "same-origin", mode: "same-origin", redirect: "error", keepalive: true, signal: controller.signal, body });
    } catch { /* No retry: bounded diagnostic loss is preferable to replay or request amplification. */ }
    finally { clearTimeout(timeout); controllers.delete(controller); }
  }
  function traceparent(input: string): string | undefined {
    if (!valid()) return;
    try {
      const url = new URL(input, options.origin);
      if (url.origin !== options.origin || !/^\/(api|v1)\//.test(url.pathname) || url.pathname.startsWith(TELEMETRY_PATH)) return;
      const bytes = new Uint8Array(8); crypto.getRandomValues(bytes);
      if (bytes.every(byte => byte === 0)) bytes[0] = 1;
      return `00-${session!.id}-${[...bytes].map(byte => byte.toString(16).padStart(2, "0")).join("")}-01`;
    } catch { return; }
  }
  return { configure, reset, record, flush, traceparent, enabled: valid, needsRefresh: () => now() >= refreshAfter };
}
