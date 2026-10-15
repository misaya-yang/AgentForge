import { expect, test } from "@playwright/test";
import { buildAuthHeaders, loginThroughApi } from "./support/helpers";

type CatalogModel = { id: string; name: string; provider: string };
type ManagedModel = { model_id: string; provider_id: string; is_enabled: boolean };

const apiUrl = () => process.env.E2E_API_URL;

async function openAuthenticatedAssistant(
  page: import("@playwright/test").Page,
  request: import("@playwright/test").APIRequestContext,
) {
  const { token, user } = await loginThroughApi(request);
  await page.addInitScript(({ authPayload }) => {
    localStorage.setItem("agent-gateway-auth", JSON.stringify(authPayload));
    sessionStorage.removeItem("agent-gateway-auth");
  }, {
    authPayload: {
      state: {
        token, user, isAuthenticated: true,
        forcePasswordChange: false, rememberMe: true,
      },
      version: 0,
    },
  });
  await page.goto("/assistant");
}

async function clearModelPreference(page: import("@playwright/test").Page) {
  await page.evaluate(() => {
    for (const key of Object.keys(localStorage)) {
      if (key === "assistant.lastModelId.v2" || key.startsWith("assistant.lastModelId.v2:")) {
        localStorage.removeItem(key);
      }
    }
  });
  await page.reload();
}

async function startNewChat(page: import("@playwright/test").Page) {
  const newChat = page.getByRole("button", { name: /^(New chat|新对话)$/ }).first();
  if (!(await newChat.isVisible())) {
    await page.locator('button[aria-label="Show history"], button[aria-label="显示历史"]').click();
  }
  await newChat.click();
}

async function mockAssistantCatalog(page: import("@playwright/test").Page) {
  const model = (id: string, name: string, provider: string) => ({
    id, name, provider, context_window: 32768, max_output_tokens: 4096,
    supports_vision: false, supports_tools: true,
  });
  await page.route(/\/api\/v1\/assistant\/models(?:\?|$)/, (route) => route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify({ models: [
      model("qwen3.7-plus", "Qwen 3.7 Plus", "dashscope"),
      model("qwen3.8-flash", "Qwen 3.8 Flash", "dashscope"),
      model("claude-hidden", "Claude Hidden", "anthropic"),
    ] }),
  }));
  await page.route(/\/api\/v1\/assistant\/config(?:\?|$)/, (route) => route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify({
      default_model_id: "qwen3.7-plus",
      available_providers: ["dashscope"],
      kb_enabled: false,
      web_search_enabled: false,
    }),
  }));
}

test("assistant selector shows only active configured models and defaults to Qwen 3.8 Flash", async ({ page, request }) => {
  const headers = await buildAuthHeaders(request);
  const [catalogResponse, configResponse, managedResponse] = await Promise.all([
    request.get(`${apiUrl()}/api/v1/assistant/models`, { headers }),
    request.get(`${apiUrl()}/api/v1/assistant/config`, { headers }),
    request.get(`${apiUrl()}/api/v1/models?include_disabled=true`, { headers }),
  ]);
  expect(catalogResponse.ok()).toBeTruthy();
  expect(configResponse.ok()).toBeTruthy();
  expect(managedResponse.ok()).toBeTruthy();

  const { models } = await catalogResponse.json() as { models: CatalogModel[] };
  const { available_providers: providers } = await configResponse.json() as {
    available_providers: string[];
  };
  const managed = await managedResponse.json() as ManagedModel[];
  const enabled = new Set(managed.filter((model) => model.is_enabled)
    .map((model) => `${model.provider_id}:${model.model_id}`));
  expect(models.every((model) => enabled.has(`${model.provider}:${model.id}`))).toBeTruthy();

  const visible = models.filter((model) => providers.includes(model.provider));
  expect(visible.some((model) => model.id === "qwen3.8-flash"),
    "Qwen 3.8 Flash must be active for the assistant default").toBeTruthy();

  await openAuthenticatedAssistant(page, request);
  await clearModelPreference(page);
  await startNewChat(page);
  const selector = page.getByRole("button", { name: /Qwen 3\.8 Flash/i });
  await expect(selector).toBeVisible();
  await selector.click();

  const items = page.getByRole("menuitem");
  await expect(items).toHaveCount(visible.length);
  for (const model of visible) {
    await expect(items.filter({ hasText: model.name })).toHaveCount(1);
  }
  for (const model of models.filter((candidate) => !providers.includes(candidate.provider))) {
    await expect(items.filter({ hasText: model.name })).toHaveCount(0);
  }
});

test("assistant keeps an explicit available model after refresh and New chat", async ({ page, request }) => {
  await mockAssistantCatalog(page);

  await openAuthenticatedAssistant(page, request);
  await clearModelPreference(page);
  await startNewChat(page);
  const flash = page.getByRole("button", { name: "Qwen 3.8 Flash" });
  await expect(flash).toBeVisible();
  await flash.click();
  await expect(page.getByRole("menuitem")).toHaveCount(2);
  await expect(page.getByRole("menuitem", { name: /Claude Hidden/ })).toHaveCount(0);
  await page.getByRole("menuitem", { name: /Qwen 3\.7 Plus/ }).click();

  const selected = page.getByRole("button", { name: "Qwen 3.7 Plus" });
  await expect(selected).toBeVisible();
  await page.reload();
  await expect(selected).toBeVisible();

  await startNewChat(page);
  await expect(selected).toBeVisible();
});

test("assistant restores a saved conversation model without changing the new-chat default", async ({ page, request }) => {
  const headers = await buildAuthHeaders(request);
  const title = `assistant-model-restore-${Date.now()}`;
  const created = await request.post(`${apiUrl()}/api/v1/sessions`, {
    headers,
    data: {
      service_id: "__builtin_assistant__",
      metadata: { title },
      config: { selected_model: "qwen3.7-plus" },
    },
  });
  expect(created.ok()).toBeTruthy();
  const { session_id: sessionId } = await created.json() as { session_id: string };
  const message = await request.post(`${apiUrl()}/api/v1/sessions/${sessionId}/messages`, {
    headers,
    data: { role: "user", content: "Model restore seed" },
  });
  expect(message.ok()).toBeTruthy();

  await mockAssistantCatalog(page);
  await openAuthenticatedAssistant(page, request);
  await clearModelPreference(page);
  await startNewChat(page);
  await expect(page.getByRole("button", { name: "Qwen 3.8 Flash" })).toBeVisible();

  const session = page.getByRole("button", { name: new RegExp(`^${title}`) });
  if (!(await session.isVisible())) {
    await page.locator('button[aria-label="Show history"], button[aria-label="显示历史"]').click();
  }
  await session.click();
  await expect(page.getByText("Model restore seed")).toBeVisible();
  await expect(page.getByRole("button", { name: "Qwen 3.7 Plus" })).toBeVisible();

  await page.reload();
  await expect(page.getByRole("button", { name: "Qwen 3.7 Plus" })).toBeVisible();
  await startNewChat(page);
  await expect(page.getByRole("button", { name: "Qwen 3.8 Flash" })).toBeVisible();
});
