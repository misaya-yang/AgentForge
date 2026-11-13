import { expect, test, type Page, type Route } from "@playwright/test";
import { installFirstRunHarness } from "./support/first-run";
import { seedClientPrefs } from "./support/helpers";

const item = {
  item_id: "item-md",
  space_id: "space-notes",
  space_name: "Notes",
  space_icon: "",
  parent_id: null,
  kind: "page",
  title: "FoRIS notes",
  icon: "",
  position: "a0",
  source: "native",
  created_by: "first-run-user",
  updated_by: "first-run-user",
  content_format: "blocknote_json",
  current_revision: 1,
  starred: false,
  shared: false,
  last_activity_at: new Date().toISOString(),
  updated_at: new Date().toISOString(),
  deleted_at: null,
};

const blocks = [{
  id: "seed",
  type: "paragraph",
  props: { textColor: "default", backgroundColor: "default", textAlignment: "left" },
  content: [{ type: "text", text: "审稿意见还在整理。", styles: {} }],
  children: [],
}];

async function installEditorApi(page: Page, revision = { current: 1 }) {
  await page.route("**/api/v1/knowledge/spaces**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (route.request().method() === "GET" && path === "/api/v1/knowledge/spaces") {
      return route.fulfill({ contentType: "application/json", body: JSON.stringify({ spaces: [{ space_id: "space-notes", name: "Notes", icon: "", kind: "personal", visibility: "private", dataset_id: "hidden", created_by: "first-run-user", created_at: new Date().toISOString(), updated_at: new Date().toISOString() }] }) });
    }
    if (path.endsWith("/tree")) {
      return route.fulfill({ contentType: "application/json", body: JSON.stringify({ items: [item, { ...item, item_id: "item-child", parent_id: "item-md", title: "Child", position: "a1" }] }) });
    }
    return route.fallback();
  });
  await page.route("**/api/v1/knowledge/space-items**", async (route: Route) => {
    const url = new URL(route.request().url());
    if (url.pathname.endsWith("/content") && route.request().method() === "PUT") {
      const match = route.request().headers()["if-match"];
      if (match !== `"${revision.current}"`) {
        return route.fulfill({ status: 412, contentType: "application/json", body: JSON.stringify({ code: "conflict" }) });
      }
      revision.current += 1;
      const body = route.request().postDataJSON() as { markdown?: string; blocks?: unknown };
      return route.fulfill({ contentType: "application/json", headers: { etag: `"${revision.current}"` }, body: JSON.stringify({ item_id: "item-md", revision: revision.current, content_format: "blocknote_json", blocks: body.blocks, markdown: body.markdown ?? "", content_sha256: "b".repeat(64) }) });
    }
    if (url.pathname.endsWith("/content")) {
      return route.fulfill({ contentType: "application/json", headers: { etag: `"${revision.current}"` }, body: JSON.stringify({ item_id: "item-md", revision: revision.current, content_format: "blocknote_json", blocks, markdown: "审稿意见还在整理。", content_sha256: "a".repeat(64) }) });
    }
    if (url.pathname.endsWith("/revisions")) {
      return route.fulfill({ contentType: "application/json", body: JSON.stringify({ revisions: [{ revision: 1, author: "first-run-user", created_at: new Date().toISOString(), content_sha256: "a".repeat(64) }] }) });
    }
    if (url.pathname.endsWith("/backlinks")) {
      return route.fulfill({ contentType: "application/json", body: JSON.stringify({ items: [] }) });
    }
    if (url.pathname.endsWith("/export")) {
      return route.fulfill({ contentType: "application/json", body: JSON.stringify({ body: "审稿意见还在整理。", filename: "item-md.md", format: "markdown" }) });
    }
    if (route.request().method() === "GET" && url.pathname === "/api/v1/knowledge/space-items") {
      return route.fulfill({ contentType: "application/json", body: JSON.stringify({ items: [item], next_cursor: null }) });
    }
    if (route.request().method() === "GET") {
      return route.fulfill({ contentType: "application/json", body: JSON.stringify(item) });
    }
    return route.fallback();
  });
  await page.route("**/api/v1/knowledge/space-trash**", async (route) => {
    return route.fulfill({ contentType: "application/json", body: JSON.stringify({ items: [], next_cursor: null }) });
  });
}

test("editor chunk stays off the first screen and a page can be edited", async ({ page }) => {
  const editorRequests: string[] = [];
  page.on("request", (request) => {
    if (request.url().includes("BlockEditor") || request.url().includes("blocknote")) editorRequests.push(request.url());
  });
  await installFirstRunHarness(page, {
    configured: true,
    permissions: ["console:dashboard:view", "knowledge:space:view", "knowledge:space:edit"],
  });
  await seedClientPrefs(page, { locale: "zh-CN", themeMode: "light", resolvedTheme: "light", darkMode: false });
  await installEditorApi(page);
  await page.goto("/spaces");
  await expect(page.getByRole("heading", { level: 2, name: "全部" })).toBeVisible();
  expect(editorRequests).toEqual([]);
  await page.goto("/spaces/item/item-md");
  await expect(page.getByRole("heading", { level: 2, name: /FoRIS/ })).toBeVisible();
  await expect(page.locator("[data-editor-chunk='blocknote']")).toBeVisible();
  expect(editorRequests.length).toBeGreaterThan(0);
});
