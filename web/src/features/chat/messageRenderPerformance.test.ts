import assert from "node:assert/strict";
import { test } from "node:test";

import {
  messageContainmentStyle,
  updateMessageById,
} from "./messageRenderPerformance.ts";

test("200-message history keeps old items referentially stable during a 20k stream", () => {
  const initial = Array.from({ length: 200 }, (_, index) => ({
    id: `message-${index}`,
    content: "history",
  }));
  let messages = initial;
  const chunk = "x".repeat(100);

  for (let index = 0; index < 200; index += 1) {
    messages = updateMessageById(messages, "message-199", (message) => ({
      ...message,
      content: message.content === "history" ? chunk : `${message.content}${chunk}`,
    }));
  }

  assert.equal(messages[199].content.length, 20_000);
  for (let index = 0; index < 199; index += 1) {
    assert.equal(messages[index], initial[index]);
    assert.equal(messageContainmentStyle(false)?.contentVisibility, "auto");
  }
  assert.equal(messageContainmentStyle(true), undefined);
});
