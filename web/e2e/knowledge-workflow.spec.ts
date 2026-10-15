import { expect, test } from "@playwright/test";
import type { APIRequestContext, APIResponse } from "@playwright/test";
import { buildAuthHeaders, ensureAuthenticatedPage, getApiUrl, readE2ETestUser } from "./support/helpers";

const DATASET_PREFIX = "e2e-kb-test";

function uniqueName(): string {
  return `${DATASET_PREFIX}-${Date.now()}`;
}

/**
 * Delete a dataset, absorbing the retryable index-lease conflict.
 *
 * Deletion takes the same index lifecycle lease as ingestion, so a delete
 * issued while the freshly uploaded document is still being indexed loses the
 * race and comes back 409. That is expected contention, not a failure — retry
 * until the indexer releases the lease.
 */
async function deleteDatasetWithRetry(
  request: APIRequestContext,
  apiUrl: string,
  datasetId: string,
  headers: Record<string, string>,
  password: string,
  reason: string,
): Promise<APIResponse> {
  let response!: APIResponse;
  for (let attempt = 0; attempt < 15; attempt += 1) {
    response = await request.delete(`${apiUrl}/api/v1/knowledge/datasets/${datasetId}`, {
      headers,
      data: { password, reason },
    });
    if (response.status() !== 409) return response;
    await new Promise((resolve) => setTimeout(resolve, 1_000));
  }
  return response;
}

test.describe("Knowledge Base workflow", () => {
  test.setTimeout(3 * 60_000);

  let datasetId: string | undefined;
  let datasetName: string;
  let headers: Record<string, string>;
  let testPassword: string;

  test.beforeAll(async ({ request }) => {
    testPassword = (await readE2ETestUser()).password;
    headers = await buildAuthHeaders(request);
  });

  test.afterAll(async ({ request }) => {
    // Clean up: delete dataset if it was created
    if (datasetId) {
      const apiUrl = getApiUrl();
      await deleteDatasetWithRetry(
        request,
        apiUrl,
        datasetId,
        headers,
        testPassword,
        "E2E cleanup",
      ).catch(() => {});
    }
  });

  test("full KB lifecycle: create, upload, verify, update, delete", async ({ page, request }) => {
    const apiUrl = getApiUrl();
    datasetName = uniqueName();

    // --- Step 1: Create dataset via API ---
    const createRes = await request.post(`${apiUrl}/api/v1/knowledge/datasets`, {
      headers,
      data: {
        name: datasetName,
        description: "E2E test dataset",
        embedding_provider: "local",
        embedding_model: "hash-384",
        embedding_dimension: 384,
      },
    });
    expect(createRes.ok(), `Create dataset failed: ${createRes.status()}`).toBeTruthy();
    const created = await createRes.json();
    datasetId = created.id ?? created.dataset_id ?? created.data?.id;
    expect(datasetId).toBeTruthy();

    // --- Step 2: Navigate to KB list and verify dataset appears ---
    await ensureAuthenticatedPage(page, "/knowledge");
    const mainContent = page.locator(".ant-layout-content, main").first();
    await expect(mainContent).toBeVisible();
    await expect(page.getByText(datasetName)).toBeVisible({ timeout: 15_000 });

    // --- Step 3: Upload a text document via API ---
    const docRes = await request.post(`${apiUrl}/api/v1/knowledge/${datasetId}/documents/text`, {
      headers,
      data: {
        title: "E2E Test Document",
        content: "This is a test document created by the Playwright E2E suite. Bismillah.",
      },
    });
    expect(docRes.ok(), `Create document failed: ${docRes.status()}`).toBeTruthy();

    // --- Step 4: Verify document appears in API ---
    const docsListRes = await request.get(`${apiUrl}/api/v1/knowledge/${datasetId}/documents`, { headers });
    expect(docsListRes.ok()).toBeTruthy();
    const docsList = await docsListRes.json();
    const docs = Array.isArray(docsList) ? docsList : docsList.data ?? docsList.documents ?? [];
    expect(docs.length).toBeGreaterThanOrEqual(1);

    // --- Step 5: Navigate to dataset detail page ---
    await page.goto(`/knowledge/${datasetId}`, { waitUntil: "domcontentloaded" });
    await expect(mainContent).toBeVisible();
    // Verify document title or some content is shown
    await expect(page.getByText("E2E Test Document")).toBeVisible({ timeout: 15_000 });

    // --- Step 6: Update dataset metadata via API ---
    const updatedDescription = "E2E test dataset updated";
    const updateRes = await request.put(`${apiUrl}/api/v1/knowledge/datasets/${datasetId}`, {
      headers,
      data: { description: updatedDescription },
    });
    expect(updateRes.ok(), `Update dataset failed: ${updateRes.status()}`).toBeTruthy();

    const getAfterUpdate = await request.get(`${apiUrl}/api/v1/knowledge/datasets/${datasetId}`, { headers });
    expect(getAfterUpdate.ok()).toBeTruthy();
    const dsUpdated = await getAfterUpdate.json();
    const updatedData = dsUpdated.data ?? dsUpdated;
    expect(updatedData.description).toBe(updatedDescription);

    // --- Step 8: Delete dataset via API ---
    const deleteRes = await deleteDatasetWithRetry(
      request,
      apiUrl,
      datasetId,
      headers,
      testPassword,
      "E2E lifecycle complete",
    );
    expect(deleteRes.ok(), `Delete dataset failed: ${deleteRes.status()}`).toBeTruthy();
    datasetId = undefined; // prevent afterAll cleanup

    // --- Step 9: Verify dataset no longer in list ---
    await page.goto("/knowledge", { waitUntil: "domcontentloaded" });
    await expect(mainContent).toBeVisible();
    // Wait briefly for list to load, then confirm dataset is gone
    await page.waitForTimeout(2_000);
    await expect(page.getByText(datasetName)).toHaveCount(0);
  });
});
