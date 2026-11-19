import assert from "node:assert/strict";
import test from "node:test";
import { enterpriseGrant, registerConsolePasskey, signInWithPasskey } from "./passkeyCeremony.ts";
const grant = () => ({ session_type: "cookie", csrf_token: "a".repeat(64), force_password_change: false, user: { user_id: "u", tenant_id: "t", email: null, display_name: null, department: null, roles: ["user"], permissions: ["console:settings:view"], tier: "normal" }, recovery_codes: Array.from({ length: 10 }, (_, i) => String(i).padStart(32, "0")) });
test("enterprise grant projects cookie authority and checks exact expected tenant/user", () => {
  const projected = enterpriseGrant({ ...grant(), access_token: "NEVER_PROJECT", recovery_codes: ["NEVER_PROJECT"] }, "t", "u");
  assert.equal("access_token" in projected, false); assert.equal("recovery_codes" in projected, false);
  for (const patch of [{ session_type: "bearer" }, { csrf_token: "" }, { user: { ...grant().user, tenant_id: "foreign" } }, { user: { ...grant().user, user_id: "foreign" } }, { user: { ...grant().user, permissions: "*" } }]) assert.throws(() => enterpriseGrant({ ...grant(), ...patch }, "t", "u"));
});
function fixture() {
  const calls: string[] = []; let current = true, retire = "", fail = "";
  const stage = async (name: string, value: unknown) => { calls.push(name); if (retire === name) current = false; if (fail === name) throw new Error("unconfirmed"); return value; };
  return { calls, current: () => current, retireAt(value: string) { retire = value; }, failAt(value: string) { fail = value; },
    client: { start: () => stage("start", { flow_id: "flow", publicKey: {} }) as never, create: () => stage("native-create", {}), assertion: () => stage("native-get", {}), finish: (flow: string) => { assert.equal(flow, "flow"); return stage("finish", grant()); } } };
}
test("registration performs one ceremony and returns recovery codes only after server confirmation", async () => {
  const h = fixture(), receipt = await registerConsolePasskey(h.client, h.current, "t", "u");
  assert.deepEqual(h.calls, ["start", "native-create", "finish"]); assert.equal(receipt.codes.length, 10); assert.equal(receipt.grant.session_type, "cookie");
});
for (const at of ["start", "native-create", "finish"]) test(`registration stops on account replacement after ${at}`, async () => {
  const h = fixture(); h.retireAt(at); await assert.rejects(registerConsolePasskey(h.client, h.current, "t", "u"), /ownership changed/);
  assert.deepEqual(h.calls, ["start", "native-create", "finish"].slice(0, ["start", "native-create", "finish"].indexOf(at) + 1));
});
test("unconfirmed registration finish is never retried or mistaken for a recovery receipt", async () => {
  const h = fixture(); h.failAt("finish"); await assert.rejects(registerConsolePasskey(h.client, h.current, "t", "u"), /unconfirmed/);
  assert.equal(h.calls.filter(call => call === "finish").length, 1);
});
test("passkey login uses a distinct native assertion and new-session grant, without changing assurance locally", async () => {
  const h = fixture(), result = await signInWithPasskey(h.client, h.current, "t");
  assert.deepEqual(h.calls, ["start", "native-get", "finish"]); assert.equal(result.user.tenant_id, "t");
  assert.equal("verified" in result, false);
});
