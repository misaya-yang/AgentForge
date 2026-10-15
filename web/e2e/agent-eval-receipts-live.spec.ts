import fs from "node:fs";
import path from "node:path";
import { expect, test } from "@playwright/test";

// Read back existing acceptance runs. This spec never dispatches candidates,
// provisions accounts, or removes fixtures from the singleton live stack.
const fixturePath = process.env.R3_ACCEPTANCE_FIXTURE_FILE;
test.skip(!fixturePath, "Set R3_ACCEPTANCE_FIXTURE_FILE to an existing R3 receipt file.");

type ReceiptFixture = Record<string, Array<{ run_id: string }>>;

function runId(key: string) {
  const fixture = JSON.parse(fs.readFileSync(fixturePath!, "utf8")) as ReceiptFixture;
  const id = fixture[key]?.[0]?.run_id;
  if (!id) throw new Error(`Missing acceptance receipt: ${key}`);
  return id;
}

const receipts = [
  { key: "manifest_run_b", status: "succeeded", mode: "live_candidate", text: "Candidate gate: fail", coverage: "5/5 cases" },
  { key: "provider_failure_run", status: "failed", mode: "live_candidate", text: "Unscored 1", coverage: "0/1 cases" },
  { key: "healthy_rescore_run", status: "succeeded", mode: "rescore_trace", text: "Passed 1", coverage: undefined },
  { key: "cold_eval_run_fixed", status: "cancelled", mode: "live_candidate", text: "Unscored 2", coverage: undefined },
] as const;

for (const receipt of receipts) {
  test(`existing R3 ${receipt.key} preserves execution and quality facts`, async ({ page }) => {
    const id = runId(receipt.key);
    const errors: string[] = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await page.goto(`/eval?tab=runs&run_id=${id}`);
    await expect(page.locator("code").filter({ hasText: id })).toBeVisible();
    const results = page.locator(".eval-run-results");
    await expect(results).toContainText(receipt.status);
    await expect(results).toContainText(`Mode: ${receipt.mode}`);
    await expect(results).toContainText(receipt.text);
    if (receipt.coverage) await expect(results).toContainText(receipt.coverage);
    if (receipt.status === "cancelled") {
      await expect(results).toContainText("Passed 1");
      await expect(results).not.toContainText("100.0%");
    }
    expect(errors).toEqual([]);
    const artifactDir = path.resolve("../tmp/browser");
    fs.mkdirSync(artifactDir, { recursive: true });
    await page.screenshot({ path: path.join(artifactDir, `r3-${receipt.key}.png`), fullPage: true });
  });
}

test("Agent operations retain the feedback run and open its authorized Trace", async ({ page }) => {
  test.setTimeout(30_000);
  const fixture = JSON.parse(fs.readFileSync(fixturePath!, "utf8")) as {
    final_agent: { agent_id: string };
    member_hosted_a3_answer: { run_id: string };
  };
  const traceId = fixture.member_hosted_a3_answer.run_id;
  await page.addInitScript(() => localStorage.setItem("i18nextLng", "en-US"));
  await page.goto(`/agents/${fixture.final_agent.agent_id}/analytics`);
  await expect(page.getByTestId("agent-analytics-page")).toBeVisible();
  await page.getByRole("combobox", { name: "Channel", exact: true }).click();
  await page.locator(".ant-select-dropdown:visible .ant-select-item-option-content").filter({ hasText: /^hosted$/ }).click({ timeout: 10_000 });
  const link = page.locator(".agent-trace-table").getByRole("link", { name: new RegExp(traceId.slice(0, 8)) });
  await expect(link).toBeVisible();
  await page.screenshot({ path: path.resolve("../tmp/browser/r3-agent-analytics.png"), fullPage: true });
  await link.click();
  await expect(page).toHaveURL(new RegExp(`trace_id=${traceId}`));
  await expect(page.locator("main").last()).toContainText(traceId);
});
