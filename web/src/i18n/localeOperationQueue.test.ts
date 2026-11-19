import assert from "node:assert/strict";
import { test } from "node:test";

import { loadForFinalLocale, LocaleOperationQueue } from "./localeOperationQueue.ts";

test("language and namespace operations serialize and recheck the final locale", async () => {
  const queue = new LocaleOperationQueue();
  const order: string[] = [];
  let locale = "en-US";
  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });

  const change = queue.run(async () => {
    order.push("change:start");
    await gate;
    locale = "zh-CN";
    order.push("change:end");
  });
  const namespace = queue.run(async () => {
    await loadForFinalLocale("en-US", () => locale, async (value) => {
      order.push(`load:${value}`);
    });
  });
  release();
  await Promise.all([change, namespace]);

  assert.deepEqual(order, ["change:start", "change:end", "load:en-US", "load:zh-CN"]);
});
