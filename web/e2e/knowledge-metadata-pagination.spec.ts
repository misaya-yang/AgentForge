import { expect, test } from "@playwright/test";

import { installClientAuth, seedClientPrefs } from "./support/helpers";


const DATASET_ID = "mock-metadata-pagination";

function jsonResponse(body: unknown, status = 200, headers: Record<string, string> = {}) {
  return { status, contentType: "application/json", headers, body: JSON.stringify(body) };
}

test("@mock paginates with exact total and persists typed metadata across reload", async ({
  page,
}) => {
  const documents = Array.from({ length: 51 }, (_, index) => ({
    document_id: `doc-${index.toString().padStart(2, "0")}`,
    dataset_id: DATASET_ID,
    title: `Document ${index.toString().padStart(2, "0")}`,
    status: "completed",
    display_status: "available",
    enabled: true,
    archived: false,
    metadata: {} as Record<string, unknown>,
  }));
  const registry = {
    version: 1,
    revision: 1,
    fields: [{ name: "author", label: "Author", type: "string" }],
  };

  await installClientAuth(page, {
    permissions: ["console:dashboard:view", "knowledge:dataset:view"],
    effective_permissions: ["console:dashboard:view", "knowledge:dataset:view"],
  });
  await seedClientPrefs(page, { locale: "en-US" });
  await page.route("**/api/v1/knowledge/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    if (request.method() === "GET" && path === `/api/v1/knowledge/datasets/${DATASET_ID}`) {
      await route.fulfill(jsonResponse({
        dataset_id: DATASET_ID,
        name: "Metadata pagination",
        visibility: "tenant",
        embedding_provider: "local",
        embedding_model: "hash-384",
        my_permission: "owner",
      }));
      return;
    }
    if (request.method() === "GET" && path === `/api/v1/knowledge/${DATASET_ID}/documents`) {
      const limit = Number(url.searchParams.get("limit") ?? 50);
      const offset = Number(url.searchParams.get("offset") ?? 0);
      await route.fulfill(
        jsonResponse(documents.slice(offset, offset + limit), 200, {
          "X-Total-Count": String(documents.length),
        })
      );
      return;
    }
    if (request.method() === "GET" && path === `/api/v1/knowledge/${DATASET_ID}/metadata-schema`) {
      await route.fulfill(jsonResponse(registry));
      return;
    }
    const documentMatch = path.match(
      /^\/api\/v1\/knowledge\/[^/]+\/documents\/(doc-\d+)$/
    );
    if (request.method() === "PATCH" && documentMatch) {
      const document = documents.find((item) => item.document_id === documentMatch[1]);
      const body = request.postDataJSON() as {
        metadata_patch?: Record<string, unknown>;
        metadata_remove?: string[];
      };
      if (document) {
        for (const name of body.metadata_remove ?? []) delete document.metadata[name];
        Object.assign(document.metadata, body.metadata_patch ?? {});
      }
      await route.fulfill(jsonResponse(document ?? {}, document ? 200 : 404));
      return;
    }
    if (request.method() === "GET" && path === `/api/v1/knowledge/${DATASET_ID}/sources`) {
      await route.fulfill(jsonResponse({
        file_uploads: { count: 51 },
        url_imports: { count: 0 },
        confluence_bindings: [],
        total_documents: 51,
      }));
      return;
    }
    await route.fulfill(jsonResponse([]));
  });
  await page.route("**/api/v1/eval/**", async (route) => {
    await route.fulfill(jsonResponse({ datasets: [], total: 0, limit: 200, offset: 0 }));
  });

  await page.goto(`/knowledge/${DATASET_ID}?tab=documents`);
  await expect(page.getByTestId("document-pagination")).toContainText("1–50 of 51");
  await page.getByRole("button", { name: "Next page" }).click();
  await expect(page.getByRole("button", { name: "Document 50" })).toBeVisible();
  await page.getByRole("button", { name: "Previous page" }).click();

  await page.getByTestId("doc-metadata-doc-00").click();
  await page.getByTestId("metadata-value-author").fill("Ada");
  await page.getByTestId("metadata-save").click();
  await expect(page.getByText("Metadata saved", { exact: true })).toBeVisible();

  await page.reload();
  await page.getByTestId("doc-metadata-doc-00").click();
  await expect(page.getByTestId("metadata-value-author")).toHaveValue("Ada");
});
