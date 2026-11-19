import assert from "node:assert/strict";
import test from "node:test";

import { needsGfmPlugin } from "./gfmDetection.ts";

test("standard chat markdown does not load the GFM parser", () => {
  assert.equal(needsGfmPlugin("# Summary\n\n- First\n- Second\n\n**Done**"), false);
  assert.equal(needsGfmPlugin("[Documentation](https://example.com/docs)"), false);
});

test("GFM-only syntax requests the parser", () => {
  assert.equal(needsGfmPlugin("A | B\n--- | ---\n1 | 2"), true);
  assert.equal(needsGfmPlugin("- [x] shipped"), true);
  assert.equal(needsGfmPlugin("This is ~~obsolete~~."), true);
  assert.equal(needsGfmPlugin("Visit https://example.com/docs"), true);
  assert.equal(needsGfmPlugin("Contact owner@example.com"), true);
});
