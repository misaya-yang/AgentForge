import { expect, test, type Page } from "@playwright/test";
import { installClientAuth, seedClientPrefs } from "./support/helpers";

/**
 * Mock contract for the forward-contract rendering surface (C9, PRD T1.1 /
 * A10 / §5-#16):
 *   1. a backend-stamped `display_status` wins over client derivation, and
 *      derivation remains the fallback for un-stamped rows — both drive the
 *      status filter, which runs on the same resolver as the badges;
 *   2. stage timestamps (migration 101) render per-stage durations on
 *      actively-processing rows only, and stay silent otherwise;
 *   3. hit_count telemetry renders at document level and chunk level when
 *      present (the writer lands with backend T2; this is the display side).
 *
 * Every route is fulfilled in-process, so the suite runs against `pnpm dev`
 * alone and never needs the live stack.
 */

const DATASET_ID = "mock-forward-contract";

interface MockDoc {
  document_id: string;
  dataset_id: string;
  title: string;
  status: string;
  display_status?: string;
  enabled: boolean;
  archived: boolean;
  hit_count?: number;
  word_count: number;
  char_count: number;
  size_bytes: number;
  segment_count: number;
  created_at: string;
  updated_at: string;
  started_at?: string;
  completed_at?: string;
  parsing_started_at?: string;
  splitting_started_at?: string;
  indexing_started_at?: string;
}

function makeDocs(): MockDoc[] {
  // One base instant for every stage timestamp so the derived durations are
  // exact regardless of when the page renders relative to install time.
  const now = Date.now();
  const isoAgo = (seconds: number) => new Date(now - seconds * 1000).toISOString();
  const base = {
    dataset_id: DATASET_ID,
    enabled: true,
    archived: false,
    word_count: 120,
    char_count: 240,
    size_bytes: 480,
    segment_count: 2,
    created_at: "2026-08-20T09:00:00Z",
    updated_at: "2026-08-20T09:05:00Z",
  };
  return [
    {
      ...base,
      document_id: "doc-alpha",
      title: "产品白皮书",
      status: "completed",
      display_status: "available",
      // Retrieval telemetry present: must render at row and chunk level.
      hit_count: 7,
    },
    {
      ...base,
      document_id: "doc-beta",
      title: "暂停的规范",
      // Stamp beats derivation: derived from status alone this row would be
      // "available"; the paused stamp must win everywhere it is resolved.
      status: "completed",
      display_status: "paused",
    },
    {
      ...base,
      document_id: "doc-gamma",
      title: "失败记录",
      // No stamp: the resolver must fall back to derivation (failed → error).
      status: "failed",
    },
    {
      ...base,
      document_id: "doc-delta",
      title: "正在索引",
      status: "parsing",
      display_status: "indexing",
      started_at: isoAgo(58),
      parsing_started_at: isoAgo(58),
      splitting_started_at: isoAgo(46),
      indexing_started_at: isoAgo(12),
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

async function installForwardContractHarness(page: Page) {
  const docs = makeDocs();

  await installClientAuth(page, {
    permissions: ["console:dashboard:view", "knowledge:dataset:view"],
    effective_permissions: ["console:dashboard:view", "knowledge:dataset:view"],
  });
  await seedClientPrefs(page, { locale: "zh-CN" });

  await page.route("**/api/v1/knowledge/**", async (route) => {
    const request = route.request();
    const method = request.method();
    const url = new URL(request.url());
    const pathname = url.pathname;

    if (method === "GET" && pathname === `/api/v1/knowledge/datasets/${DATASET_ID}`) {
      await route.fulfill(
        jsonResponse({
          dataset_id: DATASET_ID,
          name: "Mock Forward Contract",
          description: "Browser-only forward-contract harness",
          visibility: "tenant",
          embedding_provider: "local",
          embedding_model: "hash-384",
          embedding_dimension: 384,
          statistics: { document_count: docs.length, segment_count: 8, token_count: 160 },
        })
      );
      return;
    }

    if (method === "GET" && pathname === `/api/v1/knowledge/${DATASET_ID}/documents`) {
      await route.fulfill(jsonResponse(docs));
      return;
    }

    if (method === "GET" && pathname === `/api/v1/knowledge/${DATASET_ID}/segments`) {
      if (url.searchParams.get("document_id") === "doc-alpha") {
        await route.fulfill(
          jsonResponse([
            {
              segment_id: "seg-hit",
              dataset_id: DATASET_ID,
              document_id: "doc-alpha",
              position: 1,
              text: "产品白皮书第一段，介绍整体架构与设计目标。",
              token_count: 12,
              char_count: 24,
              enabled: true,
              hit_count: 5,
            },
            {
              segment_id: "seg-cold",
              dataset_id: DATASET_ID,
              document_id: "doc-alpha",
              position: 2,
              text: "产品白皮书第二段，附录与参考资料。",
              token_count: 10,
              char_count: 20,
              enabled: true,
            },
          ])
        );
        return;
      }
      await route.fulfill(jsonResponse([]));
      return;
    }

    await route.fulfill(jsonResponse([]));
  });

  // Hermetic eval probe: the retrieval workbench is persistently mounted and
  // resolves its linked eval dataset on mount (C7). An unmocked request would
  // leak to the live gateway and a 401 there wipes the seeded auth session.
  await page.route("**/api/v1/eval/**", async (route) => {
    await route.fulfill(jsonResponse({ datasets: [], total: 0, limit: 200, offset: 0 }));
  });
}

async function openDocumentsTab(page: Page) {
  await page.goto(`/knowledge/${DATASET_ID}?tab=documents`);
  await expect(page.getByRole("button", { name: "产品白皮书" })).toBeVisible();
}

async function selectStatusFilter(page: Page, label: string) {
  await page.getByTestId("doc-status-filter").click();
  await page.getByRole("option", { name: label, exact: true }).click();
}

test.describe("@mock KB forward contract", () => {
  test("stamped display_status beats derivation and both drive the filter", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page);
    await installForwardContractHarness(page);
    await openDocumentsTab(page);

    // The paused stamp wins: doc-beta is paused, not the derived "available".
    await selectStatusFilter(page, "已暂停");
    await expect(page.getByRole("button", { name: "暂停的规范" })).toBeVisible();
    await expect(page.getByRole("button", { name: "产品白皮书" })).toHaveCount(0);
    await expect(page.getByRole("button", { name: "失败记录" })).toHaveCount(0);
    await expect(page.getByRole("button", { name: "正在索引" })).toHaveCount(0);

    // Derived fallback without a stamp: failed → 失败.
    await selectStatusFilter(page, "失败");
    await expect(page.getByRole("button", { name: "失败记录" })).toBeVisible();
    await expect(page.getByRole("button", { name: "暂停的规范" })).toHaveCount(0);
    await expect(page.getByRole("button", { name: "产品白皮书" })).toHaveCount(0);

    // Available shows only the truly available row — the stamped paused row
    // must NOT leak in here.
    await selectStatusFilter(page, "可用");
    await expect(page.getByRole("button", { name: "产品白皮书" })).toBeVisible();
    await expect(page.getByRole("button", { name: "暂停的规范" })).toHaveCount(0);

    await selectStatusFilter(page, "全部状态");
    await expect(page.getByRole("button", { name: "产品白皮书" })).toBeVisible();
    await expect(page.getByRole("button", { name: "暂停的规范" })).toBeVisible();
    assertNoClientErrors();
  });

  test("renders per-stage durations on active rows only", async ({ page }) => {
    const assertNoClientErrors = watchClientErrors(page);
    await installForwardContractHarness(page);
    await openDocumentsTab(page);

    // parsing 12s (58→46), splitting 34s (46→12), indexing live with "…".
    const stageLine = page.getByTestId("doc-stage-times-doc-delta");
    await expect(stageLine).toBeVisible();
    await expect(stageLine).toContainText(/解析 12s · 切分 34s · 索引 \d+s…/);

    // Terminal and un-stamped rows keep the coarse badge: no stage line.
    await expect(page.getByTestId("doc-stage-times-doc-alpha")).toHaveCount(0);
    await expect(page.getByTestId("doc-stage-times-doc-beta")).toHaveCount(0);
    await expect(page.getByTestId("doc-stage-times-doc-gamma")).toHaveCount(0);

    // The indexing filter finds the stamped indexing row.
    await selectStatusFilter(page, "索引中");
    await expect(page.getByRole("button", { name: "正在索引" })).toBeVisible();
    await expect(page.getByRole("button", { name: "产品白皮书" })).toHaveCount(0);
    assertNoClientErrors();
  });

  test("shows hit_count badges on the row and on chunks when present", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page);
    await installForwardContractHarness(page);
    await openDocumentsTab(page);

    const rowBadge = page.getByTestId("doc-hit-count-doc-alpha");
    await expect(rowBadge).toBeVisible();
    await expect(rowBadge).toContainText("7");

    // Open the segment panel for the hit document.
    await page.getByRole("button", { name: "产品白皮书" }).click();
    const chunkHit = page.getByTestId("segment-hit-count-seg-hit");
    await expect(chunkHit).toBeVisible();
    await expect(chunkHit).toContainText("5");
    // The segment without telemetry shows no pill.
    await expect(page.getByTestId("segment-hit-count-seg-cold")).toHaveCount(0);
    assertNoClientErrors();
  });

  test("stage line and badges stay usable at mobile width", async ({ page }) => {
    const assertNoClientErrors = watchClientErrors(page);
    await page.setViewportSize({ width: 375, height: 812 });
    await installForwardContractHarness(page);
    await openDocumentsTab(page);

    await expect(page.getByTestId("doc-stage-times-doc-delta")).toBeVisible();
    await expect(page.getByTestId("doc-hit-count-doc-alpha")).toBeVisible();

    const scrollWidth = await page.evaluate(() => document.documentElement.scrollWidth);
    expect(scrollWidth).toBeLessThanOrEqual(375);
    assertNoClientErrors();
  });
});
