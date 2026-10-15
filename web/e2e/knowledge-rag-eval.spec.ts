import { expect, test, type Page, type Request } from "@playwright/test";
import {
  assertNoBlockingA11yIssues,
  buildAuthHeaders,
  ensureAuthenticatedPage,
  getApiUrl,
  installClientAuth,
  readE2ETestUser,
  seedClientPrefs,
} from "./support/helpers";

/**
 * Real-user regression for the KB RAG configuration + evaluation workbench.
 *
 * Drives the UI like a human — clicking tabs, choosing retrieval presets,
 * typing queries, labelling the correct chunks, and running an A/B retrieval
 * evaluation — against the running stack (frontend :8081, gateway :8080).
 *
 * Covers the new enterprise surfaces:
 *   1. Retrieval-test tab: opt-in preset hydration + hit test.
 *   2. Retrieval Evaluation Workbench: labelled test set -> A/B presets ->
 *      deterministic IR metrics (hit-rate / recall / MRR / nDCG) + pass gate.
 *
 * Presets are request configurations. These tests do not treat a preset name as
 * proof that rerank, hierarchical, or multimodal stages actually executed.
 */

const DATASET_PREFIX = "e2e-rag-eval";

function uniqueName(): string {
  return `${DATASET_PREFIX}-${Date.now()}`;
}

const DOC_CONTENT = [
  "报销流程需要提交发票、费用申请单和审批记录。",
  "年假申请应提前五天在人事系统提交，并由直属经理审批。",
  "新员工入职第一天需要领取工牌、笔记本电脑和门禁卡。",
  "会议室预订通过办公平台完成，单次最长可预订两小时。",
].join("\n\n");

const MOCK_DATASET_ID = "mock-rag-eval";

function jsonResponse(body: unknown, status = 200) {
  return {
    status,
    contentType: "application/json",
    body: JSON.stringify(body),
  };
}

function expectFastDenseRequestShape(requestBody: Record<string, unknown>) {
  expect(requestBody.mode).toBe("dense");
  expect(requestBody.fusion).toBeUndefined();
  expect(requestBody.fusion_method).toBeUndefined();
  expect(requestBody.rrf_k).toBeUndefined();
  expect(requestBody.rrf_weights).toBeUndefined();
  expect(requestBody.alpha).toBeUndefined();
  expect(requestBody.keyword_top_k).toBeUndefined();
  expect(requestBody.keyword_candidate_k).toBeUndefined();
  expect(requestBody.vector).toBeUndefined();
  expect(requestBody.keyword).toBeUndefined();
  expect(requestBody.multimodal).toBeUndefined();
  expect(requestBody.rerank).toBe(false);
  expect(requestBody.mmr).toBe(false);
}

function mockPreset(
  name: "fast" | "balanced",
  options: { mode: "vector" | "hybrid"; rerank: boolean }
) {
  return {
    name,
    label: name === "fast" ? "快速 (Fast)" : "均衡 (Balanced)",
    summary: name === "fast" ? "仅向量检索请求" : "混合检索与重排请求",
    recommended_for: "mock verification",
    config: {
      mode: options.mode,
      top_k: 5,
      score_threshold: name === "fast" ? 0.2 : 0.3,
      vector: { enabled: true, top_k: 20, score_threshold: null },
      keyword: {
        enabled: options.mode === "hybrid",
        top_k: 20,
        candidate_pool_size: 200,
        bm25_k1: 1.2,
        bm25_b: 0.75,
      },
      fusion: {
        strategy: name === "fast" ? "weighted" : "rrf",
        rrf_k: 60,
        rrf_weights: { vector: 1, keyword: 1 },
        alpha: 0.6,
      },
      rerank: {
        enabled: options.rerank,
        provider: "dashscope",
        model: "gte-rerank-v2",
        top_n: null,
        score_threshold: null,
      },
      mmr: { enabled: false, lambda: 0.5, similarity_threshold: null },
      multimodal: {
        enabled: true,
        image_search_enabled: true,
        image_score_threshold: 0.2,
        text_score_threshold: 0.3,
        use_separate_thresholds: true,
        image_boost: 1,
        vlm_rerank_enabled: false,
        vlm_rerank_weight: 0.4,
        content_type_filter: null,
      },
    },
  };
}

function watchClientErrors(page: Page, allowedConsoleErrors: RegExp[] = []) {
  const pageErrors: string[] = [];
  const consoleErrors: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(String(error)));
  page.on("console", (message) => {
    if (message.type() === "error") consoleErrors.push(message.text());
  });
  return () => {
    expect(pageErrors, `Page errors:\n${pageErrors.join("\n")}`).toEqual([]);
    const unexpectedConsoleErrors = consoleErrors.filter(
      (message) => !allowedConsoleErrors.some((pattern) => pattern.test(message))
    );
    expect(
      unexpectedConsoleErrors,
      `Unexpected console errors:\n${unexpectedConsoleErrors.join("\n")}`
    ).toEqual([]);
  };
}

async function installMockKnowledgeHarness(
  page: Page,
  options: { presetsFail?: boolean; evaluationDelayMs?: number } = {}
) {
  const evalBodies: Array<Record<string, unknown>> = [];
  await installClientAuth(page, {
    permissions: ["console:dashboard:view", "knowledge:dataset:view"],
    effective_permissions: ["console:dashboard:view", "knowledge:dataset:view"],
  });
  await seedClientPrefs(page, { locale: "zh-CN" });

  await page.route("**/api/v1/knowledge/**", async (route) => {
    const request = route.request();
    const pathname = new URL(request.url()).pathname;

    if (pathname === "/api/v1/knowledge/retrieval/presets") {
      if (options.presetsFail) {
        await route.fulfill(jsonResponse({ detail: "preset service unavailable" }, 503));
        return;
      }
      await route.fulfill(
        jsonResponse({
          presets: [
            mockPreset("balanced", { mode: "hybrid", rerank: true }),
            mockPreset("fast", { mode: "vector", rerank: false }),
          ],
          recommended_default: "balanced",
          notes: {},
        })
      );
      return;
    }

    if (pathname === `/api/v1/knowledge/datasets/${MOCK_DATASET_ID}`) {
      await route.fulfill(
        jsonResponse({
          dataset_id: MOCK_DATASET_ID,
          name: "Mock RAG Evaluation",
          description: "Browser-only evaluation harness",
          visibility: "tenant",
          embedding_provider: "local",
          embedding_model: "hash-384",
          embedding_dimension: 384,
          statistics: { document_count: 1, segment_count: 1, token_count: 20 },
        })
      );
      return;
    }

    if (pathname === `/api/v1/knowledge/${MOCK_DATASET_ID}/documents`) {
      await route.fulfill(jsonResponse([]));
      return;
    }

    if (pathname === `/api/v1/knowledge/${MOCK_DATASET_ID}/segments`) {
      await route.fulfill(jsonResponse([]));
      return;
    }

    if (pathname === `/api/v1/knowledge/${MOCK_DATASET_ID}/retrieve`) {
      await route.fulfill(
        jsonResponse({
          results: [
            {
              segment_id: "segment-annual-leave",
              document_id: "document-handbook",
              score: 0.92,
              text: "年假申请应提前五天提交。",
              metadata: {},
            },
          ],
          metadata: { mode: "hybrid" },
        })
      );
      return;
    }

    if (pathname === `/api/v1/knowledge/${MOCK_DATASET_ID}/retrieve_evaluate`) {
      const requestBody = request.postDataJSON() as Record<string, unknown>;
      const rerankRequested =
        (requestBody.rerank as { enabled?: boolean } | undefined)?.enabled ?? false;
      const requestCases = Array.isArray(requestBody.cases)
        ? (requestBody.cases as Array<{ case_id?: string }>)
        : [];
      const numCases = requestCases.length;
      evalBodies.push(requestBody);
      if (options.evaluationDelayMs) {
        await new Promise((resolve) => setTimeout(resolve, options.evaluationDelayMs));
      }
      const metrics = Object.fromEntries(
        [1, 3, 5, 10].map((k) => [
          String(k),
          {
            k,
            num_queries: numCases,
            hit_rate: 1,
            precision_at_k: 1 / k,
            recall_at_k: 1,
            mrr: 1,
            ndcg_at_k: 1,
            map: 1,
          },
        ])
      );
      await route.fulfill(
        jsonResponse({
          dataset_id: MOCK_DATASET_ID,
          num_cases: numCases,
          k_values: [1, 3, 5, 10],
          metrics,
          primary_metrics: metrics["10"],
          cases: [],
          per_query: {},
          requested_config: {
            mode: requestBody.mode,
            rerank: rerankRequested,
            mmr: (requestBody.mmr as { enabled?: boolean } | undefined)?.enabled ?? false,
          },
          case_metadata: requestCases.map((testCase, index) => ({
            case_id: testCase.case_id ?? `mock-case-${index}`,
            provider_retrieved_count: 1,
            retrieved_count: 1,
            unique_retrieved_count: 1,
            duplicate_segment_ids: [],
            retrieval_metadata: {
              pipeline: "standard",
              ...(rerankRequested
                ? index === 0
                  ? {
                      rerank_applied_provider: "dashscope",
                    }
                  : {
                      rerank_fallback: true,
                      rerank_error: "primary reranker failed before fallback",
                    }
                : {}),
            },
          })),
        })
      );
      return;
    }

    await route.fulfill(jsonResponse([]));
  });

  // The workbench persists cases into a linked eval dataset (C7) and probes
  // it on mount. Keep this suite hermetic: no linked dataset, so the
  // workbench starts empty and nothing leaks to the live gateway (an
  // unmocked 401 there would wipe the seeded auth session mid-test).
  await page.route("**/api/v1/eval/**", async (route) => {
    await route.fulfill(jsonResponse({ datasets: [], total: 0, limit: 200, offset: 0 }));
  });

  return evalBodies;
}

test.describe("@mock KB RAG evaluation UI contract", () => {
  test("projects Fast into a provider-free dense request without hybrid-only fields", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page);
    await installMockKnowledgeHarness(page);
    await page.goto(`/knowledge/${MOCK_DATASET_ID}?tab=retrieval`);

    const presetTrigger = page.getByTestId("retrieval-preset");
    await expect(presetTrigger).toBeEnabled();
    await presetTrigger.click();
    await page.getByRole("option", { name: /快速|Fast/ }).click();
    await page.getByPlaceholder("请输入文本").first().fill("报销流程需要什么材料？");
    const requestPromise = page.waitForRequest((request) =>
      request.url().includes(`/api/v1/knowledge/${MOCK_DATASET_ID}/hit_test`)
    );
    await page.getByRole("button", { name: "测试" }).first().click();

    expectFastDenseRequestShape((await requestPromise).postDataJSON() as Record<string, unknown>);
    assertNoClientErrors();
  });

  test("submits canonical presets, preserves result labels, and has no blocking a11y issues", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const evalBodies = await installMockKnowledgeHarness(page);
    await page.goto(`/knowledge/${MOCK_DATASET_ID}?tab=eval`);

    await expect(page.getByTestId("eval-preset-a")).toBeEnabled();
    await page.getByPlaceholder(/输入一个测试问题/).fill("年假需要提前几天申请？");
    await page.getByRole("button", { name: "添加" }).click();
    await page
      .getByLabel("正确分段 ID（逗号分隔）")
      .first()
      .fill("segment-outside-baseline-top-ten");
    await page.getByRole("button", { name: "标注正确分段" }).click();
    await page.getByRole("checkbox").check();
    await page.getByPlaceholder(/输入一个测试问题/).fill("报销需要提交什么材料？");
    await page.getByRole("button", { name: "添加" }).click();
    await page.getByRole("button", { name: "标注正确分段" }).nth(1).click();
    await page.getByRole("checkbox").nth(1).check();
    await expect(page.getByTestId("run-retrieval-eval")).toBeEnabled();
    await page.getByTestId("run-retrieval-eval").click();

    await expect(page.getByRole("columnheader", { name: "B · fast" })).toBeVisible();
    await expect(page.getByTestId("eval-execution-evidence")).toContainText("pipeline=standard");
    await expect(page.getByTestId("eval-execution-evidence")).toContainText(
      "applied cases=1/2 providers=dashscope"
    );
    await expect(page.getByTestId("eval-execution-evidence")).toContainText(
      "fallback/failure cases=1/2"
    );
    expect(evalBodies).toHaveLength(2);
    for (const body of evalBodies) {
      expect(typeof body.vector).toBe("object");
      expect(typeof body.keyword).toBe("object");
      expect(typeof body.fusion).toBe("object");
      expect(typeof body.multimodal).toBe("object");
      expect(typeof body.rerank).toBe("object");
      const requestCases = body.cases as Array<{ relevant_segment_ids?: string[] }>;
      expect(requestCases[0]?.relevant_segment_ids).toContain(
        "segment-outside-baseline-top-ten"
      );
    }
    await page.getByTestId("eval-preset-a").click();
    await page.getByRole("option", { name: /快速|Fast/ }).click();
    await expect(page.getByText(/测试集或请求预设已修改/)).toBeVisible();
    await expect(
      page.getByRole("columnheader", { name: "A · balanced" })
    ).toBeVisible();
    await assertNoBlockingA11yIssues(page, ["[data-testid='retrieval-eval-workbench']"]);
    assertNoClientErrors();
  });

  test("fails closed on preset errors at mobile width", async ({ page }) => {
    const assertNoClientErrors = watchClientErrors(page, [/503|Service Unavailable/]);
    await page.setViewportSize({ width: 375, height: 812 });
    await installMockKnowledgeHarness(page, { presetsFail: true });
    await page.goto(`/knowledge/${MOCK_DATASET_ID}?tab=eval`);

    await expect(page.getByRole("alert")).toContainText("检索预设加载失败");
    await expect(page.getByTestId("run-retrieval-eval")).toBeDisabled();
    const box = await page.getByTestId("run-retrieval-eval").boundingBox();
    expect(box).not.toBeNull();
    expect(box!.x + box!.width).toBeLessThanOrEqual(375);
    assertNoClientErrors();
  });

  test("cancels an in-flight A/B evaluation without publishing results", async ({ page }) => {
    const assertNoClientErrors = watchClientErrors(page);
    await installMockKnowledgeHarness(page, { evaluationDelayMs: 1_000 });
    await page.goto(`/knowledge/${MOCK_DATASET_ID}?tab=eval`);

    await page.getByPlaceholder(/输入一个测试问题/).fill("年假需要提前几天申请？");
    await page.getByRole("button", { name: "添加" }).click();
    await page.getByRole("button", { name: "标注正确分段" }).click();
    await page.getByRole("checkbox").check();
    await page.getByTestId("run-retrieval-eval").click();
    await page.getByRole("button", { name: "取消评测" }).click();

    await expect(page.getByRole("alert")).toContainText("评测已取消");
    await expect(page.getByText("指标对比")).toHaveCount(0);
    assertNoClientErrors();
  });
});

test.describe("KB RAG config & evaluation workbench", () => {
  test.setTimeout(5 * 60_000);

  // These assertions are written against the Chinese console. Pin the locale
  // instead of inheriting whatever the shared storage state happens to carry:
  // when `ensureAuthenticatedPage` has to log in through the form, the app
  // starts from the default locale and every Chinese locator misses.
  test.beforeEach(async ({ page }) => {
    await seedClientPrefs(page, { locale: "zh-CN" });
  });

  let datasetId: string | undefined;
  let headers: Record<string, string>;
  let testPassword: string;

  test.beforeAll(async ({ request }) => {
    testPassword = (await readE2ETestUser()).password;
    headers = await buildAuthHeaders(request);
    const apiUrl = getApiUrl();

    // Create a dataset with a local embedding model (no external API needed).
    const createRes = await request.post(`${apiUrl}/api/v1/knowledge/datasets`, {
      headers,
      data: {
        name: uniqueName(),
        description: "E2E RAG eval workbench dataset",
        embedding_provider: "local",
        embedding_model: "hash-384",
        embedding_dimension: 384,
      },
    });
    expect(createRes.ok(), `Create dataset failed: ${createRes.status()}`).toBeTruthy();
    const created = await createRes.json();
    datasetId = created.id ?? created.dataset_id ?? created.data?.id;
    expect(datasetId).toBeTruthy();

    // Upload a document so retrieval/evaluation have real segments to hit.
    const docRes = await request.post(`${apiUrl}/api/v1/knowledge/${datasetId}/documents/text`, {
      headers,
      data: { title: "员工手册", content: DOC_CONTENT },
    });
    expect(docRes.ok(), `Create document failed: ${docRes.status()}`).toBeTruthy();

    // Wait for the async pipeline to produce at least one segment.
    const deadline = Date.now() + 120_000;
    let segments = 0;
    while (Date.now() < deadline) {
      const segRes = await request.get(`${apiUrl}/api/v1/knowledge/${datasetId}/segments`, { headers });
      if (segRes.ok()) {
        const body = await segRes.json();
        const list = Array.isArray(body) ? body : body.data ?? body.segments ?? [];
        segments = list.length;
        if (segments > 0) break;
      }
      await new Promise((r) => setTimeout(r, 2000));
    }
    expect(segments, "Document was not segmented in time").toBeGreaterThan(0);

    // Segment creation precedes vector-index visibility. Wait until the real
    // retrieval endpoint can read at least one result before starting UI tests.
    const retrievalDeadline = Date.now() + 120_000;
    let retrievable = false;
    while (Date.now() < retrievalDeadline) {
      const retrieveRes = await request.post(
        `${apiUrl}/api/v1/knowledge/${datasetId}/retrieve`,
        {
          headers,
          data: {
            query: "报销流程需要什么材料？",
            top_k: 5,
            mode: "vector",
            rerank: false,
            mmr: false,
          },
        }
      );
      if (retrieveRes.ok()) {
        const body = await retrieveRes.json();
        if (Array.isArray(body.results) && body.results.length > 0) {
          retrievable = true;
          break;
        }
      }
      await new Promise((resolve) => setTimeout(resolve, 2000));
    }
    expect(retrievable, "Document was segmented but never became retrievable").toBeTruthy();
  });

  test.afterAll(async ({ request }) => {
    if (datasetId) {
      const apiUrl = getApiUrl();
      const cleanupResponse = await request.delete(
        `${apiUrl}/api/v1/knowledge/datasets/${datasetId}`,
        {
          headers,
          data: { password: testPassword, reason: "E2E cleanup" },
        }
      );
      expect(
        cleanupResponse.ok(),
        `Delete dataset failed during cleanup: ${cleanupResponse.status()}`
      ).toBeTruthy();
    }
  });

  test("retrieval tab: preset dropdown hydrates config, hit test returns scored chunks", async ({
    page,
  }) => {
    await ensureAuthenticatedPage(page, `/knowledge/${datasetId}?tab=retrieval`);

    // The preset dropdown is present (loaded from /retrieval/presets).
    const presetTrigger = page.getByTestId("retrieval-preset");
    await expect(presetTrigger).toBeEnabled({ timeout: 20_000 });

    // Use the provider-free Fast preset so this live check does not depend on
    // an external reranker credential.
    await presetTrigger.click();
    await page.getByRole("option", { name: /快速|Fast/ }).click();

    // Type a real query and run the hit test.
    await page.getByPlaceholder("请输入文本").first().fill("报销流程需要什么材料？");
    const requestPromise = page.waitForRequest((request) =>
      request.url().includes(`/api/v1/knowledge/${datasetId}/hit_test`)
    );
    await page.getByRole("button", { name: "测试" }).first().click();

    const requestBody = (await requestPromise).postDataJSON() as Record<string, unknown>;
    expectFastDenseRequestShape(requestBody);

    // Scored chunks should render in the results panel.
    await expect(page.getByText(/相似度|得分|score/i).first()).toBeVisible({ timeout: 30_000 });
  });

  test("eval workbench: local-live retrieval with a provider-free preset fixture", async ({
    page,
  }) => {
    // This test keeps dataset/index/evaluation calls live, but projects the
    // server presets into two provider-free configs so external reranker
    // availability cannot turn a frontend regression into an environment flake.
    await page.route("**/api/v1/knowledge/retrieval/presets", async (route) => {
      const upstream = await route.fetch();
      expect(upstream.ok(), "Live preset endpoint should be available").toBeTruthy();
      const body = await upstream.json();
      const fast = body.presets.find((preset: { name: string }) => preset.name === "fast");
      const balanced = body.presets.find(
        (preset: { name: string }) => preset.name === "balanced"
      );
      expect(fast).toBeTruthy();
      expect(balanced).toBeTruthy();
      const localFast = {
        ...fast,
        config: {
          ...fast.config,
          rerank: { ...fast.config.rerank, enabled: false },
          mmr: { ...fast.config.mmr, enabled: false },
          multimodal: {
            ...fast.config.multimodal,
            enabled: false,
            image_search_enabled: false,
            vlm_rerank_enabled: false,
          },
        },
      };
      const localHybrid = {
        ...balanced,
        name: "local-hybrid",
        label: "本地混合 (Local Hybrid)",
        summary: "混合检索，不请求外部重排。",
        config: {
          ...balanced.config,
          rerank: { ...balanced.config.rerank, enabled: false },
          mmr: { ...balanced.config.mmr, enabled: false },
          multimodal: {
            ...balanced.config.multimodal,
            enabled: false,
            image_search_enabled: false,
            vlm_rerank_enabled: false,
          },
        },
      };
      await route.fulfill(
        jsonResponse({
          ...body,
          presets: [localFast, localHybrid],
          recommended_default: "fast",
        })
      );
    });
    await ensureAuthenticatedPage(page, `/knowledge/${datasetId}?tab=eval`);

    // Workbench header and explicit evidence boundary rendered.
    await expect(page.getByText("检索评测工作台").first()).toBeVisible({ timeout: 20_000 });
    await expect(page.getByTestId("eval-scope-note")).toContainText("case_metadata");
    await expect(page.getByTestId("run-retrieval-eval")).toBeDisabled();

    // --- Build a labelled test case like a real evaluator ---
    await page
      .getByPlaceholder(/输入一个测试问题/)
      .first()
      .fill("年假申请需要提前几天提交？");
    await page.getByRole("button", { name: "添加" }).click();

    // Open the annotation helper and mark the first retrieved candidate as correct.
    await page.getByRole("button", { name: "标注正确分段" }).first().click();
    // Candidates load asynchronously; wait for at least one checkbox row.
    const relevantCheckbox = page.getByRole("checkbox", { name: /标记为正确分段/ }).first();
    await relevantCheckbox.waitFor({ state: "visible", timeout: 30_000 });
    await relevantCheckbox.check();

    // --- Pick presets A (baseline) and B (candidate) ---
    // The fixture selects two distinct provider-free presets by default.
    const presetB = page.getByTestId("eval-preset-b");
    await expect(presetB).toBeEnabled();

    // --- Run the A/B evaluation ---
    const requestBodies: Array<Record<string, unknown>> = [];
    const captureEvalRequests = (request: Request) => {
      if (request.url().includes("/retrieve_evaluate")) {
        requestBodies.push(request.postDataJSON() as Record<string, unknown>);
      }
    };
    page.on("request", captureEvalRequests);
    await page.getByRole("button", { name: /运行 A\/B 评测/ }).click();

    // --- Verify the comparison table + metrics + gate badges ---
    await expect(page.getByText("指标对比").first()).toBeVisible({ timeout: 60_000 });
    page.off("request", captureEvalRequests);
    expect(requestBodies).toHaveLength(2);
    for (const body of requestBodies) {
      expect(typeof body.vector).toBe("object");
      expect(typeof body.keyword).toBe("object");
      expect(typeof body.fusion).toBe("object");
      expect(typeof body.multimodal).toBe("object");
      expect((body.rerank as { enabled?: boolean }).enabled).toBe(false);
      expect(typeof body.mmr).toBe("object");
      expect((body.multimodal as { enabled?: boolean }).enabled).toBe(false);
      expect(
        (body.multimodal as { image_search_enabled?: boolean }).image_search_enabled
      ).toBe(false);
      expect(
        (body.multimodal as { vlm_rerank_enabled?: boolean }).vlm_rerank_enabled
      ).toBe(false);
    }
    await expect(page.getByText("命中率 Hit Rate").first()).toBeVisible();
    await expect(page.getByText("nDCG@K").first()).toBeVisible();
    const gateSummary = page.getByText("指标对比").first().locator("..");
    await expect(gateSummary.getByText(/通过|未通过/)).toHaveCount(2);
    await expect(page.getByTestId("eval-execution-evidence")).toContainText(
      "pipeline=standard"
    );
    await expect(page.getByTestId("eval-execution-evidence")).toContainText(
      "fallback/failure cases=0/1"
    );

    // Changing the current selection must not relabel the completed run.
    await expect(
      page.getByRole("columnheader", { name: "B · local-hybrid" })
    ).toBeVisible();
    await presetB.click();
    await page.getByRole("option", { name: /快速|Fast/ }).click();
    await expect(
      page.getByRole("columnheader", { name: "B · local-hybrid" })
    ).toBeVisible();
    await expect(page.getByText(/测试集或请求预设已修改/)).toBeVisible();
  });

  test("eval workbench fails closed when presets cannot load", async ({ page }) => {
    await page.route("**/api/v1/knowledge/retrieval/presets", async (route) => {
      await route.fulfill({
        status: 503,
        contentType: "application/json",
        body: JSON.stringify({ detail: "preset service unavailable" }),
      });
    });

    await ensureAuthenticatedPage(page, `/knowledge/${datasetId}?tab=eval`);

    await expect(page.getByRole("alert")).toContainText("检索预设加载失败", {
      timeout: 20_000,
    });
    await expect(page.getByTestId("run-retrieval-eval")).toBeDisabled();
    await expect(page.getByRole("button", { name: "重试" })).toBeVisible();
  });

  test("eval workbench controls remain usable on a mobile viewport", async ({ page }) => {
    await page.setViewportSize({ width: 375, height: 812 });
    await ensureAuthenticatedPage(page, `/knowledge/${datasetId}?tab=eval`);

    await expect(page.getByText("检索评测工作台").first()).toBeVisible({ timeout: 20_000 });
    await expect(page.getByPlaceholder(/输入一个测试问题/)).toBeVisible();
    await expect(page.getByTestId("eval-preset-a")).toBeEnabled();
    await expect(page.getByTestId("run-retrieval-eval")).toBeVisible();

    for (const testId of ["eval-preset-a", "eval-preset-b", "run-retrieval-eval"]) {
      const box = await page.getByTestId(testId).boundingBox();
      expect(box, `${testId} should have a layout box`).not.toBeNull();
      expect(box!.x).toBeGreaterThanOrEqual(0);
      expect(box!.x + box!.width).toBeLessThanOrEqual(375);
    }
  });
});
