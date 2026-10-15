import { expect, test, type Page } from "@playwright/test";
import { installClientAuth, seedClientPrefs } from "./support/helpers";

/**
 * Mock contract for the segment operations surface (C5):
 *   1. per-segment enable/disable through PATCH /segments/{id}/status;
 *   2. batch enable/disable through POST /segments/batch/enable, including
 *      the partial-success report ({success, updated, total});
 *   3. full-field segment editing — the PUT body must carry text + answer
 *      + keywords together (the old text-only edit silently dropped the
 *      other two fields, PRD §5-#13).
 *
 * Every route is fulfilled in-process with a mutable segment fixture, so the
 * suite runs against `pnpm dev` alone and never needs the live stack.
 */

const DATASET_ID = "mock-segment-ops";
const DOC_ID = "doc-handbook";

interface MockSegment {
  segment_id: string;
  dataset_id: string;
  document_id: string;
  position: number;
  text: string;
  enabled: boolean;
  answer?: string;
  keywords?: string[];
}

function makeSegments(): MockSegment[] {
  return [
    {
      segment_id: "seg-reimburse",
      dataset_id: DATASET_ID,
      document_id: DOC_ID,
      position: 1,
      text: "报销流程需要提交发票、费用申请单和审批记录。",
      enabled: true,
      answer: "提交发票、费用申请单和审批记录。",
      keywords: ["报销", "发票"],
    },
    {
      segment_id: "seg-leave",
      dataset_id: DATASET_ID,
      document_id: DOC_ID,
      position: 2,
      text: "年假申请应提前五天在人事系统提交，并由直属经理审批。",
      enabled: true,
      keywords: [],
    },
    {
      segment_id: "seg-onboarding",
      dataset_id: DATASET_ID,
      document_id: DOC_ID,
      position: 3,
      text: "新员工入职第一天需要领取工牌、笔记本电脑和门禁卡。",
      // Ships disabled: the list render must surface the stored state.
      enabled: false,
      keywords: ["入职"],
    },
  ];
}

function jsonResponse(body: unknown, status = 200) {
  return {
    status,
    contentType: "application/json",
    body: JSON.stringify(body),
  };
}

function watchClientErrors(page: Page, allowedConsoleErrors: RegExp[] = []) {
  const pageErrors: string[] = [];
  const consoleErrors: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(String(error)));
  page.on("console", (message) => {
    if (message.type() === "error") consoleErrors.push(message.text());
  });
  return () => {
    expect(pageErrors, `Page errors:\n${pageErrors.join("\n")}`).toEqual([]);
    const unexpectedConsoleErrors = consoleErrors.filter(
      (message) => !allowedConsoleErrors.some((pattern) => pattern.test(message))
    );
    expect(
      unexpectedConsoleErrors,
      `Unexpected console errors:\n${unexpectedConsoleErrors.join("\n")}`
    ).toEqual([]);
  };
}

interface SegmentOpsHarnessOptions {
  /**
   * Simulate the backend skipping items: given the requested ids, return how
   * many actually get updated (defaults to all of them).
   */
  batchUpdated?: (segmentIds: string[]) => number;
}

async function installSegmentOpsHarness(
  page: Page,
  options: SegmentOpsHarnessOptions = {}
) {
  const segments = makeSegments();
  const captured = {
    statusUpdates: [] as Array<{ segmentId: string; body: Record<string, unknown> }>,
    edits: [] as Array<{ segmentId: string; body: Record<string, unknown> }>,
    batches: [] as Array<Record<string, unknown>>,
  };

  await installClientAuth(page, {
    permissions: ["console:dashboard:view", "knowledge:dataset:view"],
    effective_permissions: ["console:dashboard:view", "knowledge:dataset:view"],
  });
  await seedClientPrefs(page, { locale: "zh-CN" });

  await page.route("**/api/v1/knowledge/**", async (route) => {
    const request = route.request();
    const method = request.method();
    const pathname = new URL(request.url()).pathname;

    if (method === "GET" && pathname === `/api/v1/knowledge/datasets/${DATASET_ID}`) {
      await route.fulfill(
        jsonResponse({
          dataset_id: DATASET_ID,
          name: "Mock Segment Ops",
          description: "Browser-only segment operations harness",
          visibility: "tenant",
          embedding_provider: "local",
          embedding_model: "hash-384",
          embedding_dimension: 384,
          statistics: { document_count: 1, segment_count: 3, token_count: 60 },
        })
      );
      return;
    }

    if (method === "GET" && pathname === `/api/v1/knowledge/${DATASET_ID}/documents`) {
      await route.fulfill(
        jsonResponse([
          {
            document_id: DOC_ID,
            dataset_id: DATASET_ID,
            title: "员工手册",
            status: "completed",
            display_status: "available",
            enabled: true,
            archived: false,
            word_count: 120,
            char_count: 240,
            size_bytes: 480,
            segment_count: 3,
            created_at: "2026-08-20T09:00:00Z",
            updated_at: "2026-08-20T09:05:00Z",
          },
        ])
      );
      return;
    }

    if (method === "GET" && pathname === `/api/v1/knowledge/${DATASET_ID}/segments`) {
      await route.fulfill(jsonResponse(segments));
      return;
    }

    const statusMatch = pathname.match(
      /^\/api\/v1\/knowledge\/[^/]+\/segments\/([^/]+)\/status$/
    );
    if (method === "PATCH" && statusMatch) {
      const segmentId = statusMatch[1];
      const body = request.postDataJSON() as { enabled?: boolean };
      captured.statusUpdates.push({ segmentId, body });
      const segment = segments.find((s) => s.segment_id === segmentId);
      if (segment && typeof body.enabled === "boolean") segment.enabled = body.enabled;
      await route.fulfill(jsonResponse(segment ?? {}));
      return;
    }

    if (
      method === "POST" &&
      pathname === `/api/v1/knowledge/${DATASET_ID}/segments/batch/enable`
    ) {
      const body = request.postDataJSON() as { segment_ids?: string[]; enabled?: boolean };
      captured.batches.push(body);
      const ids = body.segment_ids ?? [];
      const updated = options.batchUpdated ? options.batchUpdated(ids) : ids.length;
      ids.slice(0, updated).forEach((id) => {
        const segment = segments.find((s) => s.segment_id === id);
        if (segment && typeof body.enabled === "boolean") segment.enabled = body.enabled;
      });
      await route.fulfill(jsonResponse({ success: true, updated, total: ids.length }));
      return;
    }

    const editMatch = pathname.match(/^\/api\/v1\/knowledge\/[^/]+\/segments\/([^/]+)$/);
    if (method === "PUT" && editMatch) {
      const segmentId = editMatch[1];
      const body = request.postDataJSON() as Record<string, unknown>;
      captured.edits.push({ segmentId, body });
      const segment = segments.find((s) => s.segment_id === segmentId);
      if (segment) {
        if (typeof body.text === "string") segment.text = body.text;
        if (typeof body.answer === "string") segment.answer = body.answer;
        if (Array.isArray(body.keywords)) segment.keywords = body.keywords as string[];
      }
      await route.fulfill(jsonResponse(segment ?? {}));
      return;
    }

    await route.fulfill(jsonResponse([]));
  });

  return captured;
}

async function openSegmentPanel(page: Page) {
  await page.goto(`/knowledge/${DATASET_ID}?tab=documents`);
  await page.getByRole("button", { name: "员工手册" }).click();
  await expect(page.getByText("切片列表")).toBeVisible();
  await expect(page.getByTestId("segment-switch-seg-reimburse")).toBeVisible();
}

test.describe("@mock KB segment operations", () => {
  test("toggles a single segment through PATCH /status and reflects the new state", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const captured = await installSegmentOpsHarness(page);
    await openSegmentPanel(page);

    // The fixture ships one disabled segment: the stats badge renders it.
    await expect(page.getByTestId("segment-disabled-count")).toContainText("1");

    const toggle = page.getByTestId("segment-switch-seg-reimburse");
    await expect(toggle).toBeChecked();
    await toggle.click();

    await expect.poll(() => captured.statusUpdates.length).toBeGreaterThan(0);
    expect(captured.statusUpdates[0]).toEqual({
      segmentId: "seg-reimburse",
      body: { enabled: false },
    });

    // The refetch applies the new state: switch flips, badge count grows.
    await expect(toggle).not.toBeChecked();
    await expect(page.getByTestId("segment-disabled-count")).toContainText("2");
    // exact: Radix renders an aria-live twin of every toast.
    await expect(page.getByText("分块已禁用", { exact: true })).toBeVisible();
    assertNoClientErrors();
  });

  test("batch disables selected segments and reports partial success", async ({ page }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const captured = await installSegmentOpsHarness(page, {
      // Backend skips one item: updated = requested - 1.
      batchUpdated: (ids) => ids.length - 1,
    });
    await openSegmentPanel(page);

    await page.getByTestId("segment-batch-toggle").click();
    await expect(page.getByTestId("segment-batch-bar")).toBeVisible();

    await page.getByTestId("segment-select-seg-reimburse").click();
    await page.getByTestId("segment-select-seg-leave").click();
    await expect(page.getByTestId("segment-batch-count")).toContainText("已选 2 个");

    const batchRequest = page.waitForRequest((request) =>
      request.url().includes(`/api/v1/knowledge/${DATASET_ID}/segments/batch/enable`)
    );
    await page.getByRole("button", { name: "禁用", exact: true }).click();

    const body = (await batchRequest).postDataJSON() as Record<string, unknown>;
    expect(body.segment_ids).toEqual(["seg-reimburse", "seg-leave"]);
    expect(body.enabled).toBe(false);
    expect(captured.batches).toHaveLength(1);

    // Partial success must be surfaced, not swallowed as a full success.
    // Anchored: the aria-live twin prefixes the toast text with "Notification".
    await expect(page.getByText(/^部分更新：1 \/ 2/)).toBeVisible();
    // Batch mode exits and only the actually-updated segment flipped.
    await expect(page.getByTestId("segment-batch-bar")).toHaveCount(0);
    await expect(page.getByTestId("segment-disabled-count")).toContainText("2");
    assertNoClientErrors();
  });

  test("saves full-field segment edits: text, answer, and keywords together", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const captured = await installSegmentOpsHarness(page);
    await openSegmentPanel(page);

    // Open the edit dialog from the seg-reimburse card (per-card testid:
    // the page header carries its own "编辑" menu).
    await page.getByTestId("segment-edit-seg-reimburse").click();
    const dialog = page.getByRole("dialog");
    await expect(dialog).toContainText("编辑片段");

    // Every stored field is loaded, so saving can round-trip them.
    await expect(dialog.locator("textarea").first()).toHaveValue(
      "报销流程需要提交发票、费用申请单和审批记录。"
    );
    await expect(dialog.locator("textarea").nth(1)).toHaveValue(
      "提交发票、费用申请单和审批记录。"
    );
    await expect(dialog.getByPlaceholder("用逗号分隔多个关键词")).toHaveValue("报销, 发票");

    await dialog.locator("textarea").first().fill("报销需要发票与审批记录。");
    await dialog.locator("textarea").nth(1).fill("需要发票、费用申请单。");
    // Full-width comma, duplicate, and trailing empties all get normalised.
    await dialog
      .getByPlaceholder("用逗号分隔多个关键词")
      .fill("报销, 发票, 审批，发票 , ");

    await dialog.getByRole("button", { name: "保存" }).click();

    await expect.poll(() => captured.edits.length).toBeGreaterThan(0);
    expect(captured.edits[0]?.segmentId).toBe("seg-reimburse");
    expect(captured.edits[0]?.body).toEqual({
      text: "报销需要发票与审批记录。",
      answer: "需要发票、费用申请单。",
      keywords: ["报销", "发票", "审批"],
    });

    await expect(dialog).toHaveCount(0);
    await expect(page.getByText("分块已保存", { exact: true })).toBeVisible();
    assertNoClientErrors();
  });

  test("segment batch bar stays usable at mobile width", async ({ page }) => {
    const assertNoClientErrors = watchClientErrors(page);
    await page.setViewportSize({ width: 375, height: 812 });
    await installSegmentOpsHarness(page);
    await openSegmentPanel(page);

    await page.getByTestId("segment-batch-toggle").click();
    await expect(page.getByTestId("segment-batch-bar")).toBeVisible();
    await page.getByTestId("segment-select-seg-reimburse").click();

    const enableButton = page.getByRole("button", { name: "启用", exact: true });
    await expect(enableButton).toBeEnabled();
    const box = await enableButton.boundingBox();
    expect(box).not.toBeNull();
    expect(box!.x + box!.width).toBeLessThanOrEqual(375);
    assertNoClientErrors();
  });
});
