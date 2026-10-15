import { expect, test } from "@playwright/test";

import { installClientAuth, seedClientPrefs } from "./support/helpers";

const DATASET_ID = "mock-query-feedback";
const TRACE_ID = "d04d53c8-acde-49d0-b3eb-49890dbd5673";
const QUERY_FINGERPRINT = "a".repeat(64);

function jsonResponse(body: unknown, status = 200, headers: Record<string, string> = {}) {
  return { status, contentType: "application/json", headers, body: JSON.stringify(body) };
}

test("@mock filters zero-result queries and persists retrieval feedback", async ({ page }) => {
  const feedbackWrites: Array<Record<string, unknown>> = [];
  await installClientAuth(page, {
    permissions: ["console:dashboard:view", "knowledge:dataset:view"],
    effective_permissions: ["console:dashboard:view", "knowledge:dataset:view"],
  });
  await seedClientPrefs(page, { locale: "en-US" });

  await page.route("**/api/v1/knowledge/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;

    if (path === "/api/v1/knowledge/retrieval/presets") {
      await route.fulfill(jsonResponse({ presets: [], recommended_default: null, notes: {} }));
      return;
    }
    if (path === `/api/v1/knowledge/datasets/${DATASET_ID}`) {
      await route.fulfill(jsonResponse({
        dataset_id: DATASET_ID,
        name: "Query feedback",
        visibility: "tenant",
        embedding_provider: "local",
        embedding_model: "hash-384",
        my_permission: "owner",
      }));
      return;
    }
    if (path === `/api/v1/knowledge/${DATASET_ID}/documents`) {
      await route.fulfill(jsonResponse([], 200, { "X-Total-Count": "0" }));
      return;
    }
    if (request.method() === "GET" && path === `/api/v1/knowledge/${DATASET_ID}/queries`) {
      const zeroOnly = url.searchParams.get("zero_results") === "true";
      const rows = [
        {
          id: "query-zero",
          dataset_id: DATASET_ID,
          content: "missing policy",
          source: "hit_test",
          metadata: {},
          trace_id: TRACE_ID,
          query_fingerprint: QUERY_FINGERPRINT,
          mode: "hybrid",
          top_k: 5,
          hit_count: 0,
          stage_timings: { total_ms: 12.5 },
          created_at: "2026-08-29T12:00:00Z",
        },
        {
          id: "query-hit",
          dataset_id: DATASET_ID,
          content: "known policy",
          source: "api",
          metadata: {},
          trace_id: "ec538b3a-952a-4785-8612-7fe5282785d6",
          query_fingerprint: "b".repeat(64),
          mode: "dense",
          top_k: 3,
          hit_count: 1,
          stage_timings: { total_ms: 8 },
          created_at: "2026-08-29T11:00:00Z",
        },
      ];
      await route.fulfill(jsonResponse({
        queries: zeroOnly ? rows.slice(0, 1) : rows,
        next_cursor: null,
        has_more: false,
      }));
      return;
    }
    if (request.method() === "GET" && path === `/api/v1/knowledge/${DATASET_ID}/feedback`) {
      await route.fulfill(jsonResponse({
        feedback: [{
          feedback_id: "756174a6-87b2-4292-9189-f501a79d9452",
          tenant_id: "tenant-mock",
          dataset_id: DATASET_ID,
          trace_id: TRACE_ID,
          query_fingerprint: QUERY_FINGERPRINT,
          target_type: "retrieval_hit",
          target_id: "segment-a",
          rating: "negative",
          reason_code: "irrelevant",
          comment: "Wrong handbook",
          created_by: "user-mock",
          query_content: "missing policy",
          created_at: "2026-08-29T12:01:00Z",
          updated_at: "2026-08-29T12:01:00Z",
        }],
        next_cursor: null,
        has_more: false,
      }));
      return;
    }
    if (request.method() === "PUT" && path === `/api/v1/knowledge/${DATASET_ID}/feedback`) {
      const body = request.postDataJSON() as Record<string, unknown>;
      feedbackWrites.push(body);
      await route.fulfill(jsonResponse({
        feedback_id: "756174a6-87b2-4292-9189-f501a79d9452",
        tenant_id: "tenant-mock",
        dataset_id: DATASET_ID,
        target_id: body.segment_id,
        created_by: "user-mock",
        created_at: "2026-08-29T12:01:00Z",
        updated_at: "2026-08-29T12:01:00Z",
        ...body,
      }));
      return;
    }
    if (request.method() === "POST" && path === `/api/v1/knowledge/${DATASET_ID}/hit_test`) {
      await route.fulfill(jsonResponse({
        trace_id: TRACE_ID,
        query_fingerprint: QUERY_FINGERPRINT,
        results: [{
          segment_id: "segment-a",
          document_id: "document-a",
          text: "Policy answer",
          score: 0.91,
          metadata: {},
        }],
        metadata: { mode: "hybrid", trace_id: TRACE_ID, query_fingerprint: QUERY_FINGERPRINT },
      }));
      return;
    }
    if (request.method() === "POST" && path === `/api/v1/knowledge/${DATASET_ID}/qa/stream`) {
      await route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: `data: ${JSON.stringify({
          event: "done",
          data: {
            result: {
              query: "policy",
              answer: "The policy answer",
              context_segments: [{
                segment_id: "segment-a",
                document_id: "document-a",
                text: "Policy answer",
                score: 0.91,
                metadata: {},
              }],
              retrieval_metadata: {},
              timing: { retrieval_ms: 5, llm_ms: 10, total_ms: 15 },
              model: "mock-model",
              trace_id: TRACE_ID,
              query_fingerprint: QUERY_FINGERPRINT,
            },
          },
        })}\n\n`,
      });
      return;
    }
    await route.fulfill(jsonResponse([]));
  });

  await page.goto(`/knowledge/${DATASET_ID}?tab=queries`);
  await expect(page.getByTestId("query-log-list")).toContainText("missing policy");
  await expect(page.getByTestId("query-log-list")).toContainText("known policy");
  await expect(page.getByTestId("negative-feedback-list")).toContainText("Wrong handbook");
  await page.getByTestId("query-result-filter").click();
  await page.getByRole("option", { name: "Zero results" }).click();
  await expect(page.getByTestId("query-log-list")).toContainText("missing policy");
  await expect(page.getByTestId("query-log-list")).not.toContainText("known policy");

  await page.goto(`/knowledge/${DATASET_ID}?tab=retrieval`);
  await page.getByPlaceholder("Enter text", { exact: true }).fill("policy");
  await page.getByRole("button", { name: "Test", exact: true }).click();
  await page.getByTestId("feedback-negative-retrieval_hit").click();
  await page.getByTestId("feedback-submit-retrieval_hit").click();
  await expect.poll(() => feedbackWrites.length).toBe(1);
  expect(feedbackWrites[0]).toMatchObject({
    trace_id: TRACE_ID,
    query_fingerprint: QUERY_FINGERPRINT,
    target_type: "retrieval_hit",
    segment_id: "segment-a",
    rating: "negative",
    reason_code: "irrelevant",
  });

  await page.goto(`/knowledge/${DATASET_ID}?tab=qa`);
  await page.getByPlaceholder("Enter your question, Shift+Enter for new line...").fill("policy");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await page.getByTestId("feedback-positive-qa_answer").click();
  await page.getByTestId("feedback-submit-qa_answer").click();
  await expect.poll(() => feedbackWrites.length).toBe(2);
  expect(feedbackWrites[1]).toMatchObject({
    trace_id: TRACE_ID,
    query_fingerprint: QUERY_FINGERPRINT,
    target_type: "qa_answer",
    rating: "positive",
    reason_code: "helpful",
  });
});
