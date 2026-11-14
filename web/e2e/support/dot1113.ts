import type { Page } from "@playwright/test";
import { installFirstRunHarness } from "./first-run";

const now = "2026-11-13T12:00:00Z";
export const space = { space_id: "space-design", name: "Product team", icon: "", kind: "team", visibility: "members", dataset_id: "hidden-design", created_by: "first-run-user", created_at: now, updated_at: now };
export const items = ["Project handbook", "Launch checklist", "Research notes", "Design decisions"].map((title, index) => ({
  item_id: `item-${index}`, space_id: space.space_id, space_name: space.name, parent_id: index === 3 ? "item-0" : null,
  title, kind: "page", icon: "", position: `a${index}`, source: "native", created_by: "first-run-user", updated_by: "first-run-user",
  content_format: "blocknote_json", current_revision: 1, starred: index === 0, shared: false, updated_at: now, last_activity_at: now, deleted_at: null,
}));
export const entries = [
  ["slack", "Slack", "communication", "Read channels and share approved updates."],
  ["github", "GitHub", "development", "Repositories, issues, and pull requests."],
  ["notion", "Notion", "office", "Search and work with your team’s documents."],
  ["google-drive", "Google Drive", "office", "Find files and bring them into your workspace."],
  ["space-writer", "Space writer", "more", "Draft and update pages in your spaces."],
  ["jira", "Jira", "development", "Track projects and follow team progress."],
].map(([id, name, category, description]) => ({ id, name, category, description, kind: id === "space-writer" ? "skill" : "connector", icon: `/catalog/${id}.svg`, auth_kind: id === "space-writer" ? "none" : "oauth" }));

/** Deterministic public fixtures only: screenshots never capture an account or secrets. */
export async function installDot1113Fixture(page: Page) {
  await installFirstRunHarness(page, { configured: true, roles: ["admin"], permissions: ["admin:*", "console:dashboard:view", "console:settings:view", "knowledge:dataset:view", "knowledge:space:view", "knowledge:space:edit", "conversation:assistant:access", "connector:view", "connector:connect"] });
  const respond = (value: unknown) => ({ contentType: "application/json", body: JSON.stringify(value) });
  await page.route("**/api/v1/catalog**", route => route.fulfill(respond({ entries })));
  await page.route("**/api/v1/connections**", route => route.fulfill(respond({ connections: [{ connection_id: "connection-1", entry_id: "github", owner_kind: "user", owner_id: "first-run-user", status: "connected" }] })));
  await page.route("**/api/v1/local-nodes**", route => route.fulfill(respond({ nodes: [] })));
  await page.route("**/api/v1/knowledge/**", route => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith("/spaces")) return route.fulfill(respond({ spaces: [space] }));
    if (path.endsWith("/tree")) return route.fulfill(respond({ items }));
    if (path.endsWith("/content")) return route.fulfill({ ...respond({ item_id: "item-0", revision: 1, blocks: [{ id: "intro", type: "paragraph", content: "A shared home for the team’s plans, decisions, and research.", children: [] }], markdown: "A shared home for the team’s plans, decisions, and research.", content_format: "blocknote_json", content_sha256: "a".repeat(64) }), headers: { etag: '"1"' } });
    if (path.endsWith("/revisions")) return route.fulfill(respond({ revisions: [{ revision: 1, author: "first-run-user", created_at: now, content_sha256: "a".repeat(64) }] }));
    if (path.endsWith("/backlinks") || path.endsWith("/space-trash")) return route.fulfill(respond({ items: [] }));
    if (path.endsWith("/space-items")) return route.fulfill(respond({ items, next_cursor: null }));
    if (path.includes("/space-items/")) return route.fulfill(respond(items[0]));
    if (path.endsWith("/datasets")) return route.fulfill(respond({ datasets: [], total: 0, items: [] }));
    return route.fallback();
  });
}
