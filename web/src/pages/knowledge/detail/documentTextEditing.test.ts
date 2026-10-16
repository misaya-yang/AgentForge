import assert from "node:assert/strict";
import test from "node:test";

import { canEditTextSource } from "./documentTextEditing.ts";

test("only completed active text sources get an editor entry", () => {
  assert.equal(canEditTextSource({ source_type: "text", status: "completed" }), true);
  assert.equal(canEditTextSource({ source_type: "upload", status: "completed" }), false);
  assert.equal(canEditTextSource({ source_type: "url", status: "completed" }), false);
  assert.equal(canEditTextSource({ source_type: "text", status: "processing" }), false);
  assert.equal(canEditTextSource({ source_type: "text", status: "completed", enabled: false }), false);
  assert.equal(canEditTextSource({ source_type: "text", status: "completed", archived: true }), false);
  assert.equal(canEditTextSource({ source_type: "text", status: "completed", metadata: { processing_mode: "hierarchical" } }), false);
  assert.equal(canEditTextSource({ source_type: "text", status: "completed", metadata: { processing_mode: "text_only" } }), true);
});
