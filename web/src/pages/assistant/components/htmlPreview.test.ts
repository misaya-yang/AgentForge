import assert from "node:assert/strict";
import test from "node:test";
import { preparePreviewHtml } from "./htmlPreview.ts";

test("HTML parser removes mixed-case handlers without corrupting quoted content", () => {
  const html = preparePreviewHtml('<p title="a > b">A &amp; B</p><img alt="onload=literal" src="data:image/gif;base64,AA==" oNlOaD="evil()">');
  assert.equal(html, '<p title="a > b">A &amp; B</p><img alt="onload=literal" src="data:image/gif;base64,AA==">');
});

test("handlers are removed from template, foreign content and inert noscript descendants", () => {
  const html = preparePreviewHtml('<template><img onerror="evil()"></template><svg onload="evil()"><g onclick="evil()"></g></svg><noscript><img onload="evil()"></noscript>');
  assert.equal(html, '<template><img></template><svg><g></g></svg><noscript><img></noscript>');
});
