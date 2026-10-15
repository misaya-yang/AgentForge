import { expect, test, type Page } from "@playwright/test";
import { installClientAuth, seedClientPrefs } from "./support/helpers";

/**
 * Mock contract for the document lifecycle surface (C6):
 *   1. enable/disable through PATCH /documents/{id}/status, with the row
 *      badge flipping to the resolved display status;
 *   2. archive/unarchive through PATCH /documents/{id}/archive, including
 *      the optional reason (<= 255 chars client-side, dependency D2);
 *   3. the 409 queued contract: a single reindex of an already-queued
 *      document reports "already queued" instead of a raw error
 *      (PRD §5-#6), and batch-reindex reports skipped ids — partial success
 *      in the toast, an in-dialog skipped list on the all-skipped 409
 *      (PRD §5-#7/#8).
 *
 * Every route is fulfilled in-process with a mutable document fixture, so the
 * suite runs against `pnpm dev` alone and never needs the live stack. The
 * fixture list stamps enabled/archived/display_status the way the list will
 * once backend dependency D1 lands; pre-D1 the live list omits those fields.
 */

const DATASET_ID = "mock-doc-lifecycle";

interface MockDoc {
  document_id: string;
  dataset_id: string;
  title: string;
  status: string;
  display_status: string;
  enabled: boolean;
  archived: boolean;
  archived_reason?: string | null;
  error?: string;
  metadata?: Record<string, unknown>;
  word_count: number;
  char_count: number;
  size_bytes: number;
  segment_count: number;
  created_at: string;
  updated_at: string;
}

function makeDocs(): MockDoc[] {
  const base = {
    dataset_id: DATASET_ID,
    word_count: 120,
    char_count: 240,
    size_bytes: 480,
    segment_count: 3,
    created_at: "2026-08-20T09:00:00Z",
    updated_at: "2026-08-20T09:05:00Z",
  };
  return [
    {
      ...base,
      document_id: "doc-handbook",
      title: "员工手册",
      status: "completed",
      display_status: "available",
      enabled: true,
      archived: false,
    },
    {
      ...base,
      document_id: "doc-policy",
      title: "考勤制度",
      status: "completed",
      display_status: "available",
      enabled: true,
      archived: false,
    },
    {
      ...base,
      document_id: "doc-legacy",
      title: "旧版报销制度",
      // Already owns a queue generation: reindex claims must 409.
      status: "waiting",
      display_status: "queuing",
      enabled: true,
      archived: false,
    },
    {
      ...base,
      document_id: "doc-disabled",
      title: "待启用制度",
      status: "completed",
      display_status: "disabled",
      enabled: false,
      archived: false,
    },
    {
      ...base,
      document_id: "doc-error",
      title: "失败文档",
      status: "error",
      display_status: "error",
      enabled: true,
      archived: false,
      error: "worker interrupted",
    },
  ];
}

function restamp(doc: MockDoc) {
  if (doc.archived) doc.display_status = "archived";
  else if (doc.status === "error" || doc.status === "failed") doc.display_status = "error";
  else if (doc.status === "completed")
    doc.display_status = doc.enabled ? "available" : "disabled";
  else if (doc.status === "waiting") doc.display_status = "queuing";
}

function queueActivation(doc: MockDoc, requestedAction: "enable" | "unarchive") {
  doc.status = "waiting";
  doc.metadata = {
    ...(doc.metadata ?? {}),
    _document_lifecycle_reindex: {
      status: "pending",
      desired_enabled: true,
      desired_archived: false,
      requested_action: requestedAction,
    },
  };
  restamp(doc);
}

function completeActivation(doc: MockDoc) {
  doc.enabled = true;
  doc.archived = false;
  doc.archived_reason = null;
  doc.status = "completed";
  const metadata = { ...(doc.metadata ?? {}) };
  delete metadata._document_lifecycle_reindex;
  doc.metadata = metadata;
  restamp(doc);
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

async function installLifecycleHarness(page: Page) {
  const docs = makeDocs();
  const batchReceipts = new Map<string, Record<string, unknown>>();
  const captured = {
    statusUpdates: [] as Array<{ documentId: string; body: Record<string, unknown> }>,
    archiveUpdates: [] as Array<{ documentId: string; body: Record<string, unknown> }>,
    pipelineActions: [] as Array<{ documentId: string; action: string }>,
    batches: [] as Array<Record<string, unknown>>,
    completeActivation(documentId: string) {
      const doc = docs.find((item) => item.document_id === documentId);
      if (!doc) throw new Error(`Unknown mock document: ${documentId}`);
      completeActivation(doc);
    },
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
          name: "Mock Document Lifecycle",
          description: "Browser-only document lifecycle harness",
          visibility: "tenant",
          embedding_provider: "local",
          embedding_model: "hash-384",
          embedding_dimension: 384,
          statistics: { document_count: 5, segment_count: 15, token_count: 300 },
        })
      );
      return;
    }

    if (method === "GET" && pathname === `/api/v1/knowledge/${DATASET_ID}/documents`) {
      await route.fulfill(jsonResponse(docs));
      return;
    }

    // Batch endpoints must match before the per-document regexes.
    if (
      method === "POST" &&
      pathname === `/api/v1/knowledge/${DATASET_ID}/documents/batch-reindex`
    ) {
      const body = request.postDataJSON() as { document_ids?: string[] };
      captured.batches.push(body);
      const ids = body.document_ids ?? [];
      const queuedIds = ids.filter(
        (id) => docs.find((d) => d.document_id === id)?.status !== "waiting"
      );
      const skippedIds = ids.filter((id) => !queuedIds.includes(id));
      queuedIds.forEach((id) => {
        const doc = docs.find((d) => d.document_id === id);
        if (doc) {
          doc.status = "waiting";
          restamp(doc);
        }
      });
      const operationId = `batch-${batchReceipts.size + 1}`;
      const receipt = {
        operation_id: operationId,
        tenant_id: "tenant-a",
        dataset_id: DATASET_ID,
        operation: "reembed",
        status: skippedIds.length > 0 ? "partial" : "completed",
        total_count: ids.length,
        queued_count: queuedIds.length,
        skipped_count: skippedIds.length,
        failed_count: 0,
        problem_items: skippedIds.map((documentId) => ({
          document_id: documentId,
          status: "skipped",
          error_code: "already_queued_or_ineligible",
        })),
        problem_items_truncated: false,
      };
      batchReceipts.set(operationId, receipt);
      await route.fulfill(
        jsonResponse(receipt, 202)
      );
      return;
    }

    const batchMatch = pathname.match(
      /^\/api\/v1\/knowledge\/[^/]+\/document-batches\/([^/]+)$/
    );
    if (method === "GET" && batchMatch) {
      await route.fulfill(jsonResponse(batchReceipts.get(batchMatch[1]) ?? {}, 200));
      return;
    }

    const statusMatch = pathname.match(
      /^\/api\/v1\/knowledge\/[^/]+\/documents\/([^/]+)\/status$/
    );
    if (method === "PATCH" && statusMatch) {
      const documentId = statusMatch[1];
      const body = request.postDataJSON() as { enabled?: boolean };
      captured.statusUpdates.push({ documentId, body });
      const doc = docs.find((d) => d.document_id === documentId);
      if (doc && typeof body.enabled === "boolean") {
        if (body.enabled && !doc.enabled) {
          // Activation is a real two-stage backend transition: the response
          // is waiting while the old enabled=false value stays authoritative.
          queueActivation(doc, "enable");
        } else {
          doc.enabled = body.enabled;
          doc.status = "completed";
          restamp(doc);
        }
      }
      await route.fulfill(jsonResponse(doc ?? {}));
      return;
    }

    const archiveMatch = pathname.match(
      /^\/api\/v1\/knowledge\/[^/]+\/documents\/([^/]+)\/archive$/
    );
    if (method === "PATCH" && archiveMatch) {
      const documentId = archiveMatch[1];
      const body = request.postDataJSON() as { archived?: boolean; reason?: string | null };
      captured.archiveUpdates.push({ documentId, body });
      const doc = docs.find((d) => d.document_id === documentId);
      if (doc && typeof body.archived === "boolean") {
        if (!body.archived && doc.archived) {
          // Unarchive also keeps archived=true until the queued rebuild wins.
          queueActivation(doc, "unarchive");
        } else {
          doc.archived = body.archived;
          doc.archived_reason = body.archived ? body.reason ?? null : null;
          doc.status = "completed";
          restamp(doc);
        }
      }
      await route.fulfill(jsonResponse(doc ?? {}));
      return;
    }

    const pipelineMatch = pathname.match(
      /^\/api\/v1\/knowledge\/[^/]+\/documents\/([^/]+)\/(reindex|reprocess|recover|retry)$/
    );
    if (method === "POST" && pipelineMatch) {
      const documentId = pipelineMatch[1];
      const endpoint = pipelineMatch[2];
      const action = endpoint === "reindex" ? "reembed" : endpoint;
      captured.pipelineActions.push({ documentId, action });
      const doc = docs.find((d) => d.document_id === documentId);
      if (!doc || doc.status === "waiting" || documentId === "doc-policy") {
        await route.fulfill(
          jsonResponse(
            { detail: "Document is already queued; the durable queue owns this generation" },
            409
          )
        );
        return;
      }
      doc.status = "waiting";
      restamp(doc);
      await route.fulfill(
        jsonResponse({ status: "queuing", document_id: documentId, action })
      );
      return;
    }

    await route.fulfill(jsonResponse([]));
  });

  return captured;
}

async function openDocumentsTab(page: Page) {
  await page.goto(`/knowledge/${DATASET_ID}?tab=documents`);
  await expect(page.getByRole("button", { name: "员工手册" })).toBeVisible();
}

test.describe("@mock KB document lifecycle", () => {
  test("disables a document through PATCH /status and surfaces the badge", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const captured = await installLifecycleHarness(page);
    await openDocumentsTab(page);

    const toggle = page.getByTestId("doc-switch-doc-policy");
    await expect(toggle).toBeChecked();
    await toggle.click();

    await expect.poll(() => captured.statusUpdates.length).toBeGreaterThan(0);
    expect(captured.statusUpdates[0]).toEqual({
      documentId: "doc-policy",
      body: { enabled: false },
    });

    await expect(toggle).not.toBeChecked();
    await expect(page.getByTestId("doc-disabled-badge-doc-policy")).toBeVisible();
    // exact: Radix renders an aria-live twin of every toast.
    await expect(page.getByText("文档已禁用", { exact: true })).toBeVisible();
    assertNoClientErrors();
  });

  test("enable stays queued across reload until the worker flips the durable state", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const captured = await installLifecycleHarness(page);
    await openDocumentsTab(page);

    const toggle = page.getByTestId("doc-switch-doc-disabled");
    await expect(toggle).not.toBeChecked();
    await toggle.click();

    await expect.poll(() => captured.statusUpdates.length).toBe(1);
    expect(captured.statusUpdates[0]).toEqual({
      documentId: "doc-disabled",
      body: { enabled: true },
    });
    await expect(page.getByText("启用已排队", { exact: true })).toBeVisible();
    await expect(toggle).not.toBeChecked();
    await expect(toggle).toBeDisabled();
    await expect(page.getByTestId("doc-row-doc-disabled")).toContainText("等待处理");

    // The pending marker and old enabled=false value are server state, not an
    // optimistic client patch, so a full reload must preserve both.
    await page.reload();
    const reloadedToggle = page.getByTestId("doc-switch-doc-disabled");
    await expect(reloadedToggle).not.toBeChecked();
    await expect(reloadedToggle).toBeDisabled();
    await expect(page.getByTestId("doc-row-doc-disabled")).toContainText("等待处理");

    // Completing the mock worker transition is observed by conditional poll;
    // no manual refresh is needed after this point.
    captured.completeActivation("doc-disabled");
    await expect(reloadedToggle).toBeChecked({ timeout: 5_000 });
    await expect(reloadedToggle).toBeEnabled();
    await expect(page.getByTestId("doc-disabled-badge-doc-disabled")).toHaveCount(0);
    assertNoClientErrors();
  });

  test("archives with a reason, then unarchives through the confirm dialog", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const captured = await installLifecycleHarness(page);
    await openDocumentsTab(page);

    await page.getByTestId("doc-archive-doc-handbook").click();
    const dialog = page.getByRole("dialog");
    await expect(dialog).toContainText("归档文档");
    await dialog.getByTestId("archive-reason-input").fill("内容过期");
    await dialog.getByTestId("archive-confirm").click();

    await expect.poll(() => captured.archiveUpdates.length).toBeGreaterThan(0);
    expect(captured.archiveUpdates[0]).toEqual({
      documentId: "doc-handbook",
      body: { archived: true, reason: "内容过期" },
    });

    await expect(page.getByTestId("doc-archived-badge-doc-handbook")).toBeVisible();
    await expect(page.getByText("文档已归档", { exact: true })).toBeVisible();
    // An archived document cannot be toggled until it is restored.
    await expect(page.getByTestId("doc-switch-doc-handbook")).toBeDisabled();

    await page.getByTestId("doc-unarchive-doc-handbook").click();
    await expect(page.getByRole("alertdialog")).toContainText("恢复文档");
    await page.getByTestId("unarchive-confirm").click();

    await expect.poll(() => captured.archiveUpdates.length).toBe(2);
    expect(captured.archiveUpdates[1]).toEqual({
      documentId: "doc-handbook",
      body: { archived: false },
    });
    // The activation response is waiting and deliberately preserves
    // archived=true until the worker has restored the vectors.
    await expect(page.getByTestId("doc-archived-badge-doc-handbook")).toBeVisible();
    await expect(page.getByText("恢复已排队", { exact: true })).toBeVisible();
    await expect(page.getByTestId("doc-unarchive-doc-handbook")).toBeDisabled();
    await expect(page.getByTestId("doc-row-doc-handbook")).toContainText("等待处理");

    await page.reload();
    await expect(page.getByTestId("doc-archived-badge-doc-handbook")).toBeVisible();
    await expect(page.getByTestId("doc-unarchive-doc-handbook")).toBeDisabled();
    captured.completeActivation("doc-handbook");
    await expect(page.getByTestId("doc-archived-badge-doc-handbook")).toHaveCount(0, {
      timeout: 5_000,
    });
    await expect(page.getByTestId("doc-switch-doc-handbook")).toBeChecked();
    assertNoClientErrors();
  });

  test("reembed is explicitly vector-only and calls the legacy reindex route", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const captured = await installLifecycleHarness(page);
    await openDocumentsTab(page);

    await page.getByTestId("doc-index-actions-doc-handbook").click();
    await page.getByTestId("doc-reembed-doc-handbook").click();
    const dialog = page.getByRole("alertdialog");
    await expect(dialog).toContainText("确认仅重嵌文档向量？");
    await expect(dialog).toContainText("不会重新解析源文件，也不会改变切片边界");
    await dialog.getByRole("button", { name: "加入重嵌队列" }).click();

    await expect.poll(() => captured.pipelineActions).toContainEqual({
      documentId: "doc-handbook",
      action: "reembed",
    });
    await expect(page.getByText("仅重嵌向量已排队", { exact: true })).toBeVisible();
    assertNoClientErrors();
  });

  test("reprocess has a separate full-pipeline confirmation and endpoint", async ({ page }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const captured = await installLifecycleHarness(page);
    await openDocumentsTab(page);

    await page.getByTestId("doc-index-actions-doc-handbook").click();
    await page.getByTestId("doc-reprocess-doc-handbook").click();
    const dialog = page.getByRole("alertdialog");
    await expect(dialog).toContainText("确认重新处理文档？");
    await expect(dialog).toContainText("重新解析、切分并重嵌");
    await dialog.getByRole("button", { name: "加入重处理队列" }).click();

    await expect.poll(() => captured.pipelineActions).toContainEqual({
      documentId: "doc-handbook",
      action: "reprocess",
    });
    assertNoClientErrors();
  });

  test("errored documents expose recover and full retry as distinct choices", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const captured = await installLifecycleHarness(page);
    await openDocumentsTab(page);

    await page.getByTestId("doc-index-actions-doc-error").click();
    await expect(page.getByTestId("doc-recover-doc-error")).toBeVisible();
    await expect(page.getByTestId("doc-retry-doc-error")).toBeVisible();
    await page.getByTestId("doc-recover-doc-error").click();
    const dialog = page.getByRole("alertdialog");
    await expect(dialog).toContainText("确认续跑失败世代？");
    await expect(dialog).toContainText("最远的持久化处理阶段");
    await dialog.getByRole("button", { name: "加入续跑队列" }).click();

    await expect.poll(() => captured.pipelineActions).toContainEqual({
      documentId: "doc-error",
      action: "recover",
    });
    assertNoClientErrors();
  });

  test("full retry promises atomic replacement without clearing the serving generation", async ({ page }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const captured = await installLifecycleHarness(page);
    await openDocumentsTab(page);

    await page.getByTestId("doc-index-actions-doc-error").click();
    await page.getByTestId("doc-retry-doc-error").click();
    const dialog = page.getByRole("alertdialog");
    await expect(dialog).toContainText("确认完整重试？");
    await expect(dialog).toContainText("旧世代持续可读");
    await expect(dialog).toContainText("失败或取消会恢复旧世代");
    await expect(dialog).not.toContainText("丢弃");
    await dialog.getByRole("button", { name: "加入完整重试队列" }).click();

    await expect.poll(() => captured.pipelineActions).toContainEqual({
      documentId: "doc-error",
      action: "retry",
    });
    assertNoClientErrors();
  });

  test("single reembed 409 is reported as already queued, not an error", async ({
    page,
  }) => {
    // The 409 is the contract under test; the browser logs the non-2xx
    // resource fetch as a console error even though the app handles it.
    const assertNoClientErrors = watchClientErrors(page, [
      /Failed to load resource.*409/,
    ]);
    const captured = await installLifecycleHarness(page);
    await openDocumentsTab(page);

    await page.getByTestId("doc-index-actions-doc-policy").click();
    await page.getByTestId("doc-reembed-doc-policy").click();
    await expect(page.getByRole("alertdialog")).toContainText("确认仅重嵌文档向量？");
    await page.getByRole("alertdialog").getByRole("button", { name: "加入重嵌队列" }).click();

    await expect.poll(() => captured.pipelineActions).toContainEqual({
      documentId: "doc-policy",
      action: "reembed",
    });
    await expect(page.getByText("已在队列", { exact: true })).toBeVisible();
    // The failure path must not fire alongside.
    await expect(page.getByText("仅重嵌向量失败")).toHaveCount(0);
    assertNoClientErrors();
  });

  test("batch reindex reports queued vs skipped on partial success", async ({ page }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const captured = await installLifecycleHarness(page);
    await openDocumentsTab(page);

    await page.getByRole("button", { name: /批量操作/ }).first().click();
    await page.getByRole("menuitem", { name: "进入批量模式" }).click();
    await page.getByTestId("doc-select-doc-handbook").click();
    await page.getByTestId("doc-select-doc-legacy").click();

    await page.getByRole("button", { name: /批量操作/ }).first().click();
    await page.getByRole("menuitem", { name: /^批量重嵌向量 \(2\)/ }).click();

    const batchRequest = page.waitForRequest((request) =>
      request.url().includes(`/api/v1/knowledge/${DATASET_ID}/documents/batch-reindex`)
    );
    await page.getByRole("button", { name: /^确认重嵌 \(2\)/ }).click();

    const body = (await batchRequest).postDataJSON() as Record<string, unknown>;
    expect(body.document_ids).toEqual(["doc-handbook", "doc-legacy"]);
    expect(captured.batches).toHaveLength(1);

    // Partial success names both sides of the split.
    // Anchored: the aria-live twin prefixes the toast text with "Notification".
    await expect(page.getByText(/^已入队 1 个，跳过 1 个/)).toBeVisible();
    assertNoClientErrors();
  });

  test("batch reindex 409 keeps the dialog open and lists skipped documents", async ({
    page,
  }) => {
    // The all-skipped 409 is the contract under test (see above).
    const assertNoClientErrors = watchClientErrors(page, [
      /Failed to load resource.*409/,
    ]);
    const captured = await installLifecycleHarness(page);
    await openDocumentsTab(page);

    await page.getByRole("button", { name: /批量操作/ }).first().click();
    await page.getByRole("menuitem", { name: "进入批量模式" }).click();
    await page.getByTestId("doc-select-doc-legacy").click();

    await page.getByRole("button", { name: /批量操作/ }).first().click();
    await page.getByRole("menuitem", { name: /^批量重嵌向量 \(1\)/ }).click();
    await page.getByRole("button", { name: /^确认重嵌 \(1\)/ }).click();

    await expect.poll(() => captured.batches.length).toBeGreaterThan(0);
    await expect(page.getByText("没有文档进入新的处理轮次", { exact: true })).toBeVisible();

    // The dialog stays open and shows which documents were skipped so the
    // user can deselect them and retry.
    const skipped = page.getByTestId("batch-reindex-skipped");
    await expect(skipped).toBeVisible();
    await expect(skipped).toContainText("旧版报销制度");
    assertNoClientErrors();
  });

  test("row lifecycle controls stay usable at mobile width", async ({ page }) => {
    const assertNoClientErrors = watchClientErrors(page);
    await page.setViewportSize({ width: 375, height: 812 });
    await installLifecycleHarness(page);
    await openDocumentsTab(page);

    const toggle = page.getByTestId("doc-switch-doc-handbook");
    const archive = page.getByTestId("doc-archive-doc-handbook");
    await expect(toggle).toBeVisible();
    await expect(archive).toBeVisible();

    const scrollWidth = await page.evaluate(() => document.documentElement.scrollWidth);
    expect(scrollWidth).toBeLessThanOrEqual(375);
    assertNoClientErrors();
  });
});
