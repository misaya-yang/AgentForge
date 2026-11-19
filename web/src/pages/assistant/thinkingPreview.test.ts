import assert from "node:assert/strict";
import { test } from "node:test";

import { liveThinkingLabel } from "./thinkingPreview.ts";

test("live thinking label exposes the first streamed reasoning token", () => {
  assert.equal(liveThinkingLabel("正在 验证", "思考中"), "思考中: 正在 验证");
});

test("live thinking label keeps the newest bounded preview", () => {
  assert.equal(liveThinkingLabel("一二三四五", "思考中", 3), "思考中: …三四五");
  assert.equal(liveThinkingLabel(undefined, "思考中"), "思考中");
});
