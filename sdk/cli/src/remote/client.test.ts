import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import { isTerminalEvent, parseSSEEvent } from "./client.js";

const fixture = JSON.parse(
  readFileSync(
    resolve(process.cwd(), "../fixtures/sse_inner_envelopes.json"),
    "utf-8",
  ),
);

function parseFixture(name: string) {
  const dataLine = String(fixture[name].sse)
    .split("\n")
    .find((line) => line.startsWith("data:"));
  if (!dataLine) throw new Error(`missing data line for ${name}`);
  return parseSSEEvent(null, [dataLine.slice(5).trimStart()]);
}

describe("shared SSE inner-envelope fixture", () => {
  it("unwraps text_delta data strings", () => {
    const event = parseFixture("text_delta");
    expect(event?.eventType).toBe("text_delta");
    expect(event?.data.content).toBe("Hi");
  });

  it.each(["done", "error", "cancelled", "run_finished", "run_error"])(
    "recognizes %s as terminal",
    (name) => {
      const event = parseFixture(name);
      expect(event?.eventType).toBe(fixture[name].event_type);
      expect(event && isTerminalEvent(event)).toBe(true);
      if (name === "error") expect(event?.data.message).toBe("boom");
      if (name === "cancelled") expect(event?.data.reason).toBe("user_stop");
      if (name === "run_error") expect(event?.data.message).toBe("run failed");
    },
  );

  it.each(["null_data", "number_data", "boolean_data", "array_data"])(
    "wraps %s in value",
    (name) => {
      const event = parseFixture(name);
      expect(event?.data).toEqual({ value: fixture[name].value });
    },
  );

  it("preserves and projects a real V2 event envelope with cursor metadata", () => {
    const event = parseSSEEvent("item", [JSON.stringify({
      schema_version: "agent-event/v2",
      thread_id: "thread-1",
      sequence: 7,
      timestamp: "2026-08-30T18:00:00Z",
      event: {
        id: "event-7",
        turn_id: "turn-1",
        payload: {
          event_type: "text_delta",
          data: { content: "hello" },
        },
      },
    })], "7");
    expect(event).toEqual({
      eventType: "text_delta",
      data: { content: "hello" },
      timestamp: Date.parse("2026-08-30T18:00:00Z") / 1000,
      sequence: 7,
      threadId: "thread-1",
      turnId: "turn-1",
      eventId: "event-7",
    });
  });
});
