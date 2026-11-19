import assert from "node:assert/strict";
import test from "node:test";

import { capabilityDisplayName } from "./agentCatalogPresentation.ts";

test("capability display name prefers operator-owned title", () => {
  assert.equal(
    capabilityDisplayName("mcp_docgen__generate_document", "Document generation"),
    "Document generation",
  );
});

test("capability display name hides registry transport prefixes", () => {
  assert.equal(
    capabilityDisplayName("mcp_docgen__generate_document"),
    "Generate document",
  );
});
