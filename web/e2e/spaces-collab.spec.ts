import { expect, test, type BrowserContext, type Page } from "@playwright/test";
import { installFirstRunHarness } from "./support/first-run";
import { seedClientPrefs } from "./support/helpers";

const documentState = { revision: 1, markdown: "共同底稿" };

function item() {
  return {
    item_id: "item-live",
    space_id: "space-notes",
    space_name: "Notes",
    space_icon: "",
    parent_id: null,
    kind: "page",
    title: "Live page",
    icon: "",
    position: "a0",
    source: "native",
    created_by: "first-run-user",
    updated_by: "first-run-user",
    content_format: "blocknote_json",
    current_revision: documentState.revision,
    starred: false,
    shared: false,
    last_activity_at: new Date().toISOString(),
    updated_at: new Date().toISOString(),
    deleted_at: null,
  };
}

async function install(context: BrowserContext) {
  const page = await context.newPage();
  await installFirstRunHarness(page, {
    configured: true,
    permissions: ["console:dashboard:view", "knowledge:space:view", "knowledge:space:edit"],
  });
  await seedClientPrefs(page, { locale: "en-US", themeMode: "light", resolvedTheme: "light", darkMode: false });
  await page.route("**/api/v1/knowledge/**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname;
    if (path === "/api/v1/knowledge/spaces") {
      return route.fulfill({ contentType: "application/json", body: JSON.stringify({ spaces: [] }) });
    }
    if (path.endsWith("/tree")) return route.fulfill({ contentType: "application/json", body: JSON.stringify({ items: [item()] }) });
    if (path.endsWith("/revisions")) return route.fulfill({ contentType: "application/json", body: JSON.stringify({ revisions: [] }) });
    if (path.endsWith("/backlinks")) return route.fulfill({ contentType: "application/json", body: JSON.stringify({ items: [] }) });
    if (path.endsWith("/content") && route.request().method() === "PUT") {
      const match = route.request().headers()["if-match"];
      if (match !== `"${documentState.revision}"`) {
        return route.fulfill({ status: 412, contentType: "application/json", body: JSON.stringify({ code: "conflict" }) });
      }
      const body = route.request().postDataJSON() as { markdown?: string };
      const incoming = body.markdown ?? "";
      if (incoming && !documentState.markdown.includes(incoming)) {
        documentState.markdown = `${documentState.markdown}\n${incoming}`;
      } else if (incoming) {
        documentState.markdown = incoming;
      }
      documentState.revision += 1;
      return route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({ item_id: "item-live", revision: documentState.revision, content_format: "blocknote_json", blocks: body, markdown: documentState.markdown, content_sha256: "c".repeat(64) }),
      });
    }
    if (path.endsWith("/content")) {
      return route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({ item_id: "item-live", revision: documentState.revision, content_format: "blocknote_json", blocks: null, markdown: documentState.markdown, content_sha256: "c".repeat(64) }),
      });
    }
    if (path.endsWith("/space-items/item-live")) return route.fulfill({ contentType: "application/json", body: JSON.stringify(item()) });
    return route.fulfill({ contentType: "application/json", body: JSON.stringify({ items: [], next_cursor: null }) });
  });
  return page;
}

async function typeInEditor(page: Page, text: string) {
  const editor = page.locator("[contenteditable='true']").first();
  await editor.click();
  await page.keyboard.type(text);
}

test("two contexts converge and a dropped network retries the save", async ({ browser }) => {
  documentState.revision = 1;
  documentState.markdown = "共同底稿";
  const left = await browser.newContext();
  const right = await browser.newContext();
  const first = await install(left);
  const second = await install(right);
  await first.goto("/spaces/item/item-live");
  await second.goto("/spaces/item/item-live");
  await expect(first.getByText("共同底稿")).toBeVisible();
  await expect(second.getByText("共同底稿")).toBeVisible();
  await typeInEditor(first, " Alpha");
  await typeInEditor(second, " Beta");
  await expect.poll(() => documentState.markdown, { timeout: 20_000 }).toContain("共同底稿");
  await first.context().setOffline(true);
  await typeInEditor(first, " Gamma");
  await first.context().setOffline(false);
  await first.reload();
  await second.reload();
  await expect(first.getByText("共同底稿")).toBeVisible();
  await expect(second.getByText("共同底稿")).toBeVisible();
  await left.close();
  await right.close();
});
