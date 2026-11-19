import { expect, test, type Page } from "@playwright/test";
import { installClientAuth, seedClientPrefs } from "./support/helpers";

/**
 * Mock contract for eval-case persistence in the retrieval workbench (C7,
 * PRD §5-#22/#23, F6):
 *   1. saved cases load from the linked eval dataset on tab open (found by
 *      metadata.kb_dataset_id); the eval dataset is created lazily on first
 *      save, never on read;
 *   2. "save cases" splits new vs edited: new case_ids go through
 *      examples:import (mode=skip_duplicates, which dedupes on
 *      metadata.case_id), edited cases go through PATCH — and PATCH keeps
 *      metadata.case_id so later imports still recognise the example;
 *   3. explicitly removed persisted cases go through DELETE and stay absent
 *      after a full page reload;
 *   4. JSONL import reuses the eval page's parse/validate/batch pipeline and
 *      lands in the same linked eval dataset;
 *   5. hit-test failures enter the same Eval store as pending review cases,
 *      with a user supplied expectation and only verified source versions.
 *
 * The QA tab shares the pending-case save dialog, but uses its own source
 * identity. Its SSE stream is not mocked in this suite.
 *
 * Every route is fulfilled in-process with mutable stores, so the suite runs
 * against `pnpm dev` alone and never needs the live stack.
 */

const DATASET_ID = "mock-eval-cases";
const EVAL_DATASET_ID = "eval-mock-eval-cases";

interface MockEvalExample {
  example_id: string;
  case_id?: string;
  dataset_id: string;
  tenant_id: string;
  split: string;
  input: Record<string, unknown>;
  expected_output: Record<string, unknown>;
  expected_trajectory: Record<string, unknown>;
  assertions: Array<Record<string, unknown>>;
  metadata: Record<string, unknown>;
  source_trace_id?: string | null;
  created_by: string;
  created_at?: string;
}

interface MockEvalDataset {
  dataset_id: string;
  tenant_id: string;
  name: string;
  description: string;
  version: string;
  schema: Record<string, unknown>;
  metadata: Record<string, unknown>;
  created_by: string;
}

function makeEvalDataset(): MockEvalDataset {
  return {
    dataset_id: EVAL_DATASET_ID,
    tenant_id: "tenant-mock",
    name: `kb-retrieval-eval-${DATASET_ID}`,
    description: `Retrieval evaluation cases linked to knowledge dataset ${DATASET_ID}`,
    version: "v1",
    schema: {},
    metadata: { kb_dataset_id: DATASET_ID, source: "kb-retrieval-workbench" },
    created_by: "user-mock",
  };
}

function makeSavedExample(): MockEvalExample {
  return {
    example_id: "ex-saved-1",
    case_id: "case_saved",
    dataset_id: EVAL_DATASET_ID,
    tenant_id: "tenant-mock",
    split: "regression",
    input: { query: "年假怎么申请？" },
    expected_output: { relevant_segment_ids: ["seg-annual-leave"] },
    expected_trajectory: {},
    assertions: [],
    metadata: {
      case_id: "case_saved",
      source: "kb-retrieval-workbench",
      kb_dataset_id: DATASET_ID,
    },
    created_by: "user-mock",
    created_at: "2026-08-20T09:00:00Z",
  };
}

// Full canonical shape: validateRetrievalPresetConfig rejects anything
// missing the nested vector/keyword/fusion/rerank/mmr/multimodal sections.
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

function jsonResponse(body: unknown, status = 200) {
  return {
    status,
    contentType: "application/json",
    body: JSON.stringify(body),
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

async function installEvalCasesHarness(
  page: Page,
  options: { linkedDataset?: boolean; zeroHits?: boolean; evalRunPermission?: boolean; incompleteSource?: boolean } = {}
) {
  const linked = options.linkedDataset !== false;
  const datasets: MockEvalDataset[] = linked ? [makeEvalDataset()] : [];
  const examples: MockEvalExample[] = linked ? [makeSavedExample()] : [];
  const captured = {
    createdDatasets: [] as Array<Record<string, unknown>>,
    imports: [] as Array<{ mode?: string; examples: Array<Record<string, unknown>> }>,
    failureSaves: [] as Array<Record<string, unknown>>,
    savedFailureExamples: [] as MockEvalExample[],
    patches: [] as Array<{ exampleId: string; body: Record<string, unknown> }>,
    deletes: [] as string[],
  };

  const permissions = ["console:dashboard:view", "knowledge:dataset:view", "console:eval:view"];
  if (options.evalRunPermission !== false) permissions.push("console:eval:run");
  await installClientAuth(page, {
    permissions,
    effective_permissions: permissions,
  });
  await seedClientPrefs(page, { locale: "zh-CN" });

  await page.route("**/api/v1/knowledge/**", async (route) => {
    const request = route.request();
    const pathname = new URL(request.url()).pathname;

    if (pathname === "/api/v1/knowledge/retrieval/presets") {
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

    if (pathname === `/api/v1/knowledge/datasets/${DATASET_ID}`) {
      await route.fulfill(
        jsonResponse({
          dataset_id: DATASET_ID,
          name: "Mock Eval Cases",
          description: "Browser-only eval-case persistence harness",
          visibility: "tenant",
          embedding_provider: "local",
          embedding_model: "hash-384",
          embedding_dimension: 384,
          statistics: { document_count: 1, segment_count: 2, token_count: 40 },
        })
      );
      return;
    }

    if (pathname === `/api/v1/knowledge/${DATASET_ID}/documents`) {
      await route.fulfill(jsonResponse([]));
      return;
    }

    if (pathname === `/api/v1/knowledge/${DATASET_ID}/segments`) {
      await route.fulfill(jsonResponse([]));
      return;
    }

    if (
      request.method() === "POST" &&
      pathname === `/api/v1/knowledge/${DATASET_ID}/hit_test`
    ) {
      await route.fulfill(
        jsonResponse({
          results: options.zeroHits ? [] : [
            {
              segment_id: "seg-hit-1",
              document_id: "doc-handbook",
              score: 0.91,
              text: "年假申请应提前五天提交。",
              metadata: {},
              source_version: 2,
              source_hash: "a".repeat(64),
            },
            {
              segment_id: "seg-hit-2",
              document_id: "doc-handbook",
              score: 0.8,
              text: "报销需要发票与审批记录。",
              metadata: {},
              ...(options.incompleteSource ? {} : {
                source_version: 2,
                source_hash: "b".repeat(64),
              }),
            },
          ],
          metadata: { mode: "hybrid" },
        })
      );
      return;
    }

    await route.fulfill(jsonResponse([]));
  });

  await page.route("**/api/v1/eval/**", async (route) => {
    const request = route.request();
    const method = request.method();
    const pathname = new URL(request.url()).pathname;

    if (pathname === "/api/v1/eval/datasets") {
      if (method === "GET") {
        await route.fulfill(
          jsonResponse({ datasets, total: datasets.length, limit: 200, offset: 0 })
        );
        return;
      }
      if (method === "POST") {
        const body = request.postDataJSON() as Record<string, unknown>;
        captured.createdDatasets.push(body);
        const created: MockEvalDataset = {
          dataset_id: EVAL_DATASET_ID,
          tenant_id: "tenant-mock",
          name: typeof body.name === "string" ? body.name : `kb-retrieval-eval-${DATASET_ID}`,
          description: typeof body.description === "string" ? body.description : "",
          version: "v1",
          schema: {},
          metadata: (body.metadata as Record<string, unknown>) ?? {},
          created_by: "user-mock",
        };
        datasets.push(created);
        await route.fulfill(jsonResponse(created, 201));
        return;
      }
    }

    const failureGet = pathname.match(
      /^\/api\/v1\/eval\/datasets\/[^/]+\/kb-failures\/([^/]+)$/
    );
    if (method === "GET" && failureGet) {
      const caseId = decodeURIComponent(failureGet[1]);
      const latest = captured.savedFailureExamples
        .filter((example) => example.metadata.case_id === caseId)
        .sort((a, b) => Number(b.metadata.case_revision) - Number(a.metadata.case_revision))[0];
      await route.fulfill(latest
        ? jsonResponse(latest)
        : jsonResponse({ detail: "KB failure case not found" }, 404));
      return;
    }

    if (method === "POST" && pathname === `/api/v1/eval/datasets/${EVAL_DATASET_ID}/kb-failures:save`) {
      const body = request.postDataJSON() as Record<string, unknown>;
      captured.failureSaves.push(body);
      const latest = captured.savedFailureExamples
        .filter((example) => example.metadata.case_id === body.case_id)
        .sort((a, b) => Number(b.metadata.case_revision) - Number(a.metadata.case_revision))[0];
      const revision = latest ? Number(latest.metadata.case_revision) : 0;
      const unchanged = latest
        && latest.expected_output.answer === body.expected_answer
        && latest.metadata.kb_failure_reason === body.failure_reason
        && latest.source_trace_id === body.source_trace_id;
      if (unchanged) {
        await route.fulfill(jsonResponse({ example: latest, created: false, revision }));
        return;
      }
      if (body.expected_revision !== revision) {
        await route.fulfill(jsonResponse({ detail: { error: "case_revision_conflict", current_revision: revision } }, 409));
        return;
      }
      const created: MockEvalExample = {
        example_id: `kb-failure-r${revision + 1}`,
        dataset_id: EVAL_DATASET_ID,
        tenant_id: "tenant-mock",
        split: "review",
        input: { query: body.query },
        expected_output: { answer: body.expected_answer },
        expected_trajectory: {},
        assertions: [],
        metadata: {
          case_id: body.case_id,
          case_revision: revision + 1,
          source: body.source,
          source_kind: "kb_failure",
          review_status: "pending",
          behavior_confirmed: false,
          kb_dataset_id: body.kb_dataset_id,
          kb_observed_segment_ids: body.observed_segment_ids,
          kb_source_versions: body.source_versions,
          kb_source_versions_verified: Array.isArray(body.source_versions) && body.source_versions.length > 0,
          kb_failure_reason: body.failure_reason,
          supersedes_example_id: latest?.example_id ?? null,
        },
        source_trace_id: typeof body.source_trace_id === "string" ? body.source_trace_id : null,
        created_by: "user-mock",
      };
      captured.savedFailureExamples.push(created);
      examples.push(created);
      await route.fulfill(jsonResponse({ example: created, created: true, revision: revision + 1 }));
      return;
    }

    if (
      method === "POST" &&
      pathname === `/api/v1/eval/datasets/${EVAL_DATASET_ID}/examples:import`
    ) {
      const body = request.postDataJSON() as {
        mode?: string;
        examples?: Array<Record<string, unknown>>;
      };
      captured.imports.push({ mode: body.mode, examples: body.examples ?? [] });

      // Mirror the backend repository: skip_duplicates dedupes on
      // metadata.case_id alone.
      const existingCaseIds = new Set(
        examples
          .map((example) => example.metadata?.case_id)
          .filter((caseId): caseId is string => typeof caseId === "string" && caseId !== "")
      );
      const created: MockEvalExample[] = [];
      let skipped = 0;
      for (const item of body.examples ?? []) {
        const caseId = typeof item.case_id === "string" ? item.case_id : "";
        if (body.mode !== "append" && existingCaseIds.has(caseId)) {
          skipped += 1;
          continue;
        }
        const example: MockEvalExample = {
          example_id: `ex-${caseId}`,
          case_id: caseId,
          dataset_id: EVAL_DATASET_ID,
          tenant_id: "tenant-mock",
          split: typeof item.split === "string" ? item.split : "regression",
          input: (item.input as Record<string, unknown>) ?? {},
          expected_output: (item.expected_output as Record<string, unknown>) ?? {},
          expected_trajectory: (item.expected_trajectory as Record<string, unknown>) ?? {},
          assertions: Array.isArray(item.assertions)
            ? (item.assertions as Array<Record<string, unknown>>)
            : [],
          metadata: {
            ...((item.metadata as Record<string, unknown>) ?? {}),
            case_id: caseId,
          },
          created_by: "user-mock",
          created_at: "2026-08-29T09:00:00Z",
        };
        examples.push(example);
        existingCaseIds.add(caseId);
        created.push(example);
      }
      await route.fulfill(
        jsonResponse({ imported: created.length, skipped, examples: created })
      );
      return;
    }

    if (
      method === "GET" &&
      pathname === `/api/v1/eval/datasets/${EVAL_DATASET_ID}/examples`
    ) {
      await route.fulfill(
        jsonResponse({ examples, total: examples.length, limit: 500, offset: 0 })
      );
      return;
    }

    const patchMatch = pathname.match(
      /^\/api\/v1\/eval\/datasets\/[^/]+\/examples\/([^/]+)$/
    );
    if (method === "PATCH" && patchMatch) {
      const exampleId = patchMatch[1];
      const body = request.postDataJSON() as Record<string, unknown>;
      captured.patches.push({ exampleId, body });
      const example = examples.find((entry) => entry.example_id === exampleId);
      const updated = { ...(example ?? {}), ...body, example_id: exampleId };
      await route.fulfill(jsonResponse(updated));
      return;
    }
    if (method === "DELETE" && patchMatch) {
      const exampleId = patchMatch[1];
      const index = examples.findIndex((entry) => entry.example_id === exampleId);
      if (index < 0) {
        await route.fulfill(jsonResponse({ detail: "Example not found" }, 404));
        return;
      }
      examples.splice(index, 1);
      captured.deletes.push(exampleId);
      await route.fulfill({ status: 204 });
      return;
    }

    await route.fulfill(jsonResponse({}));
  });

  return captured;
}

const JSONL_FIXTURE = [
  JSON.stringify({
    case_id: "kb-jsonl-1",
    input: { query: "会议室怎么预订？" },
    expected_output: { relevant_segment_ids: ["seg-meeting"] },
    expected_trajectory: {},
    assertions: [],
    metadata: { source: "manual" },
  }),
  JSON.stringify({
    case_id: "kb-jsonl-2",
    input: { query: "入职要领什么？" },
    expected_output: { relevant_segment_ids: ["seg-onboard"] },
    expected_trajectory: {},
    assertions: [],
    metadata: {},
  }),
].join("\n");

test.describe("@mock KB eval case persistence", () => {
  test("loads saved cases from the linked eval dataset on open", async ({ page }) => {
    const assertNoClientErrors = watchClientErrors(page);
    await installEvalCasesHarness(page);
    await page.goto(`/knowledge/${DATASET_ID}?tab=eval`);

    await expect(page.getByTestId("retrieval-eval-workbench")).toBeVisible();
    // The saved example materialises as a workbench case (query + labels).
    await expect(page.getByText("年假怎么申请？")).toBeVisible();
    await expect(
      page.getByLabel("正确分段 ID（逗号分隔）").first()
    ).toHaveValue("seg-annual-leave");
    await expect(page.getByTestId("save-eval-cases")).toBeEnabled();
    assertNoClientErrors();
  });

  test("save splits new cases through import and edits through PATCH", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const captured = await installEvalCasesHarness(page);
    await page.goto(`/knowledge/${DATASET_ID}?tab=eval`);

    await expect(page.getByText("年假怎么申请？")).toBeVisible();

    // Edit the loaded case: its case_id exists, so it must go through PATCH.
    await page
      .getByLabel("正确分段 ID（逗号分隔）")
      .first()
      .fill("seg-annual-leave, seg-extra");

    // New case: unseen case_id, so it must go through examples:import.
    await page.getByPlaceholder(/输入一个测试问题/).fill("报销需要什么材料？");
    await page.getByRole("button", { name: "添加" }).click();
    await page.getByLabel("正确分段 ID（逗号分隔）").nth(1).fill("seg-reimburse");

    await page.getByTestId("save-eval-cases").click();

    await expect.poll(() => captured.imports.length).toBe(1);
    await expect.poll(() => captured.patches.length).toBe(1);

    expect(captured.imports[0].mode).toBe("skip_duplicates");
    expect(captured.imports[0].examples).toHaveLength(1);
    const imported = captured.imports[0].examples[0];
    expect(String(imported.case_id)).toMatch(/^case_/);
    expect(imported.input).toEqual({ query: "报销需要什么材料？" });
    expect(imported.expected_output).toEqual({ relevant_segment_ids: ["seg-reimburse"] });
    expect(imported.metadata).toMatchObject({ kb_dataset_id: DATASET_ID });
    // validate_case-required fields are all present on the wire.
    expect(imported.expected_trajectory).toEqual({});
    expect(imported.assertions).toEqual([]);

    expect(captured.patches[0].exampleId).toBe("ex-saved-1");
    expect(captured.patches[0].body.input).toEqual({ query: "年假怎么申请？" });
    expect(captured.patches[0].body.expected_output).toEqual({
      relevant_segment_ids: ["seg-annual-leave", "seg-extra"],
    });
    // PATCH must keep metadata.case_id or skip_duplicates loses the link.
    expect(captured.patches[0].body.metadata).toMatchObject({ case_id: "case_saved" });

    // exact: Radix renders an aria-live twin of every toast.
    await expect(page.getByText("评测用例已保存", { exact: true })).toBeVisible();
    assertNoClientErrors();
  });

  test("deletes a persisted case and keeps it deleted after reload", async ({ page }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const captured = await installEvalCasesHarness(page);
    await page.goto(`/knowledge/${DATASET_ID}?tab=eval`);

    await expect(page.getByText("年假怎么申请？")).toBeVisible();
    await page.getByRole("button", { name: "删除测试用例" }).click();
    await expect(page.getByText("年假怎么申请？")).toHaveCount(0);
    // The last persisted case can still be saved even though the local list is empty.
    await expect(page.getByTestId("save-eval-cases")).toBeEnabled();

    const deleteRequest = page.waitForRequest(
      (request) =>
        request.url().includes(
          `/api/v1/eval/datasets/${EVAL_DATASET_ID}/examples/ex-saved-1`
        ) && request.method() === "DELETE"
    );
    await page.getByTestId("save-eval-cases").click();
    await deleteRequest;

    await expect.poll(() => captured.deletes).toEqual(["ex-saved-1"]);
    await expect(page.getByText("评测用例已保存", { exact: true })).toBeVisible();
    await expect(
      page.getByText("新增 0 个，更新 0 个，删除 1 个，未变 0 个", { exact: true })
    ).toBeVisible();

    await page.reload();
    await expect(page.getByTestId("retrieval-eval-workbench")).toBeVisible();
    await expect(page.getByText("年假怎么申请？")).toHaveCount(0);
    expect(captured.deletes).toEqual(["ex-saved-1"]);
    assertNoClientErrors();
  });

  test("creates the linked eval dataset lazily on first save", async ({ page }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const captured = await installEvalCasesHarness(page, { linkedDataset: false });
    await page.goto(`/knowledge/${DATASET_ID}?tab=eval`);

    // No linked eval dataset yet: reading must not create one.
    await expect(page.getByTestId("save-eval-cases")).toBeDisabled();
    expect(captured.createdDatasets).toHaveLength(0);

    await page.getByPlaceholder(/输入一个测试问题/).fill("加班怎么调休？");
    await page.getByRole("button", { name: "添加" }).click();
    await page.getByLabel("正确分段 ID（逗号分隔）").first().fill("seg-overtime");
    await page.getByTestId("save-eval-cases").click();

    await expect.poll(() => captured.createdDatasets.length).toBe(1);
    expect(captured.createdDatasets[0].metadata).toMatchObject({
      kb_dataset_id: DATASET_ID,
      source: "kb-retrieval-workbench",
    });
    await expect.poll(() => captured.imports.length).toBe(1);
    expect(captured.imports[0].examples[0].input).toEqual({ query: "加班怎么调休？" });
    await expect(page.getByText("评测用例已保存", { exact: true })).toBeVisible();
    assertNoClientErrors();
  });

  test("JSONL import validates, imports, and reloads the workbench list", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const captured = await installEvalCasesHarness(page);
    await page.goto(`/knowledge/${DATASET_ID}?tab=eval`);

    await expect(page.getByText("年假怎么申请？")).toBeVisible();
    await page.getByTestId("eval-jsonl-import-open").click();

    const dialog = page.getByRole("dialog");
    await expect(dialog).toContainText("导入评测用例（JSONL）");
    await dialog.getByTestId("eval-jsonl-textarea").fill(JSONL_FIXTURE);
    await expect(dialog.getByRole("status")).toContainText("2 个用例校验通过");

    const importRequest = page.waitForRequest(
      (request) =>
        request.url().includes(`/api/v1/eval/datasets/${EVAL_DATASET_ID}/examples:import`) &&
        request.method() === "POST"
    );
    await dialog.getByTestId("eval-jsonl-submit").click();

    const body = (await importRequest).postDataJSON() as {
      mode?: string;
      examples?: Array<Record<string, unknown>>;
    };
    expect(body.mode).toBe("skip_duplicates");
    expect(body.examples?.map((item) => item.case_id)).toEqual(["kb-jsonl-1", "kb-jsonl-2"]);
    // The route handler records the body asynchronously after the request fires.
    await expect.poll(() => captured.imports.length).toBeGreaterThan(0);

    await expect(page.getByText("JSONL 导入完成", { exact: true })).toBeVisible();
    // Dialog closes and the reload shows imported queries in the workbench.
    await expect(dialog).toHaveCount(0);
    await expect(page.getByText("会议室怎么预订？")).toBeVisible();
    await expect(page.getByText("入职要领什么？")).toBeVisible();
    assertNoClientErrors();
  });

  test("JSONL import surfaces validation errors without sending a request", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const captured = await installEvalCasesHarness(page);
    await page.goto(`/knowledge/${DATASET_ID}?tab=eval`);

    await page.getByTestId("eval-jsonl-import-open").click();
    const dialog = page.getByRole("dialog");
    // Missing case_id -> validate_case rejects before any network call.
    await dialog
      .getByTestId("eval-jsonl-textarea")
      .fill(JSON.stringify({ input: { query: "没有 case_id" }, expected_output: {} }));

    await expect(dialog.getByRole("alert")).toContainText("case_id must be a non-empty string");
    await expect(dialog.getByTestId("eval-jsonl-submit")).toBeDisabled();
    expect(captured.imports).toHaveLength(0);
    assertNoClientErrors();
  });

  test("hit-test saves a pending failure with expectation and stable source version", async ({
    page,
  }) => {
    // The first latest-revision lookup is expected to be 404 for a new case.
    const assertNoClientErrors = watchClientErrors(page, [
      /^Failed to load resource: the server responded with a status of 404 \(Not Found\)$/,
    ]);
    const captured = await installEvalCasesHarness(page);
    await page.goto(`/knowledge/${DATASET_ID}?tab=retrieval`);

    await page.getByPlaceholder("请输入文本").first().fill("年假怎么申请？");
    await page.getByRole("button", { name: "测试" }).first().click();

    const sendButton = page.getByTestId("send-hits-to-eval");
    await expect(sendButton).toBeVisible();
    await sendButton.click();
    await page.getByLabel("期望答案").fill("年假申请应提前三天提交。");
    await page.getByLabel("失败原因（选填）").fill("召回了旧规则");
    await page.getByTestId("save-knowledge-failure").click();

    await expect.poll(() => captured.failureSaves.length).toBe(1);
    const sent = captured.failureSaves[0];
    expect(String(sent.case_id)).toMatch(/^kb-failure-kb-hit-test-[0-9a-f]{8}-[0-9a-f]{8}$/);
    expect(sent.query).toBe("年假怎么申请？");
    expect(sent.expected_answer).toBe("年假申请应提前三天提交。");
    expect(sent.expected_revision).toBe(0);
    expect(sent).toMatchObject({
      source: "kb-hit-test",
      kb_dataset_id: DATASET_ID,
      observed_segment_ids: ["seg-hit-1", "seg-hit-2"],
      source_versions: [{
        kb_dataset_id: DATASET_ID,
        document_id: "doc-handbook",
        segment_id: "seg-hit-1",
        source_version: 2,
        source_hash: "a".repeat(64),
      }, {
        kb_dataset_id: DATASET_ID,
        document_id: "doc-handbook",
        segment_id: "seg-hit-2",
        source_version: 2,
        source_hash: "b".repeat(64),
      }],
    });
    expect(captured.savedFailureExamples[0].metadata).toMatchObject({
      source_kind: "kb_failure", review_status: "pending", case_revision: 1,
    });
    await expect(page.getByText("已保存到 Eval 审核队列", { exact: true })).toBeVisible();

    // An exact retry returns the same immutable revision without claiming an update.
    await sendButton.click();
    await page.getByLabel("期望答案").fill("年假申请应提前三天提交。");
    await page.getByLabel("失败原因（选填）").fill("召回了旧规则");
    await page.getByTestId("save-knowledge-failure").click();
    await expect.poll(() => captured.failureSaves.length).toBe(2);
    expect(captured.failureSaves[1].expected_revision).toBe(1);
    expect(captured.savedFailureExamples).toHaveLength(1);
    await expect(page.getByText("样本未更新", { exact: true })).toBeVisible();

    // Changing the expectation appends revision 2; revision 1 keeps its meaning.
    await sendButton.click();
    await page.getByLabel("期望答案").fill("年假申请应提前四天提交。");
    await page.getByLabel("失败原因（选填）").fill("期望答案已修正");
    await page.getByTestId("save-knowledge-failure").click();
    await expect.poll(() => captured.failureSaves.length).toBe(3);
    expect(captured.failureSaves[2].expected_revision).toBe(1);
    expect(captured.savedFailureExamples).toHaveLength(2);
    expect(captured.savedFailureExamples[0].expected_output.answer).toBe("年假申请应提前三天提交。");
    expect(captured.savedFailureExamples[1].expected_output.answer).toBe("年假申请应提前四天提交。");
    expect(captured.savedFailureExamples[1].metadata.case_revision).toBe(2);
    assertNoClientErrors();
  });

  test("zero-hit retrieval can still save a pending case without invented sources", async ({ page }) => {
    const assertNoClientErrors = watchClientErrors(page, [
      /^Failed to load resource: the server responded with a status of 404 \(Not Found\)$/,
    ]);
    const captured = await installEvalCasesHarness(page, { zeroHits: true });
    await page.goto(`/knowledge/${DATASET_ID}?tab=retrieval`);

    await page.getByPlaceholder("请输入文本").first().fill("找不到的规则？");
    await page.getByRole("button", { name: "测试" }).first().click();
    await page.getByTestId("send-hits-to-eval").click();
    await expect(page.getByTestId("save-knowledge-failure-dialog")).toContainText("0 / 0");
    await page.getByLabel("期望答案").fill("应明确说明资料不足。");
    await page.getByTestId("save-knowledge-failure").click();

    await expect.poll(() => captured.failureSaves.length).toBe(1);
    expect(captured.savedFailureExamples[0].metadata).toMatchObject({
      review_status: "pending",
      kb_observed_segment_ids: [],
      kb_source_versions: [],
    });
    assertNoClientErrors();
  });

  test("KB readers without Eval run permission see why saving is unavailable", async ({ page }) => {
    const captured = await installEvalCasesHarness(page, { evalRunPermission: false });
    await page.goto(`/knowledge/${DATASET_ID}?tab=retrieval`);
    await page.getByPlaceholder("请输入文本").first().fill("年假怎么申请？");
    await page.getByRole("button", { name: "测试" }).first().click();
    await expect(page.getByTestId("send-hits-to-eval")).toBeDisabled();
    await expect(page.getByRole("note")).toContainText("需要 Eval 运行权限");
    expect(captured.failureSaves).toHaveLength(0);
  });

  test("a mixed result with missing source versions cannot enter the review store", async ({ page }) => {
    const captured = await installEvalCasesHarness(page, { incompleteSource: true });
    await page.goto(`/knowledge/${DATASET_ID}?tab=retrieval`);
    await page.getByPlaceholder("请输入文本").first().fill("年假怎么申请？");
    await page.getByRole("button", { name: "测试" }).first().click();
    await page.getByTestId("send-hits-to-eval").click();
    await page.getByLabel("期望答案").fill("应提前三天申请。");
    await expect(page.getByTestId("save-knowledge-failure-dialog").getByRole("alert")).toContainText("切片缺少稳定来源版本");
    await expect(page.getByTestId("save-knowledge-failure")).toBeDisabled();
    expect(captured.failureSaves).toHaveLength(0);
  });

  test("workbench persistence controls stay usable at mobile width", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page);
    await page.setViewportSize({ width: 375, height: 812 });
    await installEvalCasesHarness(page);
    await page.goto(`/knowledge/${DATASET_ID}?tab=eval`);

    await expect(page.getByText("年假怎么申请？")).toBeVisible();
    await expect(page.getByTestId("save-eval-cases")).toBeVisible();
    await expect(page.getByTestId("eval-jsonl-import-open")).toBeVisible();

    const scrollWidth = await page.evaluate(() => document.documentElement.scrollWidth);
    expect(scrollWidth).toBeLessThanOrEqual(375);
    assertNoClientErrors();
  });
});
