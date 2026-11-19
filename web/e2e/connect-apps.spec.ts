import { expect, test } from "@playwright/test";
import { installFirstRunHarness } from "./support/first-run";

const ENTRIES = [
  { id: "slack", kind: "connector", name: "Slack", description: "Read channels and post approved messages.", category: "communication", icon: "/catalog/slack.svg", auth_kind: "oauth" },
  { id: "github", kind: "connector", name: "GitHub", description: "Repositories, issues, and pull requests.", category: "development", icon: "/catalog/github.svg", auth_kind: "oauth" },
  { id: "agentmail", kind: "connector", name: "AgentMail", description: "Send and read mail with an API key.", category: "communication", icon: "/catalog/agentmail.svg", auth_kind: "api_key" },
  { id: "space-writer", kind: "skill", name: "Space writer", description: "Draft and update a page in a knowledge space.", category: "more", icon: "/catalog/space-writer.svg", auth_kind: "none" },
  { id: "support-bot", kind: "bot_template", name: "Support bot", description: "A bot template that answers from connected knowledge.", category: "more", icon: "/catalog/support-bot.svg", auth_kind: "none" },
  { id: "local-files", kind: "connector", name: "Files", description: "Files on this computer.", category: "local", icon: "/catalog/local-files.svg", auth_kind: "none" },
];

test("connect apps dialog searches, filters, and saves a connection", async ({ page }) => {
  const connections: Array<Record<string, string>> = [];
  await installFirstRunHarness(page, {
    configured: true,
    permissions: ["console:dashboard:view", "console:settings:view", "connector:view", "connector:connect", "connector:share", "connector:admin"],
  });
  await page.route("**/api/v1/catalog**", async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname !== "/api/v1/catalog") return route.fallback();
    return route.fulfill({ contentType: "application/json", body: JSON.stringify({ entries: ENTRIES }) });
  });
  await page.route("**/api/v1/connections**", async (route) => {
    const url = new URL(route.request().url());
    if (route.request().method() === "GET" && url.pathname === "/api/v1/connections") {
      return route.fulfill({ contentType: "application/json", body: JSON.stringify({ connections }) });
    }
    if (route.request().method() === "POST" && url.pathname === "/api/v1/connections") {
      const body = route.request().postDataJSON() as { entry_id?: string };
      connections.push({ connection_id: "conn-1", entry_id: body.entry_id ?? "", owner_kind: "user", owner_id: "first-run-user", status: "connected" });
      return route.fulfill({ status: 201, contentType: "application/json", body: JSON.stringify(connections[0]) });
    }
    if (url.pathname.endsWith("/oauth/start")) {
      return route.fulfill({ contentType: "application/json", body: JSON.stringify({ authorization_url: "https://example.test/oauth", connection_id: "conn-oauth", status: "connecting" }) });
    }
    return route.fallback();
  });
  await page.route("**/api/v1/local-nodes**", async (route) => {
    return route.fulfill({ contentType: "application/json", body: JSON.stringify({ nodes: [{ node_id: "node-1", status: "online" }] }) });
  });

  await page.goto("/dashboard");
  await page.getByRole("button", { name: "Connect apps" }).click();
  const dialog = page.getByRole("dialog", { name: "Connect apps" });
  await expect(dialog.getByRole("heading", { name: "Recommended for you" })).toBeVisible();
  await expect(dialog).toHaveAttribute("closedby", "any");
  await expect(dialog).toHaveCSS("position", "fixed");
  await expect(dialog.getByRole("searchbox")).toBeFocused();
  await dialog.getByRole("radio", { name: "All", exact: true }).focus();
  await page.keyboard.press("ArrowRight");
  await expect(dialog.getByRole("radio", { name: "This computer", exact: true })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(dialog).not.toBeVisible();
  await expect(page.getByRole("button", { name: "Connect apps", exact: true })).toBeFocused();
  await page.getByRole("button", { name: "Connect apps", exact: true }).click();
  await dialog.getByRole("radio", { name: "All", exact: true }).click();
  await expect(dialog.getByText("Connected 0")).toBeVisible();
  await expect(dialog.getByRole("heading", { name: "This computer" })).toBeVisible();
  await expect(dialog.getByText("Files", { exact: true })).toBeVisible();
  await dialog.getByRole("searchbox", { name: "Search apps and skills" }).fill("space writer");
  await expect(dialog.getByText("Space writer")).toBeVisible();
  await expect(dialog.getByText("Slack")).toHaveCount(0);
  await dialog.getByRole("searchbox", { name: "Search apps and skills" }).fill("");
  await dialog.getByRole("radio", { name: "Development" }).click();
  await expect(dialog.getByText("GitHub")).toBeVisible();
  await expect(dialog.getByText("Slack")).toHaveCount(0);
  await dialog.getByRole("radio", { name: "All" }).click();
  await dialog.getByRole("link", { name: "Bot templates" }).click();
  await expect(dialog.getByText("Support bot")).toBeVisible();
  await expect(dialog.getByText("Slack")).toHaveCount(0);
  await dialog.getByRole("radio", { name: "All" }).click();
  const agent = dialog.locator("article").filter({ hasText: "AgentMail" });
  await agent.getByRole("button", { name: "Connect" }).click();
  await agent.getByLabel("API key or token").fill("test-token");
  await agent.getByRole("button", { name: "Save connection" }).click();
  await expect(agent.getByText("Connected", { exact: true })).toBeVisible();
  await page.goto("/settings/connectors");
  await expect(page.getByRole("heading", { name: "Connectors" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Shared connections" })).toBeVisible();
});
