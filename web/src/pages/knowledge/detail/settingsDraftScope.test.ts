// @ts-expect-error -- node built-ins are supplied by the node --test runtime.
import assert from "node:assert/strict";
// @ts-expect-error -- node built-ins are supplied by the node --test runtime.
import { test } from "node:test";

import { currentSettingsDraftKey, shouldLoadSettingsConfig } from "./settingsDraftScope.ts";

test("settings draft cannot cross from dataset A into B during route reuse", () => {
  assert.equal(currentSettingsDraftKey("user", "A", "A", true), "kb-settings:user:A");
  assert.equal(currentSettingsDraftKey("user", "B", "A", true), null);
  assert.equal(currentSettingsDraftKey("user", "B", "B", false), null);
  assert.equal(currentSettingsDraftKey("user", "B", "B", true), "kb-settings:user:B");
  assert.equal(currentSettingsDraftKey(undefined, "B", "B", true), null);
});

test("failed config load waits while active and retries on the next tab visit", () => {
  assert.equal(shouldLoadSettingsConfig("A", null, null, true, false), true);
  assert.equal(shouldLoadSettingsConfig("A", null, "A", true, true), false);
  assert.equal(shouldLoadSettingsConfig("A", null, "A", false, true), false);
  assert.equal(shouldLoadSettingsConfig("A", null, "A", true, false), true);
  assert.equal(shouldLoadSettingsConfig("B", "A", "A", true, true), true);
});
