/** Real authenticated backend only. No route mocks, fabricated roles or provider calls.
 * Writes require explicit opt-in and an existing configured, dedicated test tenant.
 */
import { expect, test, type Page } from "@playwright/test";
import { ensureAuthenticatedPage } from "./support/helpers";
import type { GuardrailPolicy, PolicySnapshot } from "../src/api/guardrails";
async function readPolicy(page: Page): Promise<{ status: number; snapshot: PolicySnapshot | null }> {
  return page.evaluate(async () => {
    const response = await fetch("/api/v1/guardrails/policy", { credentials: "same-origin", cache: "no-store" });
    return { status: response.status, snapshot: response.ok ? await response.json() : null };
  });
}
async function writePolicy(page: Page, policy: GuardrailPolicy, expectedPin: string | null): Promise<number> {
  return page.evaluate(async ({ policy, expectedPin }) => {
    if (location.protocol !== "https:") throw new Error("Guardrails browser acceptance requires trusted HTTPS");
    const session = await fetch("/api/auth/session", { credentials: "same-origin", cache: "no-store" });
    if (!session.ok) throw new Error("BFF session unavailable");
    const { csrf_token } = await session.json();
    if (!csrf_token) throw new Error("CSRF bootstrap missing");
    const response = await fetch("/api/v1/guardrails/policy", { method: "PUT", credentials: "same-origin", headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf_token }, body: JSON.stringify({ policy, expected_pin: expectedPin }) });
    return response.status;
  }, { policy, expectedPin });
}
test("real tenant policy authority and missing decision are represented honestly", async ({ page }) => {
  await ensureAuthenticatedPage(page, "/settings/guardrails");
  await expect(page.getByRole("heading", { name: /Guardrail policy and decisions|护栏策略与决策日志/ })).toBeVisible();
  const current = await readPolicy(page);
  if (current.status === 403) {
    await expect(page.getByText(/Policy management requires|策略管理需要|current identity cannot|当前身份无权/)).toBeVisible();
    await expect(page.getByRole("button", { name: /Review changes|检查更改/ })).toHaveCount(0);
  } else {
    expect(current.status).toBe(200);
    await expect(page.getByRole("form", { name: /Guardrail policy editor|护栏策略编辑/ })).toBeVisible();
    const toggle = page.getByLabel(/Enable guardrails|启用护栏/);
    await expect(toggle).toBeChecked({ checked: current.snapshot!.policy?.enabled ?? false });
  }
  await page.getByLabel(/Request ID|请求编号/).fill(`missing-guardrail-${Date.now()}`);
  const response = page.waitForResponse(r => r.url().includes("/api/v1/guardrails/decisions/") && r.request().method() === "GET");
  await page.getByRole("button", { name: /Find decisions|查询决策/ }).click();
  expect((await response).status()).toBe(404);
  await expect(page.getByRole("alert").filter({ hasText: /No record|未找到记录/ })).toBeVisible();
});
test("real concurrent policy update blocks stale UI save and supports explicit reload", async ({ page }) => {
  test.skip(process.env.RUN_GUARDRAILS_WRITE_LIVE !== "1", "Requires explicit policy-write authorization on the existing dedicated tenant");
  await ensureAuthenticatedPage(page, "/settings/guardrails");
  const before = await readPolicy(page);
  expect(before.status).toBe(200);
  if (!before.snapshot?.policy) throw new Error("Seed a real valid tenant policy before guardrails write acceptance");
  const original = before.snapshot.policy;
  const revision = `e2e-${Date.now()}`;
  let changed = false;
  try {
    await expect(page.getByLabel(/Policy version|策略版本/)).toHaveValue(original.version);
    await page.getByLabel(/Policy version|策略版本/).fill("stale-ui-draft");
    expect(await writePolicy(page, { ...original, version: revision }, before.snapshot.policy_pin)).toBe(200);
    changed = true;
    await page.getByRole("button", { name: /Review changes|检查更改/ }).click();
    const conflict = page.waitForResponse(r => r.url().endsWith("/api/v1/guardrails/policy") && r.request().method() === "PUT");
    await page.getByRole("button", { name: /Confirm save|确认保存/ }).click();
    expect((await conflict).status()).toBe(409);
    await expect(page.getByRole("alert")).toContainText(/Someone changed|其他人修改/);
    await expect(page.getByRole("button", { name: /Confirm save|确认保存/ })).toBeDisabled();
    await page.getByRole("button", { name: /Discard draft and reload|放弃草案并重新读取/ }).click();
    await expect(page.getByLabel(/Policy version|策略版本/)).toHaveValue(revision);
  } finally {
    if (changed) {
      const latest = await readPolicy(page);
      // Never overwrite another actor's intervening change during cleanup.
      expect(latest.snapshot?.policy?.version).toBe(revision);
      expect(await writePolicy(page, original, latest.snapshot!.policy_pin)).toBe(200);
    }
  }
});
