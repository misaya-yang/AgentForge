import assert from "node:assert/strict";
import { test } from "node:test";

import {
  clearLastModelId,
  readHydratedLastModelId,
  readLastModelId,
  writeLastModelId,
} from "./lastModel.ts";

class MemoryStorage {
  private items = new Map<string, string>();

  getItem(key: string): string | null {
    return this.items.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    this.items.set(key, value);
  }

  removeItem(key: string): void {
    this.items.delete(key);
  }

  key(index: number): string | null {
    return [...this.items.keys()][index] ?? null;
  }

  get length(): number {
    return this.items.size;
  }
}

function installStorage(storage: MemoryStorage): void {
  (globalThis as { window?: unknown }).window = {
    localStorage: storage,
  };
}

test("last model cache round-trips through storage", () => {
  installStorage(new MemoryStorage());

  assert.equal(readLastModelId(), "");

  writeLastModelId("qwen3.7-plus");
  assert.equal(readLastModelId(), "qwen3.7-plus");

  writeLastModelId("gpt-4.1");
  assert.equal(readLastModelId(), "gpt-4.1");
});

test("last model cache refuses to write empty ids", () => {
  const storage = new MemoryStorage();
  installStorage(storage);

  writeLastModelId("qwen3.7-plus");
  writeLastModelId("");
  assert.equal(readLastModelId(), "qwen3.7-plus");
});

test("last model cache scopes keys by user and can clear them", () => {
  installStorage(new MemoryStorage());
  writeLastModelId("model-a", "user-a");
  writeLastModelId("model-b", "user-b");
  assert.equal(readLastModelId("user-a"), "model-a");
  assert.equal(readLastModelId("user-b"), "model-b");
  clearLastModelId();
  assert.equal(readLastModelId("user-a"), "");
  assert.equal(readLastModelId("user-b"), "");
});

test("legacy last model is migrated once and removed from the shared key", () => {
  const storage = new MemoryStorage();
  installStorage(storage);
  storage.setItem("assistant.lastModelId.v2", "legacy-model");

  assert.equal(readLastModelId("user-a"), "legacy-model");
  assert.equal(storage.getItem("assistant.lastModelId.v2"), null);
  assert.equal(storage.getItem("assistant.lastModelId.v2:user-a"), "legacy-model");
  assert.equal(readLastModelId("user-b"), "");
});

test("legacy cache is not consumed before auth hydration", () => {
  const storage = new MemoryStorage();
  installStorage(storage);
  storage.setItem("assistant.lastModelId.v2", "legacy-model");

  assert.equal(readHydratedLastModelId(false, "user-a"), "");
  assert.equal(storage.getItem("assistant.lastModelId.v2"), "legacy-model");
  assert.equal(readHydratedLastModelId(true, "user-a"), "legacy-model");
  assert.equal(storage.getItem("assistant.lastModelId.v2"), null);
});

test("last model cache degrades to empty when storage throws", () => {
  (globalThis as { window?: unknown }).window = {
    get localStorage() {
      throw new Error("storage unavailable");
    },
  };

  assert.equal(readLastModelId(), "");
  writeLastModelId("qwen3.7-plus"); // must not throw
});
