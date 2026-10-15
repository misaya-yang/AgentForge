import { EventEmitter } from "node:events";
import type { ServerResponse } from "node:http";
import { describe, expect, it } from "vitest";

import { projectChatStream } from "./chat_stream_projector.js";

interface StreamEvent {
  type: string;
  delta?: string;
  response?: { error?: { code: string }; status?: string };
}

class Consumer extends EventEmitter {
  destroyed = false;
  writableEnded = false;
  events: StreamEvent[] = [];
  private blocked = false;
  private signalBlocked!: () => void;
  readonly waitingForDrain = new Promise<void>((resolve) => { this.signalBlocked = resolve; });

  constructor(private blockType?: string) { super(); }

  write(frame: string): boolean {
    const event = JSON.parse(frame.split("\ndata: ")[1]!) as StreamEvent;
    this.events.push(event);
    if (event.type === this.blockType && !this.blocked) {
      this.blocked = true;
      this.signalBlocked();
      return false;
    }
    return true;
  }

  end() { this.writableEnded = true; }
  destroy() { this.destroyed = true; }
  disconnect() { this.destroyed = true; this.emit("close"); }
}

function upstream(chunks: string[]) {
  const state = { reads: 0, cancelled: 0 };
  const encoder = new TextEncoder();
  const response = new Response(new ReadableStream<Uint8Array>({
    pull(controller) {
      const next = chunks[state.reads++];
      if (next === undefined) controller.close();
      else controller.enqueue(encoder.encode(next));
    },
    cancel() { state.cancelled++; },
  }, { highWaterMark: 0 }));
  return { response, state };
}

const frame = (delta: Record<string, unknown>) => `data: ${JSON.stringify({ choices: [{ delta }] })}\n\n`;
const done = "data: [DONE]\n\n";

function project(response: Response, sink: Consumer) {
  return projectChatStream(response, sink as unknown as ServerResponse, "fixture", 1000, new AbortController().signal, new Map(), new Set(["lookup"]));
}

function expectSingleFailure(sink: Consumer, code: string) {
  const terminal = sink.events.filter(event => ["response.failed", "response.completed"].includes(event.type));
  expect(terminal.length).toBe(1);
  expect(terminal[0]?.type).toBe("response.failed");
  expect(terminal[0]?.response?.error?.code).toBe(code);
  expect(sink.writableEnded).toBe(true);
}

describe("bounded Chat projection", () => {
  it("cancels a provider with more than 1 MiB of unseparated frame data", async () => {
    const { response, state } = upstream(["data: " + "x".repeat(1024 * 1024), done]);
    const sink = new Consumer();
    await project(response, sink);
    expectSingleFailure(sink, "provider_frame_limit");
    expect(state.reads).toBe(1);
    expect(state.cancelled).toBe(1);
    expect(response.body!.locked).toBe(false);
  });

  it.each([
    ["content", "provider_text_limit"],
    ["reasoning_content", "provider_reasoning_limit"],
  ])("bounds accumulated %s across individually valid frames", async (field, code) => {
    const delta = "x".repeat(512 * 1024);
    const { response, state } = upstream([...Array.from({ length: 9 }, () => frame({ [field]: delta })), done]);
    const sink = new Consumer();
    await project(response, sink);
    expectSingleFailure(sink, code!);
    expect(state.cancelled).toBe(1);
    expect(state.reads).toBe(9);
  });

  it.each([
    { name: "one tool", toolCount: 1, parts: 3, code: "provider_tool_arguments_limit" },
    { name: "all tools", toolCount: 5, parts: 2, code: "provider_tool_arguments_total_limit" },
  ])("bounds accumulated arguments for $name without a second terminal", async ({ toolCount, parts, code }) => {
    const delta = "x".repeat(512 * 1024);
    const chunks = Array.from({ length: toolCount }, (_, index) => Array.from({ length: parts }, (_, part) => frame({
      tool_calls: [{ index, id: `call_${index}`, function: { ...(part === 0 ? { name: "lookup" } : {}), arguments: delta } }],
    }))).flat();
    const { response, state } = upstream([...chunks, done]);
    const sink = new Consumer();
    await project(response, sink);
    expectSingleFailure(sink, code);
    expect(state.cancelled).toBe(1);
    expect(state.reads).toBeLessThanOrEqual(chunks.length);
  });

  it("does not read the next provider chunk until a slow consumer drains", async () => {
    const { response, state } = upstream([frame({ content: "first" }), frame({ content: "second" }), done]);
    const sink = new Consumer("response.output_text.delta");
    const running = project(response, sink);
    await sink.waitingForDrain;
    await new Promise<void>((resolve) => setImmediate(resolve));
    expect(state.reads).toBe(1);
    expect(sink.events.filter(event => event.type === "response.output_text.delta").length).toBe(1);
    sink.emit("drain");
    await running;
    expect(sink.events.filter(event => event.type === "response.output_text.delta").map(event => event.delta)).toEqual(["first", "second"]);
    expect(sink.events.filter(event => event.type === "response.completed").length).toBe(1);
    expect(state.cancelled).toBe(1);
  });

  it("accepts coalesced valid frames larger than 2 MiB without treating a chunk as a frame", async () => {
    const delta = "汉".repeat(16 * 1024);
    const { response, state } = upstream([Array.from({ length: 50 }, () => frame({ content: delta })).join("") + done]);
    const sink = new Consumer();
    await project(response, sink);
    expect(sink.events.filter(event => event.type === "response.output_text.delta").map(event => event.delta).join("")).toBe(delta.repeat(50));
    expect(sink.events.filter(event => event.type === "response.completed")).toHaveLength(1);
    expect(sink.events.some(event => event.type === "response.failed")).toBe(false);
    expect(state.cancelled).toBe(1);
  });

  it("disconnects during backpressure without waiting for drain and cancels upstream", async () => {
    const { response, state } = upstream([frame({ content: "first" }), frame({ content: "unread" }), done]);
    const sink = new Consumer("response.output_text.delta");
    const running = project(response, sink);
    await sink.waitingForDrain;
    sink.disconnect();
    await running;
    expect(state.reads).toBe(1);
    expect(state.cancelled).toBe(1);
    expect(response.body!.locked).toBe(false);
    expect(sink.listenerCount("drain")).toBe(0);
    expect(sink.events.filter(event => ["response.completed", "response.failed"].includes(event.type)).length).toBe(0);
  });
});
