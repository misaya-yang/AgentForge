import { EventEmitter } from "node:events";
import { afterEach, describe, expect, it, vi } from "vitest";
import { runKnowledgeCli } from "./knowledge_cli.js";
import * as configModule from "../config.js";
import { DEFAULT_CONFIG } from "../types/config.js";

const id = "11111111-1111-4111-8111-111111111111";
const other = "22222222-2222-4222-8222-222222222222";
const task = (status = "pending", extra = {}) => ({ task_id: id, binding_id: "binding", status,
  progress: null, total_items: null, processed_items: 0, synced_items: null, failed_items: null,
  error_count: null, started_at: null, completed_at: null, error_summary: null, ...extra });
const sources = (extra = {}) => ({ connector_status: "available", confluence_bindings: [
  { binding_id: "binding", manual_sync_available: true, current_sync_task: null, ...extra },
] });
const args = (command = "sync") => [command, "dataset", "binding", "--request-id", id, ...(command === "sync" ? ["--confirm"] : [])];
function setup(responses: (Response | Error)[], config = {}) {
  let stdout = "", stderr = "";
  const fetch = vi.fn(async () => {
    const response = responses.shift();
    if (response instanceof Error) throw response;
    if (!response) throw new Error("Unexpected extra request");
    return response;
  });
  vi.stubGlobal("fetch", fetch);
  return { fetch, run: (argv: string[]) => runKnowledgeCli(argv, {
    loadConfig: () => ({ ...DEFAULT_CONFIG, api_key: "synthetic-secret", base_url: "https://gateway.example", ...config }),
    stdout: { write: (value: string) => { stdout += value; return true; } },
    stderr: { write: (value: string) => { stderr += value; return true; } },
  }), output: () => ({ stdout, stderr }), json: () => JSON.parse(stdout) };
}
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });
afterEach(() => { vi.unstubAllGlobals(); vi.useRealTimers(); });

describe("Knowledge CLI production parser, transport and output", () => {
  it("dispatches the actual executable entrypoint into Knowledge without the native Runtime or chat UI", async () => {
    const h = setup([json(sources())]);
    const config = vi.spyOn(configModule, "loadConfig").mockReturnValue({ ...DEFAULT_CONFIG, api_key: "synthetic-secret", base_url: "https://gateway.example" });
    const output = vi.spyOn(process.stdout, "write").mockReturnValue(true);
    const error = vi.spyOn(process.stderr, "write").mockReturnValue(true);
    const previousArgv = process.argv, previousExit = process.exitCode;
    try {
      process.argv = ["node", "ai-gateway", "gateway", "knowledge", "sources", "dataset"];
      await import("../cli.js");
      expect(process.exitCode).toBe(0);
      expect(h.fetch).toHaveBeenCalledTimes(1);
      expect(JSON.parse(String(output.mock.calls[0]![0])).connector_status).toBe("available");
    } finally {
      process.argv = previousArgv; process.exitCode = previousExit;
      output.mockRestore(); error.mockRestore(); config.mockRestore();
    }
  });
  it.each(["SIGINT", "SIGTERM"])("interrupting an admitted POST with %s preserves exact uncertain recovery", async (signal) => {
    const events = new EventEmitter();
    let stdout = "", stderr = "";
    const fetch = vi.fn(async (_url: unknown, options: RequestInit) => {
      if (options.method === "GET") return json(sources());
      return new Promise<Response>((_resolve, reject) => {
        options.signal!.addEventListener("abort", () => reject(new Error("interrupted")), { once: true });
        events.emit(signal);
      });
    });
    vi.stubGlobal("fetch", fetch);
    expect(await runKnowledgeCli(args(), {
      loadConfig: () => ({ ...DEFAULT_CONFIG, api_key: "synthetic-secret", base_url: "https://gateway.example" }),
      signalSource: events,
      stdout: { write: value => { stdout += value; return true; } },
      stderr: { write: value => { stderr += value; return true; } },
    })).toBe(3);
    expect(JSON.parse(stdout)).toEqual({ outcome: "uncertain", request_id: id });
    expect(stderr).toContain(`--request-id ${id}`);
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(events.listenerCount(signal)).toBe(0);
  });
  it.each(["headers", "body"])("bounds a stalled exact status %s read and removes signal listeners", async (phase) => {
    vi.useFakeTimers();
    const events = new EventEmitter();
    let stdout = "";
    vi.stubGlobal("fetch", vi.fn(async (_url: unknown, options: RequestInit) => {
      if (phase === "headers") return new Promise<Response>((_resolve, reject) => {
        options.signal!.addEventListener("abort", () => reject(new Error("timeout")), { once: true });
      });
      return new Response(new ReadableStream({ start(controller) {
        controller.enqueue(new TextEncoder().encode('{"task_id":'));
        options.signal!.addEventListener("abort", () => controller.error(new Error("timeout")), { once: true });
      } }));
    }));
    const result = runKnowledgeCli(args("status"), {
      loadConfig: () => ({ ...DEFAULT_CONFIG, timeout: 1, api_key: "synthetic-secret", base_url: "https://gateway.example" }),
      signalSource: events,
      stdout: { write: value => { stdout += value; return true; } },
      stderr: { write: () => true },
    });
    await vi.advanceTimersByTimeAsync(1001);
    expect(await result).toBe(3);
    expect(JSON.parse(stdout)).toEqual({ outcome: "uncertain", request_id: id });
    expect(events.listenerCount("SIGINT")).toBe(0);
    expect(events.listenerCount("SIGTERM")).toBe(0);
    expect(vi.getTimerCount()).toBe(0);
  });
  it("runs saved binding list -> explicit sync -> exact completed readback", async () => {
    const h = setup([json(sources())]);
    expect(await h.run(["sources", "dataset"])).toBe(0);
    expect(h.json().confluence_bindings[0].manual_sync_available).toBe(true);
    const start = setup([json(sources()), json(task(), 202)]);
    expect(await start.run(args())).toBe(0);
    const [url, options] = start.fetch.mock.calls[1] as unknown as [string, RequestInit];
    expect(url).toBe("https://gateway.example/api/v1/knowledge/dataset/sources/confluence/binding/sync");
    expect(options.method).toBe("POST");
    expect(options.body).toBe(JSON.stringify({ request_id: id }));
    expect(options.redirect).toBe("error");
    expect(options.headers).toMatchObject({ "X-API-Key": "synthetic-secret" });
    expect(start.output().stderr).toContain("Accepted, not completed");
    const finish = setup([json(task("completed", { processed_items: 3, synced_items: 2 }))]);
    expect(await finish.run(args("status"))).toBe(0);
    expect((finish.fetch.mock.calls[0] as unknown as [string])[0].endsWith(`/sync/${id}`)).toBe(true);
    expect(finish.json().task.synced_items).toBe(2);
    expect(finish.output().stderr).toContain("Attempted: 3; applied: 2");
  });
  it.each([[], ["--request-id", id], ["--confirm"], ["--request-id", "wrong", "--confirm"],
    ["--request-id", id, "--confirm", "--confirm"], ["--request-id", id, "--confirm", "--unknown"]].map(flags => ({ flags })) )("rejects incomplete or ambiguous sync options %j", async ({ flags }) => {
    const h = setup([]);
    expect(await h.run(["sync", "dataset", "binding", ...flags])).toBe(2);
    expect(h.fetch).not.toHaveBeenCalled();
  });
  it.each([false, undefined])("does not mutate when capability is %j", async (manual_sync_available) => {
    const h = setup([json(sources({ manual_sync_available }))]);
    expect(await h.run(args())).toBe(1);
    expect(h.fetch).toHaveBeenCalledTimes(1);
    expect(h.json().outcome).toBe("not_submitted");
  });
  it("blocks a new sync while the latest exact task remains active", async () => {
    const h = setup([json(sources({ current_sync_task: task("processing", { task_id: other }) }))]);
    expect(await h.run(args())).toBe(1);
    expect(h.fetch).toHaveBeenCalledTimes(1);
    expect(h.output().stderr).toContain(other);
  });
  it.each([new Error("synthetic-secret provider body"), json({ detail: "synthetic-secret" }, 503),
    json(task("pending", { task_id: other }), 202), new Response("malformed", { status: 202 })])("retains uncertain POST identity without retry or secret output", async (failure) => {
    const h = setup([json(sources()), failure]);
    expect(await h.run(args())).toBe(3);
    expect(h.fetch).toHaveBeenCalledTimes(2);
    expect(h.json()).toEqual({ outcome: "uncertain", request_id: id });
    expect(h.output().stderr).toContain(`--request-id ${id}`);
    expect(JSON.stringify(h.output())).not.toContain("synthetic-secret");
  });
  it("recognizes only explicit POST pre-admission rejection", async () => {
    const h = setup([json(sources()), json({ detail: { code: "CONFLUENCE_SYNC_NOT_ACCEPTED", accepted: false, message: "synthetic-secret" } }, 409)]);
    expect(await h.run(args())).toBe(1);
    expect(h.json().outcome).toBe("not_accepted");
    expect(h.output().stderr).not.toContain("synthetic-secret");
  });
  it.each([404, 403, 503])("GET %i remains inconclusive even with a rejection marker", async (status) => {
    const h = setup([json({ detail: { code: "CONFLUENCE_SYNC_NOT_ACCEPTED", accepted: false } }, status)]);
    expect(await h.run(args("status"))).toBe(3);
    expect(h.fetch).toHaveBeenCalledTimes(1);
    expect(h.json().outcome).toBe("uncertain");
  });
  it.each(["failed", "cancelled"])("shows %s as unsuccessful partial execution", async (status) => {
    const h = setup([json(task(status, { processed_items: 5, synced_items: 2, failed_items: 3, error_summary: "synthetic-secret" }))]);
    expect(await h.run(args("status"))).toBe(1);
    expect(h.json().task.synced_items).toBe(2);
    expect(h.output().stderr).toContain("Partial changes may already be applied");
    expect(h.output().stdout).not.toContain("synthetic-secret");
  });
  it.each(["https://user:synthetic-secret@gateway.example", "http://gateway.example", "https://gateway.example?token=synthetic-secret"])("rejects unsafe origin without displaying it", async (base_url) => {
    const h = setup([], { base_url });
    expect(await h.run(args())).toBe(2);
    expect(h.fetch).not.toHaveBeenCalled();
    expect(h.output().stderr).not.toContain("synthetic-secret");
  });
  it("bounds successful response bodies and fails uncertain on oversized exact readback", async () => {
    const h = setup([new Response('"' + "x".repeat(8 * 1024 * 1024) + '"')]);
    expect(await h.run(args("status"))).toBe(3);
    expect(h.fetch).toHaveBeenCalledTimes(1);
  });
  it.each([{ task_id: other }, { binding_id: "other" }, { status: "unknown" }, { processed_items: -1 }, { started_at: "invalid" }])("rejects invalid exact receipt %j", async (change) => {
    const h = setup([json(task("completed", change))]);
    expect(await h.run(args("status"))).toBe(3);
    expect(h.json().task).toBeUndefined();
  });
});
