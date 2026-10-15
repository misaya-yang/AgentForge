import assert from "node:assert/strict";
import { test } from "node:test";

import {
  readSetupBannerDismissed,
  writeSetupBannerDismissed,
} from "./setupBannerStorage.ts";

class MemoryStorage {
  private items = new Map<string, string>();

  getItem(key: string): string | null {
    return this.items.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    this.items.set(key, value);
  }
}

test("setup banner dismissal is isolated by user", () => {
  (globalThis as { localStorage?: unknown }).localStorage = new MemoryStorage();

  assert.equal(readSetupBannerDismissed("user-a"), false);
  assert.equal(writeSetupBannerDismissed("user-a"), true);
  assert.equal(readSetupBannerDismissed("user-a"), true);
  assert.equal(readSetupBannerDismissed("user-b"), false);
});

test("setup banner storage failure stays fail-open", () => {
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    get() {
      throw new Error("storage unavailable");
    },
  });

  assert.equal(readSetupBannerDismissed("user-a"), false);
  assert.equal(writeSetupBannerDismissed("user-a"), false);
});
