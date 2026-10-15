import { expect, test, type APIRequestContext } from "@playwright/test";
import { buildAuthHeaders, ensureAuthenticatedPage } from "./support/helpers";

function sessionButtonName(title: string): RegExp {
  return new RegExp(`^${title.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\s`);
}

async function getFirstServiceId(request: APIRequestContext): Promise<string> {
  const headers = await buildAuthHeaders(request);
  const response = await request.get(`${process.env.E2E_API_URL}/api/v1/proxy`, { headers });
  expect(response.ok()).toBeTruthy();
  const payload = (await response.json()) as { services?: Array<{ service_id: string; enabled?: boolean }> };
  const serviceId = payload.services?.find((service) => service.enabled !== false)?.service_id;
  if (!serviceId) {
    test.skip(true, "No services configured for playground history test");
  }
  return serviceId;
}

async function seedPlaygroundSession(request: APIRequestContext) {
  const headers = await buildAuthHeaders(request);
  const serviceId = await getFirstServiceId(request);
  const title = `playground-history-${Date.now()}`;
  const createResponse = await request.post(`${process.env.E2E_API_URL}/api/v1/sessions`, {
    headers,
    data: {
      service_id: serviceId,
      metadata: { title },
    },
  });
  expect(createResponse.ok()).toBeTruthy();
  const { session_id: sessionId } = (await createResponse.json()) as { session_id: string };

  await request.post(`${process.env.E2E_API_URL}/api/v1/sessions/${sessionId}/messages`, {
    headers,
    data: { role: "user", content: "Playground history question" },
  });
  await request.post(`${process.env.E2E_API_URL}/api/v1/sessions/${sessionId}/messages`, {
    headers,
    data: { role: "assistant", content: "Playground history answer" },
  });

  return { serviceId, title };
}

test("playground restores seeded history and keeps mobile history reachable", async ({ page, request }) => {
  const { title } = await seedPlaygroundSession(request);

  await page.setViewportSize({ width: 390, height: 844 });
  await ensureAuthenticatedPage(page, "/playground");

  const sessionButton = page.getByRole("button", { name: sessionButtonName(title) }).first();
  const sessionVisible = await expect(sessionButton)
    .toBeVisible({ timeout: 3000 })
    .then(() => true)
    .catch(() => false);

  if (!sessionVisible) {
    await page.getByRole("button", { name: /show history|显示历史/i }).first().click();
    await expect(sessionButton).toBeVisible();
  }

  await sessionButton.click();

  await expect(page.getByText("Playground history question")).toBeVisible();
  await expect(page.getByText("Playground history answer")).toBeVisible();
});
