import { expect, test, type Page } from "@playwright/test";
import { installFirstRunHarness } from "./support/first-run";
import { seedClientPrefs } from "./support/helpers";

const NOW = Date.now();

function item(partial: Record<string, unknown>) {
  return {
    parent_id: null,
    icon: "",
    position: "a0",
    source: "native",
    created_by: "first-run-user",
    updated_by: "first-run-user",
    content_format: "markdown",
    current_revision: 0,
    starred: false,
    shared: false,
    updated_at: new Date(NOW).toISOString(),
    deleted_at: null,
    ...partial,
  };
}

const SPACES = [
  { space_id: "space-studio", name: "高颜值写实人像 · 提示词与样片", icon: "", kind: "team", visibility: "members", dataset_id: "hidden-studio", created_by: "first-run-user", created_at: new Date(NOW).toISOString(), updated_at: new Date(NOW).toISOString() },
  { space_id: "space-notes", name: "拍摄笔记", icon: "", kind: "personal", visibility: "private", dataset_id: "hidden-notes", created_by: "first-run-user", created_at: new Date(NOW).toISOString(), updated_at: new Date(NOW).toISOString() },
];

const ITEMS = [
  item({ item_id: "item-c37", space_id: "space-notes", space_name: "拍摄笔记", kind: "image", title: "C37 至 C51 电影感照片", last_activity_at: new Date(NOW - 14 * 60_000).toISOString() }),
  item({ item_id: "item-prompt", space_id: "space-notes", space_name: "拍摄笔记", kind: "image", title: "B 和 C 系列提示词出图", last_activity_at: new Date(NOW - 37 * 60_000).toISOString() }),
  item({ item_id: "item-handbook", space_id: "space-notes", space_name: "拍摄笔记", kind: "site", title: "llm-rl-handbook.html", last_activity_at: new Date(NOW - 60 * 60_000).toISOString() }),
  item({ item_id: "item-md", space_id: "space-notes", space_name: "拍摄笔记", kind: "page", title: "FoRIS_ICLR2027_3970_审稿意见整理.md", last_activity_at: new Date(NOW - 7 * 60 * 60_000).toISOString() }),
  item({ item_id: "item-rebuttal", space_id: "space-notes", space_name: "拍摄笔记", kind: "page", title: "Beyond_the_Base_Rebuttal_Commitment_Audit_2026-11-12.md", last_activity_at: new Date(NOW - 14 * 60 * 60_000).toISOString() }),
  item({ item_id: "item-portrait", space_id: "space-studio", space_name: "高颜值写实人像 · 提示词与样片", kind: "image", title: "生活人像三张试拍 旅馆 球场 地铁", created_by: "other-user", shared: true, last_activity_at: new Date(NOW - 24 * 60 * 60_000).toISOString() }),
  item({ item_id: "item-image26", space_id: "space-studio", space_name: "高颜值写实人像 · 提示词与样片", kind: "image", title: "Image2.6 生图裁决 三组稳定性测试", created_by: "other-user", shared: true, last_activity_at: new Date(NOW - 24 * 60 * 60_000).toISOString() }),
  item({ item_id: "item-zip", space_id: "space-notes", space_name: "拍摄笔记", kind: "file", title: "develop-scientific-methods.zip", last_activity_at: new Date(NOW - 2 * 24 * 60 * 60_000).toISOString() }),
  item({ item_id: "item-bowl", space_id: "space-notes", space_name: "拍摄笔记", kind: "image", title: "保龄球馆 小酒馆 台球厅人像", last_activity_at: new Date(NOW - 2 * 24 * 60 * 60_000).toISOString() }),
  item({ item_id: "item-cvpr", space_id: "space-notes", space_name: "拍摄笔记", kind: "page", title: "SH_CVPR2027_Oral_Story_Review.md", last_activity_at: new Date(NOW - 3 * 24 * 60 * 60_000).toISOString() }),
  item({ item_id: "item-y2k", space_id: "space-notes", space_name: "拍摄笔记", kind: "image", title: "Y2K CCD 人像试验", last_activity_at: new Date(NOW - 3 * 24 * 60 * 60_000).toISOString() }),
];

async function installSpaceApi(page: Page) {
  await page.route("**/api/v1/knowledge/spaces**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (route.request().method() === "GET" && path === "/api/v1/knowledge/spaces") {
      return route.fulfill({ contentType: "application/json", body: JSON.stringify({ spaces: SPACES }) });
    }
    if (route.request().method() === "POST" && path === "/api/v1/knowledge/spaces") {
      return route.fulfill({ status: 201, contentType: "application/json", body: JSON.stringify({ ...SPACES[1], space_id: "space-new", name: "新空间" }) });
    }
    if (route.request().method() === "POST" && path.endsWith("/items")) {
      const body = route.request().postDataJSON() as { title?: string };
      return route.fulfill({ status: 201, contentType: "application/json", body: JSON.stringify(item({ item_id: "item-new", space_id: "space-notes", space_name: "拍摄笔记", kind: "page", title: body.title ?? "新页面", last_activity_at: new Date(NOW).toISOString() })) });
    }
    return route.fallback();
  });
  await page.route("**/api/v1/knowledge/space-items**", async (route) => {
    const url = new URL(route.request().url());
    if (route.request().method() === "GET" && url.pathname === "/api/v1/knowledge/space-items") {
      const view = url.searchParams.get("view") ?? "all";
      const q = (url.searchParams.get("q") ?? "").toLowerCase();
      const kind = url.searchParams.get("kind");
      let rows = ITEMS.filter((row) => !q || String(row.title).toLowerCase().includes(q));
      if (kind) rows = rows.filter((row) => row.kind === kind);
      if (view === "starred") rows = rows.filter((row) => row.starred);
      if (view === "mine") rows = rows.filter((row) => row.created_by === "first-run-user");
      if (view === "shared") rows = rows.filter((row) => row.shared);
      if (view === "recent") rows = rows.slice(0, Number(url.searchParams.get("limit") ?? 5));
      return route.fulfill({ contentType: "application/json", body: JSON.stringify({ items: rows, next_cursor: null }) });
    }
    if (url.pathname === "/api/v1/knowledge/space-items/item-md/content") {
      return route.fulfill({ contentType: "application/json", headers: { etag: '"0"' }, body: JSON.stringify({ item_id: "item-md", revision: 0, content_format: "markdown", blocks: null, markdown: "审稿意见还在整理。", content_sha256: "a".repeat(64) }) });
    }
    if (route.request().method() === "GET" && url.pathname.startsWith("/api/v1/knowledge/space-items/")) {
      const id = url.pathname.split("/").at(-1);
      const found = ITEMS.find((row) => row.item_id === id);
      return route.fulfill({ status: found ? 200 : 404, contentType: "application/json", body: JSON.stringify(found ?? { code: "not_found" }) });
    }
    return route.fallback();
  });
  await page.route("**/api/v1/knowledge/space-trash**", async (route) => {
    return route.fulfill({ contentType: "application/json", body: JSON.stringify({ items: [], next_cursor: null }) });
  });
}

test("space list filters, creates, and opens a readonly page", async ({ page }) => {
  await installFirstRunHarness(page, {
    configured: true,
    permissions: [
      "console:dashboard:view",
      "knowledge:dataset:view",
      "knowledge:space:view",
      "knowledge:space:create",
      "knowledge:space:edit",
      "knowledge:space:share",
    ],
  });
  await installSpaceApi(page);
  await page.goto("/spaces");
  const main = page.locator(".spaces-main");
  await expect(page.getByRole("heading", { level: 1, name: "Spaces" })).toBeVisible();
  await expect(page.getByRole("heading", { level: 2, name: "All" })).toBeVisible();
  await expect(main.getByRole("link", { name: /C37/ })).toBeVisible();
  await expect(main.getByRole("row", { name: /生活人像/ })).toContainText("高颜值写实人像 · 提示词与样片");
  await page.getByRole("tab", { name: "Shared with you" }).click();
  await expect(main.getByRole("link", { name: /生活人像/ })).toBeVisible();
  await expect(main.getByRole("link", { name: /C37/ })).toHaveCount(0);
  await page.getByRole("tab", { name: "Recommended" }).click();
  await expect(main.getByRole("link", { name: /C37/ })).toBeVisible();
  await page.getByRole("searchbox", { name: "Search items" }).fill("handbook");
  await page.getByRole("searchbox", { name: "Search items" }).press("Enter");
  await expect(main.getByRole("link", { name: /handbook/ })).toBeVisible();
  await expect(main.getByRole("link", { name: /C37/ })).toHaveCount(0);
  await page.getByRole("button", { name: "Grid" }).click();
  await expect(page.getByRole("button", { name: "Grid" })).toHaveAttribute("aria-pressed", "true");
  await page.getByRole("button", { name: "Google Drive" }).click();
  await expect(page.getByRole("heading", { name: "Connect Google Drive" })).toBeVisible();
  await page.getByRole("button", { name: "Close" }).click();
  await page.getByRole("link", { name: "Trash" }).click();
  await expect(page.getByRole("heading", { level: 2, name: "Trash" })).toBeVisible();
  await expect(page.getByText("Trash is empty")).toBeVisible();
  await page.goto("/spaces/item/item-md");
  await expect(page.getByRole("heading", { level: 2, name: /FoRIS/ })).toBeVisible();
  await expect(page.getByText("审稿意见还在整理。")).toBeVisible();

  await page.setViewportSize({ width: 1440, height: 900 });
  await seedClientPrefs(page, { locale: "zh-CN", themeMode: "light", resolvedTheme: "light", darkMode: false });
  await page.goto("/spaces");
  await expect(page.getByRole("heading", { level: 2, name: "全部" })).toBeVisible();
  await page.screenshot({ path: "test-results/spaces-list-light.png", fullPage: true });
  await seedClientPrefs(page, { locale: "zh-CN", themeMode: "dark", resolvedTheme: "dark", darkMode: true });
  await page.goto("/spaces");
  await expect(page.getByRole("heading", { level: 2, name: "全部" })).toBeVisible();
  await page.screenshot({ path: "test-results/spaces-list-dark.png", fullPage: true });
});
