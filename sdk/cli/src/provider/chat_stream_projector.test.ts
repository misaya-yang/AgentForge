import { EventEmitter } from "node:events";
import type { ServerResponse } from "node:http";
import { describe, expect, it } from "vitest";

import { projectChatStream } from "./chat_stream_projector.js";

interface ProjectedEvent {
  type: string;
  delta?: string;
  sequence_number: number;
  response?: { status: string; error: { code: string; message: string } | null };
}

class Consumer extends EventEmitter {
  destroyed = false;
  writableEnded = false;
  events: ProjectedEvent[] = [];

  write(frame: string): boolean {
    this.events.push(JSON.parse(frame.split("\ndata: ")[1]!) as ProjectedEvent);
    return true;
  }

  end() { this.writableEnded = true; }
  destroy() { this.destroyed = true; }
}

const frame = (payload: unknown) => `data: ${JSON.stringify(payload)}\n\n`;
const text = (content: string) => frame({ choices: [{ delta: { content } }] });
const done = "data: [DONE]\n\n";
const finish = frame({ choices: [{ delta: {}, finish_reason: "stop" }] });
const privateDetail = "synthetic-provider-key-and-private-request-body";
const error = frame({ error: { message: privateDetail, type: "server_error", code: privateDetail } });

function upstream(chunks: string[]) {
  const state = { reads: 0, cancelled: 0 };
  const encoder = new TextEncoder();
  const response = new Response(new ReadableStream<Uint8Array>({
    pull(controller) {
      const chunk = chunks[state.reads++];
      if (chunk === undefined) controller.close();
      else controller.enqueue(encoder.encode(chunk));
    },
    cancel() { state.cancelled++; },
  }, { highWaterMark: 0 }));
  return { response, state };
}

async function project(chunks: string[]) {
  const { response, state } = upstream(chunks);
  const sink = new Consumer();
  await projectChatStream(response, sink as unknown as ServerResponse, "fixture", 1000,
    new AbortController().signal, new Map(), new Set(["lookup"]));
  return { sink, response, state };
}

function expectSingleFailure(sink: Consumer) {
  const terminals = sink.events.filter(event => ["response.failed", "response.completed"].includes(event.type));
  expect(terminals).toHaveLength(1);
  expect(terminals[0]).toMatchObject({
    type: "response.failed",
    response: { status: "failed", error: { code: "provider_stream_error", message: "provider_stream_error" } },
  });
  expect(sink.events.some(event => ["response.output_item.done", "response.function_call_arguments.done", "response.output_text.done"].includes(event.type))).toBe(false);
  expect(sink.events.map(event => event.sequence_number)).toEqual(sink.events.map((_, index) => index));
  expect(JSON.stringify(sink.events)).not.toContain(privateDetail);
  expect(sink.writableEnded).toBe(true);
  expect(sink.destroyed).toBe(false);
  expect(sink.listenerCount("close")).toBe(0);
}

describe("Chat provider stream error terminal", () => {
  it.each([
    { name: "before any output", prefix: [] },
    { name: "after partial text", prefix: [text("partial")] },
    { name: "after the provider finish reason", prefix: [text("partial"), finish] },
    { name: "after function arguments", prefix: [frame({ choices: [{ delta: {
      tool_calls: [{ index: 0, id: "call_1", function: { name: "lookup", arguments: "{}" } }],
    }, finish_reason: "tool_calls" }] })] },
  ])("fails $name without consuming the later success marker", async ({ prefix }) => {
    const { sink, response, state } = await project([...prefix, error, done, text("unread")]);
    expectSingleFailure(sink);
    expect(state.reads).toBe(prefix.length + 1);
    expect(state.cancelled).toBe(1);
    expect(response.body!.locked).toBe(false);
    expect(sink.events.filter(event => event.type === "response.output_text.delta").map(event => event.delta)).toEqual(
      prefix.some(value => value === text("partial")) ? ["partial"] : [],
    );
  });

  it("fails an error frame at EOF with its explicit error code rather than incomplete", async () => {
    const { sink, response, state } = await project([error]);
    expectSingleFailure(sink);
    expect(state.reads).toBe(1);
    expect(state.cancelled).toBe(1);
    expect(response.body!.locked).toBe(false);
  });

  it("fails a coalesced error and success marker without emitting later content", async () => {
    const { sink, state } = await project([text("partial") + error + text(privateDetail) + finish + done]);
    expectSingleFailure(sink);
    expect(sink.events.filter(event => event.type === "response.output_text.delta").map(event => event.delta)).toEqual(["partial"]);
    expect(state.reads).toBe(1);
    expect(state.cancelled).toBe(1);
  });

  it("recognizes an error frame split across transport chunks", async () => {
    const { sink, state } = await project([error.slice(0, 9), error.slice(9, 30), error.slice(30), done]);
    expectSingleFailure(sink);
    expect(state.reads).toBe(3);
    expect(state.cancelled).toBe(1);
  });

  it.each([privateDetail, {}, [], false, 0])("fails closed for a non-null error value %j", async (value) => {
    const { sink, state } = await project([frame({ error: value, choices: [{ delta: { content: privateDetail }, finish_reason: "stop" }] }), done]);
    expectSingleFailure(sink);
    expect(sink.events.some(event => event.type === "response.output_text.delta")).toBe(false);
    expect(state.reads).toBe(1);
    expect(state.cancelled).toBe(1);
  });

  it.each([
    { name: "ordinary text mentioning error", chunks: [text("error is just text"), finish, done] },
    { name: "null error extension", chunks: [frame({ error: null, choices: [{ delta: { content: "ok" }, finish_reason: "stop" }] }), done] },
    { name: "usage-only frame", chunks: [text("ok"), finish, frame({ choices: [], usage: { prompt_tokens: 2, completion_tokens: 1, total_tokens: 3 } }), done] },
    { name: "empty successful completion", chunks: [finish, done] },
  ])("preserves $name", async ({ chunks }) => {
    const { sink, response, state } = await project(chunks);
    expect(sink.events.filter(event => ["response.completed", "response.failed"].includes(event.type))).toMatchObject([
      { type: "response.completed", response: { status: "completed", error: null } },
    ]);
    expect(sink.writableEnded).toBe(true);
    expect(state.cancelled).toBe(1);
    expect(response.body!.locked).toBe(false);
  });
});
