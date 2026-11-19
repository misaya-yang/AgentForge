import fs from "node:fs/promises";
import path from "node:path";

import { expect, test, type Page, type Route } from "@playwright/test";

import { assertNoBlockingA11yIssues, seedClientPrefs } from "./support/helpers";

const PUBLIC_ID = "44444444-4444-4444-8444-444444444444";
const PUBLICATION_ID = "33333333-3333-4333-8333-333333333333";
const SCREENSHOTS = path.resolve("../reports/agent-studio/as-07-screenshots");

function json(route: Route, body: unknown, status = 200) {
  return route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) });
}

async function installHostedApi(page: Page, options: { streamStatus?: number; attachments?: boolean } = {}) {
  const requests: Array<{ url: string; body: string | null; authorization: string | null }> = [];
  await page.route(`**/api/v1/public/agents/${PUBLIC_ID}**`, async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    requests.push({
      url: request.url(),
      body: request.postData(),
      authorization: request.headers().authorization ?? null,
    });
    if (url.pathname.endsWith("/attachments")) {
      return json(route, {
        artifact_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        filename: "policy.txt",
        mime_type: "text/plain",
        size_bytes: 18,
        expires_at: "2026-07-19T00:00:00Z",
        request_id: "request-attachment",
      }, 201);
    }
    if (url.pathname.endsWith("/sessions")) {
      return json(route, {
        session_id: "hosted-session-a",
        agent_id: "11111111-1111-4111-8111-111111111111",
        agent_version_id: "22222222-2222-4222-8222-222222222222",
        draft_revision: null,
        publication_id: PUBLICATION_ID,
        channel: "hosted",
        runtime_fingerprint: "sha256:runtime",
        request_id: "request-session",
      }, 201);
    }
    if (url.pathname.endsWith("/chat/stream")) {
      if (options.streamStatus) {
        return json(route, {
          detail: { code: "AGENT_RUNTIME_QUOTA_EXCEEDED", message: "Daily quota exceeded" },
        }, options.streamStatus);
      }
      return route.fulfill({
        status: 200,
        headers: { "Content-Type": "text/event-stream", "Cache-Control": "no-cache" },
        body: [
          'data: {"event_type":"context_retrieved","data":{"dataset_id":"dataset-refund","dataset_name":"Refund policy","citation_count":2}}',
          'data: {"event_type":"TEXT_MESSAGE_CONTENT","content":"Use the approved "}',
          'data: {"event_type":"TEXT_MESSAGE_CONTENT","content":"support policy."}',
        ].join("\n\n") + "\n\n",
      });
    }
    if (url.pathname.endsWith("/feedback")) {
      return json(route, { feedback_id: "feedback-a", session_id: "hosted-session-a", rating: 1 });
    }
    return json(route, {
      public_id: PUBLIC_ID,
      publication_id: PUBLICATION_ID,
      channel: "hosted",
      auth_mode: "public",
      name: "Support Guide",
      description: "Answers from approved support policy.",
      identity: {
        theme_color: "#635bff",
        welcome_message: "What support question can I help with?",
        suggested_prompts: ["Summarize the refund policy", "Explain escalation rules"],
      },
      attachments: options.attachments ?? true,
      request_id: "request-config",
    });
  });
  return requests;
}

test.beforeEach(async ({ page }) => {
  await seedClientPrefs(page, { locale: "en-US", themeMode: "light", resolvedTheme: "light" });
});

test("public Hosted page streams, gives feedback, stays redacted and accessible", async ({ page }) => {
  const requests = await installHostedApi(page);
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(`/a/${PUBLIC_ID}`);

  await expect(page.getByRole("heading", { name: "Support Guide" })).toBeVisible();
  await expect(page.getByText("What support question can I help with?")).toBeVisible();
  await page.getByRole("button", { name: "Summarize the refund policy" }).click();
  await expect(page.getByText("Use the approved support policy.")).toBeVisible();
  await expect(page.getByText("2 sources · Refund policy")).toBeVisible();
  await page.getByRole("button", { name: "Helpful", exact: true }).click();

  await assertNoBlockingA11yIssues(page, [".agent-public-shell"]);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - innerWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  const serialized = JSON.stringify(requests);
  expect(serialized).not.toContain("agt_");
  expect(serialized).not.toContain("resolved_spec");
  expect(serialized).not.toContain("GATEWAY_ASSISTANT_SHARED_SECRET");

  await fs.mkdir(SCREENSHOTS, { recursive: true });
  await page.screenshot({ path: path.join(SCREENSHOTS, "hosted-desktop.png"), fullPage: true });
});

test("Hosted mobile layout supports keyboard send, new chat and no horizontal overflow", async ({ page }) => {
  await installHostedApi(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(`/a/${PUBLIC_ID}`);
  const composer = page.getByPlaceholder("Message this agent");
  await composer.fill("What is the escalation path?");
  await composer.press("Enter");
  await expect(page.getByText("Use the approved support policy.")).toBeVisible();
  await page.getByRole("button", { name: "New chat" }).click();
  await expect(page.getByText("What support question can I help with?")).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);

  await fs.mkdir(SCREENSHOTS, { recursive: true });
  await page.screenshot({ path: path.join(SCREENSHOTS, "hosted-mobile.png"), fullPage: true });
});

test("Hosted presents stable disabled and quota errors without internal detail", async ({ page }) => {
  await installHostedApi(page, { streamStatus: 429 });
  await page.goto(`/a/${PUBLIC_ID}`);
  await page.getByPlaceholder("Message this agent").fill("Use more quota");
  await page.getByRole("button", { name: "Send message" }).click();
  await expect(page.getByRole("alert")).toContainText("429");
  await expect(page.locator("body")).not.toContainText("Traceback");
  await expect(page.locator("body")).not.toContainText("resolved_snapshot");
});

test("Hosted uploads allowed attachments and sends only opaque handles", async ({ page }) => {
  const requests = await installHostedApi(page, { attachments: true });
  await page.goto(`/a/${PUBLIC_ID}`);
  await page.locator('input[type="file"]').setInputFiles({
    name: "policy.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("approved policy"),
  });
  await expect(page.getByText("policy.txt", { exact: true })).toBeVisible();
  await page.getByPlaceholder("Message this agent").fill("Summarize the file");
  await page.getByRole("button", { name: "Send message" }).click();
  await expect(page.getByText("Use the approved support policy.")).toBeVisible();
  const chat = requests.find((request) => request.url.endsWith("/chat/stream"));
  expect(chat?.body).toContain("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
  expect(chat?.body).not.toContain("/uploads/");

  const disabledPage = await page.context().newPage();
  await seedClientPrefs(disabledPage, { locale: "en-US", themeMode: "light", resolvedTheme: "light" });
  await installHostedApi(disabledPage, { attachments: false });
  await disabledPage.goto(`/a/${PUBLIC_ID}`);
  await expect(disabledPage.locator('button[aria-label="Attach a file"]')).toBeDisabled();
  await disabledPage.close();
});

test("private Hosted publication sends unauthenticated callers to login", async ({ page }) => {
  await page.route(`**/api/v1/public/agents/${PUBLIC_ID}**`, (route) => json(route, {
    detail: { code: "PUBLICATION_AUTHENTICATION_REQUIRED", message: "Authentication required" },
  }, 401));
  await page.goto(`/a/${PUBLIC_ID}`).catch(() => null);
  await expect.poll(() => page.url()).toContain("/login");
});
