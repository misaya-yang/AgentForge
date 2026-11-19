import assert from "node:assert/strict";
import test from "node:test";
import { consoleLoginFence } from "./consoleLoginFence.ts";
function fixture() {
  let identity = { token: null as string | null, csrfToken: null as string | null, isAuthenticated: false, user: null }, generation = 0, mounted = true;
  const callbacks = new Set<() => void>();
  const fence = consoleLoginFence(() => identity, () => generation, listener => { callbacks.add(listener); return () => callbacks.delete(listener); }, () => mounted);
  return { fence, callbacks, replace(token: string | null) { identity = { ...identity, token, isAuthenticated: Boolean(token) }; callbacks.forEach(fn => fn()); }, clear() { generation++; callbacks.forEach(fn => fn()); }, unmount() { mounted = false; } };
}
test("own anonymous 401 remains visible while it cannot authorize a late success", () => { const h = fixture(); h.clear(); assert.equal(h.fence.current(), false); assert.equal(h.fence.ownsFailure(401), true); assert.equal(h.fence.ownsFailure(503), false); });
test("login/logout ABA permanently retires old success and failure callbacks", () => { const h = fixture(); h.replace("session-b"); h.replace(null); h.clear(); assert.equal(h.fence.current(), false); assert.equal(h.fence.ownsFailure(401), false); });
test("unmount and explicit disposal retire the listener", () => { const h = fixture(); h.unmount(); assert.equal(h.fence.current(), false); assert.equal(h.fence.ownsFailure(401), false); h.fence.dispose(); assert.equal(h.callbacks.size, 0); });
