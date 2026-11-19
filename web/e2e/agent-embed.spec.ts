import fs from "node:fs/promises";
import path from "node:path";

import { expect, test, type Page, type Route } from "@playwright/test";

import { assertNoBlockingA11yIssues } from "./support/helpers";

const PUBLIC_ID = "44444444-4444-4444-8444-444444444444";
// The embed frame is CSP-pinned to its parent origin, and the widget
// postMessage handshake compares origins too. Both must follow the config's
// baseURL, or the iframe is blocked whenever the console is not on :4181.
const BASE = (process.env.E2E_BASE_URL || "http://127.0.0.1:4181").replace(/\/$/, "");
const EMBED_TOKEN = "e1.fixture.signature";
const SCREENSHOTS = path.resolve("../reports/agent-studio/as-07-screenshots");

function json(route: Route, body: unknown, status = 200) {
  return route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) });
}

function embedDocument() {
  return `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/agent-embed.css"><title>Secure Support</title></head><body><main id="agent-embed-root" data-public-id="${PUBLIC_ID}" data-parent-origin="${BASE}" data-embed-token="${EMBED_TOKEN}"></main><script type="module" src="/agent-embed.js"></script></body></html>`;
}

async function installEmbedHarness(page: Page, mode: "launcher" | "inline" = "launcher") {
  const requests: Array<{ url: string; token: string | null; body: string | null }> = [];
  await page.route("**/agent-embed-host", (route) => route.fulfill({
    status: 200,
    contentType: "text/html",
    body: `<!doctype html><html><body><div id="mount"></div><script src="/agent-widget.js" data-agent-id="${PUBLIC_ID}" data-mode="${mode}" data-container="#mount" data-label="Chat with Support"></script></body></html>`,
  }));
  await page.route(`**/embed/agents/${PUBLIC_ID}**`, (route) => route.fulfill({
    status: 200,
    contentType: "text/html",
    headers: {
      "Content-Security-Policy": `default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors ${BASE}; object-src 'none'`,
      "Cache-Control": "no-store",
    },
    body: embedDocument(),
  }));
  await page.route(`**/api/v1/public/agents/${PUBLIC_ID}**`, async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    requests.push({
      url: request.url(),
      token: request.headers()["x-agent-embed-token"] ?? null,
      body: request.postData(),
    });
    if (url.pathname.endsWith("/sessions")) {
      return json(route, {
        session_id: "embed-session-a",
        agent_id: "11111111-1111-4111-8111-111111111111",
        agent_version_id: "22222222-2222-4222-8222-222222222222",
        draft_revision: null,
        publication_id: "33333333-3333-4333-8333-333333333333",
        channel: "embed",
        runtime_fingerprint: "sha256:runtime",
        request_id: "request-session",
      }, 201);
    }
    if (url.pathname.endsWith("/chat/stream")) {
      return route.fulfill({
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
        body: 'data: {"event_type":"TEXT_MESSAGE_CONTENT","content":"Embedded approved answer."}\n\n',
      });
    }
    return json(route, {
      public_id: PUBLIC_ID,
      publication_id: "33333333-3333-4333-8333-333333333333",
      channel: "embed",
      auth_mode: "public",
      name: "Secure Support",
      description: "Origin-bound assistance",
      identity: {
        theme_color: "#4f46e5",
        welcome_message: "Ask a support question",
        suggested_prompts: ["Show the escalation path"],
      },
      attachments: false,
      request_id: "request-config",
    });
  });
  return requests;
}

test("launcher Embed validates channel, streams and returns focus on close", async ({ page }) => {
  const requests = await installEmbedHarness(page);
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/agent-embed-host");
  const launcher = page.getByRole("button", { name: "Chat with Support" });
  await expect(launcher).toBeVisible();
  await launcher.click();

  const frame = page.frameLocator(`iframe[title="Chat with Support"]`);
  await expect(frame.getByRole("heading", { name: "Ask a support question" })).toBeVisible();
  await frame.getByRole("button", { name: "Show the escalation path" }).click();
  await expect(frame.getByText("Embedded approved answer.")).toBeVisible();
  await frame.getByRole("button", { name: "Close chat" }).click();
  await expect(launcher).toBeFocused();

  expect(requests.length).toBeGreaterThanOrEqual(3);
  expect(requests.every((request) => request.token === EMBED_TOKEN)).toBe(true);
  const serialized = JSON.stringify(requests);
  expect(serialized).not.toContain("agt_");
  expect(serialized).not.toContain("resolved_snapshot");
  expect(serialized).not.toContain("GATEWAY_ASSISTANT_SHARED_SECRET");
  await assertNoBlockingA11yIssues(page, ["body"]);

  await fs.mkdir(SCREENSHOTS, { recursive: true });
  await page.screenshot({ path: path.join(SCREENSHOTS, "embed-launcher.png"), fullPage: true });
});

test("inline Embed resizes on mobile and ignores wrong protocol messages", async ({ page }) => {
  await installEmbedHarness(page, "inline");
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/agent-embed-host");
  const iframe = page.locator("iframe");
  await expect(iframe).toBeVisible();
  const frame = page.frameLocator("iframe");
  await expect(frame.getByText("Ask a support question")).toBeVisible();

  await page.locator("iframe").evaluate((node) => {
    (node as HTMLIFrameElement).contentWindow?.postMessage({
      version: "wrong-version",
      type: "new_message",
      payload: { message: "must be ignored" },
    }, window.location.origin);
  });
  await expect(frame.getByText("must be ignored")).toHaveCount(0);
  const width = await iframe.evaluate((node) => node.getBoundingClientRect().width);
  expect(width).toBeLessThanOrEqual(390);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);

  await fs.mkdir(SCREENSHOTS, { recursive: true });
  await page.screenshot({ path: path.join(SCREENSHOTS, "embed-mobile-inline.png"), fullPage: true });
});

test("rejected origin fixture never initializes an Embed document", async ({ page }) => {
  await page.route(`**/embed/agents/${PUBLIC_ID}**`, (route) => json(route, {
    detail: { code: "AGENT_EMBED_ORIGIN_FORBIDDEN", message: "Parent origin is not allowed" },
  }, 403));
  const response = await page.goto(`/embed/agents/${PUBLIC_ID}`);
  expect(response?.status()).toBe(403);
  await expect(page.locator("body")).toContainText("AGENT_EMBED_ORIGIN_FORBIDDEN");
  await expect(page.locator("script[src='/agent-embed.js']")).toHaveCount(0);
});
