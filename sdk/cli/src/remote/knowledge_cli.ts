/** Explicit hosted Knowledge management; never starts the independent Runtime. */
import { isIP } from "node:net";
import { loadConfig } from "../config.js";
import type { CLIConfig } from "../types/config.js";
import { GatewayClient, GatewayHttpError } from "./client.js";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const STATES = new Set(["pending", "processing", "completed", "failed", "cancelled"]);
const HELP = `Usage: ai-gateway gateway knowledge sources <dataset> [--base-url <origin>]
       ai-gateway gateway knowledge sync <dataset> <binding> --request-id <UUID> --confirm [--base-url <origin>]
       ai-gateway gateway knowledge status <dataset> <binding> --request-id <UUID> [--base-url <origin>]
Uses existing ~/.ai-gateway/config.json Gateway credentials. JSON stdout; guidance on stderr.
Sync performs full source reconciliation and may remove source-deleted documents.
Retain your request UUID before submission. If the outcome is uncertain, use status with that UUID; do not submit a new request.
`;

type Receipt = Record<string, unknown> & { task_id: string; binding_id: string; status: string };
interface Dependencies {
  loadConfig?: () => CLIConfig;
  signalSource?: { on(event: "SIGINT" | "SIGTERM", listener: () => void): unknown; off(event: "SIGINT" | "SIGTERM", listener: () => void): unknown };
  stdout?: Pick<NodeJS.WriteStream, "write">;
  stderr?: Pick<NodeJS.WriteStream, "write">;
}
function object(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}
function identity(value: string | undefined): value is string {
  return !!value && value.length <= 1024 && value !== "." && value !== ".." && !/[\x00-\x20\x7f]/.test(value);
}
function receipt(value: unknown, binding: string, requestId?: string): Receipt {
  if (!object(value) || typeof value.task_id !== "string" || !UUID.test(value.task_id)
    || (requestId && value.task_id.toLowerCase() !== requestId.toLowerCase())
    || value.binding_id !== binding || typeof value.status !== "string" || !STATES.has(value.status)) {
    throw new Error("Invalid task receipt");
  }
  const result: Receipt = { task_id: value.task_id, binding_id: binding, status: value.status };
  for (const key of ["total_items", "processed_items", "synced_items", "failed_items", "error_count", "progress"]) {
    const n = value[key];
    if (n !== null && (typeof n !== "number" || !Number.isFinite(n) || n < 0
      || (key !== "progress" && !Number.isSafeInteger(n)))) throw new Error("Invalid task counts");
    result[key] = n;
  }
  for (const key of ["started_at", "completed_at"]) {
    if (value[key] !== null && (typeof value[key] !== "string" || !Number.isFinite(Date.parse(value[key])))) {
      throw new Error("Invalid task timestamps");
    }
    result[key] = value[key];
  }
  // Do not relay arbitrary backend error text or unexpected fields.
  result.error_summary = value.status === "failed" ? "Sync failed or was interrupted. Review source status before retrying." : null;
  return result;
}
function safeOrigin(value: string): boolean {
  try {
    const url = new URL(value);
    const host = url.hostname.replace(/^\[|\]$/g, "");
    return !url.username && !url.password && !url.search && !url.hash && url.pathname === "/"
      && (url.protocol === "https:" || (url.protocol === "http:"
        && (host === "::1" || (isIP(host) === 4 && host.startsWith("127.")))));
  } catch { return false; }
}
function notAccepted(error: unknown): boolean {
  if (!(error instanceof GatewayHttpError)) return false;
  try {
    const body = JSON.parse(error.body);
    return object(body?.detail) && body.detail.code === "CONFLUENCE_SYNC_NOT_ACCEPTED" && body.detail.accepted === false;
  } catch { return false; }
}

export async function runKnowledgeCli(argv: string[], dependencies: Dependencies = {}): Promise<number> {
  const out = dependencies.stdout ?? process.stdout;
  const err = dependencies.stderr ?? process.stderr;
  if (argv.length === 0 || (argv.length === 1 && ["--help", "help"].includes(argv[0]!))) {
    out.write(HELP); return 0;
  }
  const emit = (value: unknown) => out.write(`${JSON.stringify(value)}\n`);
  const [command, dataset, ...rest] = argv;
  let binding: string | undefined;
  let requestId: string | undefined;
  let baseUrl: string | undefined;
  let confirm = false;
  try {
    if (!["sources", "sync", "status"].includes(command!) || !identity(dataset)) throw new Error();
    if (command !== "sources") { binding = rest.shift(); if (!identity(binding)) throw new Error(); }
    const seen = new Set<string>();
    while (rest.length) {
      const flag = rest.shift()!;
      if (seen.has(flag)) throw new Error();
      seen.add(flag);
      if (flag === "--confirm" && command === "sync") confirm = true;
      else if (flag === "--request-id" && command !== "sources") requestId = rest.shift();
      else if (flag === "--base-url") baseUrl = rest.shift();
      else throw new Error();
    }
    if (command !== "sources" && (!requestId || !UUID.test(requestId))) throw new Error();
    if (seen.has("--base-url") && !baseUrl) throw new Error();
    if (command === "sync" && !confirm) throw new Error();
  } catch {
    err.write(`Invalid Knowledge command or missing explicit sync confirmation.\n${HELP}`); return 2;
  }
  let config: CLIConfig;
  try {
    config = (dependencies.loadConfig ?? loadConfig)();
    if (baseUrl) config = { ...config, base_url: baseUrl };
    if (!config.api_key || /[\r\n]/.test(config.api_key) || !safeOrigin(config.base_url)) throw new Error();
  } catch {
    err.write("Knowledge requires an existing Gateway API key and an HTTPS origin (or explicit loopback IP over HTTP). Check ~/.ai-gateway/config.json; credentials in URLs are rejected.\n"); return 2;
  }
  const client = new GatewayClient(config);
  const path = `/api/v1/knowledge/${encodeURIComponent(dataset!)}/sources`;
  const syncPath = `${path}/confluence/${encodeURIComponent(binding ?? "")}/sync`;
  // Single quoting is literal in POSIX shells and PowerShell. Double apostrophes
  // are PowerShell-specific, so unusual apostrophes use readback placeholders.
  const quote = (value: string) => value.includes("'") ? "<copy-target-from-your-original-command>" : `'${value}'`;
  const next = `ai-gateway gateway knowledge status ${quote(dataset!)} ${quote(binding ?? "")} --request-id ${requestId}${baseUrl ? ` --base-url ${quote(baseUrl)}` : ""}`;
  const controller = new AbortController();
  const signalSource = dependencies.signalSource ?? process;
  const interrupt = () => controller.abort();
  signalSource.on("SIGINT", interrupt);
  signalSource.on("SIGTERM", interrupt);
  const options = { signal: controller.signal, boundedJson: true, timeoutMs: Math.min(300, Math.max(1, Number.isFinite(config.timeout) ? config.timeout : 30)) * 1000 };
  let submitted = false;
  try {
    if (command === "sources" || command === "sync") {
      const sources = await client.request("GET", path, undefined, options);
      if (!object(sources) || !Array.isArray(sources.confluence_bindings)
        || !["available", "unavailable", "not_configured"].includes(String(sources.connector_status))) throw new Error();
      const bindings = sources.confluence_bindings.map((item: unknown) => {
        if (!object(item) || typeof item.binding_id !== "string" || !identity(item.binding_id)) throw new Error();
        return { binding_id: item.binding_id, manual_sync_available: item.manual_sync_available === true,
          current_sync_task: item.current_sync_task == null ? null : receipt(item.current_sync_task, item.binding_id) };
      });
      if (command === "sources") {
        emit({ connector_status: sources.connector_status, confluence_bindings: bindings });
        err.write("Saved bindings are not proof of scheduled synchronization. Choose an eligible binding for explicit sync; current_sync_task can be read using status.\n");
        return sources.connector_status === "unavailable" ? 1 : 0;
      }
      const selected = bindings.find((item) => item.binding_id === binding);
      if (sources.connector_status !== "available" || !selected?.manual_sync_available) {
        emit({ outcome: "not_submitted", request_id: requestId });
        err.write("Manual sync is unavailable for this binding. Check saved connection status and current editor/source-owner access, then list sources again. No sync submitted.\n"); return 1;
      }
      if (selected.current_sync_task && ["pending", "processing"].includes(selected.current_sync_task.status)) {
        emit({ outcome: "not_submitted", current_sync_task: selected.current_sync_task });
        err.write(`An existing task is active. Use status with request ID ${selected.current_sync_task.task_id}; no sync submitted.\n`); return 1;
      }
    }
    if (command === "sync") {
      err.write(`Submitting request ${requestId}. Full reconciliation can remove source-deleted documents; other binding references preserve shared documents.\nRead back exactly: ${next}\n`);
      submitted = true;
    }
    const value = command === "sync"
      ? await client.request("POST", syncPath, { request_id: requestId }, options)
      : await client.request("GET", `${syncPath}/${requestId}`, undefined, options);
    const task = receipt(value, binding!, requestId);
    emit({ outcome: task.status, task });
    err.write(`Task ${task.task_id}: ${task.status}. Attempted: ${task.processed_items ?? "unknown"}; applied: ${task.synced_items ?? "unknown"}; failed: ${task.failed_items ?? "unknown"}.\n`);
    if (["pending", "processing"].includes(task.status)) err.write(`Accepted, not completed. Read back exactly: ${next}\n`);
    if (["failed", "cancelled"].includes(task.status)) {
      err.write("Partial changes may already be applied; this is not rollback. Review Sources/connection/access before explicitly starting a new sync with a new retained UUID.\n"); return 1;
    }
    return 0;
  } catch (error) {
    if (submitted && notAccepted(error)) {
      emit({ outcome: "not_accepted", request_id: requestId });
      err.write("The server explicitly rejected this request before admission. Refresh sources and repair access/connection prerequisites before another explicit sync.\n"); return 1;
    }
    if (submitted || command === "status") {
      emit({ outcome: "uncertain", request_id: requestId });
      err.write(`Task outcome is uncertain. Keep this UUID; do not submit another sync. A read error or HTTP 404 does not prove non-admission. Check access and connection, then read back exactly: ${next}\n`); return 3;
    }
    emit({ outcome: "read_failed" });
    err.write("Could not read valid Sources. Check Gateway connection, saved credentials and dataset access. No sync submitted.\n"); return 1;
  } finally {
    signalSource.off("SIGINT", interrupt);
    signalSource.off("SIGTERM", interrupt);
  }
}
