import assert from "node:assert/strict";
import test from "node:test";

import {
  ASSISTANT_COMPACT_MAX_WIDTH,
  ASSISTANT_COMPACT_MEDIA_QUERY,
  isAssistantCompactWidth,
} from "./layout.ts";

test("assistant switches side panels to sheets before the chat lane is squeezed", () => {
  assert.equal(isAssistantCompactWidth(812), true);
  assert.equal(isAssistantCompactWidth(ASSISTANT_COMPACT_MAX_WIDTH), true);
  assert.equal(isAssistantCompactWidth(ASSISTANT_COMPACT_MAX_WIDTH + 1), false);
  assert.equal(
    ASSISTANT_COMPACT_MEDIA_QUERY,
    `(max-width: ${ASSISTANT_COMPACT_MAX_WIDTH}px)`,
  );
});
