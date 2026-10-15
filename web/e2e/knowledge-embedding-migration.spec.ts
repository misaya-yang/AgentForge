import { expect, test, type Page } from "@playwright/test";

import { installClientAuth, seedClientPrefs } from "./support/helpers";

const DATASET_ID = "mock-embedding-migration";
const MIGRATION_ID = "3f2c1a4e-9b8d-4e6f-8a1b-2c3d4e5f6071";
const FIRST_JOB_ID = "4f2c1a4e-9b8d-4e6f-8a1b-2c3d4e5f6072";
const MIGRATION_PATH = `/api/v1/knowledge/datasets/${DATASET_ID}/embedding-migration`;

type MigrationState =
  | "shadow_build"
  | "backfilling"
  | "verified"
  | "gating"
  | "gate_failed"
  | "ready"
  | "completed"
  | "rolled_back"
  | "failed"
  | "abandoned";

function gateReceipt(state: MigrationState): Record<string, unknown> | null {
  if (["ready", "completed", "rolled_back"].includes(state)) {
    return {
      passed: true,
      samples: 8,
      shadow_hit_rate: 1,
      serving_hit_rate: 0.875,
    };
  }
  if (state === "gate_failed") {
    return {
      passed: false,
      samples: 8,
      shadow_hit_rate: 0.5,
      serving_hit_rate: 0.875,
    };
  }
  return null;
}

function healthStatus(state: MigrationState | null): "healthy" | "drifted" | "unknown" {
  if (!state) return "unknown";
  if (state === "failed") return "drifted";
  if (state === "abandoned") return "unknown";
  return "healthy";
}

function actionErrorDetail(status: 404 | 409 | 503): string {
  switch (status) {
    case 409:
      return "embedding migration backfill is already running";
    case 404:
      return "migration not found";
    case 503:
      return "embedding migration store unavailable";
  }
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
      `Console errors:\n${unexpectedConsoleErrors.join("\n")}`
    ).toEqual([]);
  };
}

function servingBinding() {
  return {
    binding_id: "11111111-1111-4111-8111-111111111111",
    dataset_id: DATASET_ID,
    tenant_id: "tenant-a",
    collection_name: `kb_${DATASET_ID}_1024_serving`,
    embedding_provider: "dashscope",
    embedding_model: "text-embedding-v3",
    embedding_model_version: "2026-01",
    embedding_dimension: 1024,
    capabilities: ["text"],
    state: "serving",
    activated_at: "2026-08-28T10:00:00Z",
  };
}

function targetBinding(state: MigrationState) {
  const bindingState = state === "completed" ? "serving" : "shadow";
  return {
    binding_id: "22222222-2222-4222-8222-222222222222",
    dataset_id: DATASET_ID,
    tenant_id: "tenant-a",
    collection_name: `kb_${DATASET_ID}_1024_candidate`,
    embedding_provider: "dashscope",
    embedding_model: "text-embedding-v4",
    embedding_model_version: "2026-08",
    embedding_dimension: 1024,
    capabilities: ["text"],
    state: bindingState,
    created_at: "2026-08-29T09:00:00Z",
  };
}

function migrationJob(state: MigrationState, pending: number) {
  return {
    migration_id: MIGRATION_ID,
    dataset_id: DATASET_ID,
    source_binding_id: servingBinding().binding_id,
    target_binding_id: targetBinding(state).binding_id,
    state,
    checkpoint: { last_round: 1 },
    totals: {
      target_collection: targetBinding(state).collection_name,
      enabled_chunks: 10,
      pending_after_backfill: pending,
      verified_enabled_chunks: pending === 0 ? 10 : undefined,
      verified_points: pending === 0 ? 10 : undefined,
    },
    gate: gateReceipt(state),
    error: state === "failed" ? "provider timeout; old collection remains serving" : null,
    created_at: "2026-08-29T09:00:00Z",
    updated_at: "2026-08-29T09:05:00Z",
  };
}

type ActionName = "backfill" | "verify" | "gate";
type ActionJobState = "queued" | "running" | "succeeded" | "failed" | "cancelled";

interface MockActionJob {
  job_id: string;
  migration_id: string;
  dataset_id: string;
  action: ActionName;
  state: ActionJobState;
  payload: Record<string, unknown>;
  result: Record<string, unknown> | null;
  error: string | null;
  attempt_count: number;
  request_hash: string;
  reused?: boolean;
  poll_after_ms: number;
  created_at: string;
  updated_at: string;
}

function makeActionJob(
  action: ActionName,
  state: ActionJobState = "queued",
  overrides: Partial<MockActionJob> = {}
): MockActionJob {
  return {
    job_id: FIRST_JOB_ID,
    migration_id: MIGRATION_ID,
    dataset_id: DATASET_ID,
    action,
    state,
    payload: {},
    result: null,
    error: null,
    attempt_count: state === "queued" ? 0 : 1,
    request_hash: `${action}-request-hash`,
    poll_after_ms: 500,
    created_at: "2026-08-29T09:06:00Z",
    updated_at: "2026-08-29T09:06:00Z",
    ...overrides,
  };
}

interface MockHarness {
  setState: (state: MigrationState | null, pending?: number) => void;
  setActionJob: (job: MockActionJob | null) => void;
  setAutoCompleteAfterPolls: (count: number) => void;
  failNextJobPoll: (status: 404 | 503) => void;
  hideActiveJobFromDescribe: (hidden: boolean) => void;
  failNextAction: (status: 404 | 409 | 503) => void;
  jobPollCount: () => number;
  currentJob: () => MockActionJob | null;
  actionBodies: Array<{ action: string; body: Record<string, unknown> }>;
}

async function installEmbeddingMigrationHarness(page: Page): Promise<MockHarness> {
  let state: MigrationState | null = null;
  let pending = 10;
  let nextErrorStatus: 404 | 409 | 503 | null = null;
  let nextJobPollError: 404 | 503 | null = null;
  let currentActionJob: MockActionJob | null = null;
  let terminalActionJobs: MockActionJob[] = [];
  let actionJobCounter = 0;
  let currentJobPolls = 0;
  let totalJobPolls = 0;
  let autoCompleteAfterPolls = 2;
  let hideActiveJob = false;
  const actionBodies: Array<{ action: string; body: Record<string, unknown> }> = [];

  await installClientAuth(page, {
    permissions: ["console:dashboard:view", "knowledge:dataset:view"],
    effective_permissions: ["console:dashboard:view", "knowledge:dataset:view"],
  });
  await seedClientPrefs(page, { locale: "zh-CN" });

  function nextJobId(): string {
    const suffix = String(6072 + actionJobCounter).padStart(4, "0");
    actionJobCounter += 1;
    return `4f2c1a4e-9b8d-4e6f-8a1b-2c3d4e5f${suffix}`;
  }

  function finishCurrentJob(resultState: "succeeded" | "failed", error: string | null = null) {
    if (!currentActionJob) return;
    const finishedJob: MockActionJob = {
      ...currentActionJob,
      state: resultState,
      result:
        resultState === "succeeded"
          ? { action: currentActionJob.action, completed: true }
          : null,
      error,
      updated_at: "2026-08-29T09:07:00Z",
    };
    currentActionJob = finishedJob;
    terminalActionJobs = [
      finishedJob,
      ...terminalActionJobs.filter((job) => job.job_id !== finishedJob.job_id),
    ];
    if (resultState !== "succeeded") return;
    switch (finishedJob.action) {
      case "backfill":
        state = "backfilling";
        pending = 0;
        break;
      case "verify":
        state = "verified";
        break;
      case "gate":
        state = "ready";
        break;
    }
  }

  function enqueueActionJob(
    action: ActionName,
    payload: Record<string, unknown>
  ): MockActionJob | "conflict" {
    if (
      currentActionJob &&
      ["queued", "running"].includes(currentActionJob.state)
    ) {
      if (currentActionJob.action !== action) return "conflict";
      return { ...currentActionJob, reused: true };
    }
    if (
      currentActionJob?.state === "failed" &&
      currentActionJob.action === action &&
      !String(currentActionJob.error || "").includes("cancelled")
    ) {
      const retriedJob: MockActionJob = {
        ...currentActionJob,
        state: "queued",
        payload,
        result: null,
        error: null,
        reused: true,
        poll_after_ms: 500,
      };
      currentActionJob = retriedJob;
      terminalActionJobs = terminalActionJobs.filter(
        (job) => job.job_id !== retriedJob.job_id
      );
      currentJobPolls = 0;
      return retriedJob;
    }
    currentActionJob = makeActionJob(action, "queued", {
      job_id: nextJobId(),
      payload,
    });
    currentJobPolls = 0;
    return currentActionJob;
  }

  function advanceCurrentJob(): MockActionJob | null {
    if (!currentActionJob) return null;
    currentJobPolls += 1;
    totalJobPolls += 1;
    if (currentActionJob.state === "queued") {
      currentActionJob = {
        ...currentActionJob,
        state: "running",
        attempt_count: currentActionJob.attempt_count + 1,
      };
      if (currentActionJob.action === "backfill") state = "backfilling";
    }
    if (
      currentActionJob.state === "running" &&
      currentJobPolls >= autoCompleteAfterPolls
    ) {
      finishCurrentJob("succeeded");
    }
    return currentActionJob;
  }

  await page.route("**/api/v1/knowledge/**", async (route) => {
    const request = route.request();
    const method = request.method();
    const pathname = new URL(request.url()).pathname;

    if (method === "GET" && pathname === `/api/v1/knowledge/datasets/${DATASET_ID}`) {
      await route.fulfill(
        jsonResponse({
          dataset_id: DATASET_ID,
          name: "Embedding Migration Mock",
          description: "Hermetic H1 #27/#28 contract",
          visibility: "private",
          embedding_provider: "dashscope",
          embedding_model: "text-embedding-v3",
          embedding_dimension: 1024,
          collection_name: servingBinding().collection_name,
          statistics: { document_count: 2, segment_count: 10 },
        })
      );
      return;
    }

    if (method === "GET" && pathname === `/api/v1/knowledge/${DATASET_ID}/documents`) {
      await route.fulfill(jsonResponse([]));
      return;
    }

    if (method === "GET" && pathname === `/api/v1/knowledge/${DATASET_ID}/config`) {
      await route.fulfill(
        jsonResponse({
          dataset_id: DATASET_ID,
          chunking: { mode: "automatic", chunk_size: 500, chunk_overlap: 50 },
          retrieval: {
            mode: "hybrid",
            top_k: 5,
            score_threshold: 0.3,
            fusion: { strategy: "rrf", rrf_k: 60, alpha: 0.7 },
            rerank: { enabled: false, model: "gte-rerank" },
            mmr: { enabled: false, lambda: 0.5 },
          },
          embedding: {
            provider: "dashscope",
            model: "text-embedding-v3",
            dimension: 1024,
            collection_name: servingBinding().collection_name,
          },
          statistics: {
            document_count: 2,
            available_document_count: 2,
            segment_count: 10,
            available_segment_count: 10,
            word_count: 500,
            hit_count: 9,
          },
        })
      );
      return;
    }

    if (
      method === "GET" &&
      pathname === `/api/v1/knowledge/${DATASET_ID}/metadata-schema`
    ) {
      await route.fulfill(jsonResponse({ version: 1, revision: 0, fields: [] }));
      return;
    }

    if (method === "GET" && pathname === `/api/v1/knowledge/${DATASET_ID}/debug`) {
      await route.fulfill(
        jsonResponse({
          dataset: {
            id: DATASET_ID,
            name: "Embedding Migration Mock",
            embedding_provider: "dashscope",
            embedding_model: "text-embedding-v3",
            embedding_dimension: 1024,
            collection_name: servingBinding().collection_name,
          },
          database: { documents: 2, segments: 10 },
          vector_store: { collection_exists: true, points_count: 10 },
        })
      );
      return;
    }

    if (method === "GET" && pathname === MIGRATION_PATH) {
      const job = state ? migrationJob(state, pending) : null;
      const isLive = state && ["shadow_build", "backfilling", "verified", "gating", "ready"].includes(state);
      const currentServing = state === "completed" ? targetBinding(state) : servingBinding();
      const describedActionJob = currentActionJob
        ? { ...currentActionJob, reused: undefined, poll_after_ms: undefined }
        : null;
      await route.fulfill(
        jsonResponse({
          dataset_id: DATASET_ID,
          serving_binding: currentServing,
          live_migration: isLive || state === "failed" || state === "gate_failed" ? job : null,
          latest_migration: job,
          recent_migrations: job ? [job] : [],
          active_action_job:
            !hideActiveJob &&
            describedActionJob &&
            ["queued", "running"].includes(describedActionJob.state)
              ? describedActionJob
              : null,
          recent_action_jobs: terminalActionJobs.map((actionJob) => ({
            ...actionJob,
            reused: undefined,
            poll_after_ms: undefined,
          })),
          source_binding: servingBinding(),
          target_binding: state ? targetBinding(state) : null,
          collection_health: {
            status: healthStatus(state),
            checked_live: Boolean(state),
            collection_name: state ? targetBinding(state).collection_name : null,
            pending_chunks: state ? pending : null,
            authority: state
              ? {
                  authority_kind: "postgresql_enabled_segments",
                  dataset_id: DATASET_ID,
                  tenant_id: "tenant-a",
                  serving_collection_name: currentServing.collection_name,
                  point_count: 10,
                  point_ids_sha256: "authority-points-sha",
                  source_text_sha256: "authority-text-sha",
                }
              : null,
            target_scope: state
              ? {
                  point_count: Math.max(10 - pending, 0),
                  point_ids_sha256: "target-points-sha",
                  source_text_sha256: "target-text-sha",
                }
              : null,
            verified_authority: pending === 0 && state ? { point_count: 10 } : null,
            verified_target_scope: pending === 0 && state ? { point_count: 10 } : null,
            gate_report: state && ["ready", "completed", "rolled_back"].includes(state)
              ? { passed: true }
              : null,
            reason: state === "failed" ? "target scope drifted from authority" : null,
          },
          pending_chunks: state && ["shadow_build", "backfilling", "failed"].includes(state) ? pending : null,
          enabled_chunks: 10,
        })
      );
      return;
    }

    if (
      method === "GET" &&
      pathname.startsWith(`${MIGRATION_PATH}/`) &&
      pathname.includes("/jobs/")
    ) {
      if (nextJobPollError) {
        const status = nextJobPollError;
        nextJobPollError = null;
        await route.fulfill(
          jsonResponse(
            {
              detail:
                status === 404
                  ? "migration job not found"
                  : "embedding migration action jobs unavailable",
            },
            status
          )
        );
        return;
      }
      const jobId = pathname.split("/").at(-1);
      if (!currentActionJob || currentActionJob.job_id !== jobId) {
        await route.fulfill(jsonResponse({ detail: "migration job not found" }, 404));
        return;
      }
      const polled = advanceCurrentJob();
      await route.fulfill(jsonResponse(polled));
      return;
    }

    if (method === "POST" && pathname.startsWith(MIGRATION_PATH)) {
      const action = pathname.slice(MIGRATION_PATH.length + 1).split("/").at(-1) ?? "";
      const body = (request.postDataJSON() ?? {}) as Record<string, unknown>;
      actionBodies.push({ action, body });
      if (nextErrorStatus) {
        const status = nextErrorStatus;
        nextErrorStatus = null;
        await route.fulfill(
          jsonResponse(
            {
              detail: actionErrorDetail(status),
            },
            status
          )
        );
        return;
      }

      if (action === "backfill" || action === "verify" || action === "gate") {
        const enqueued = enqueueActionJob(action, body);
        if (enqueued === "conflict") {
          await route.fulfill(
            jsonResponse({ detail: "migration already has an active durable job" }, 409)
          );
          return;
        }
        await route.fulfill(jsonResponse(enqueued, 202));
        return;
      }

      switch (action) {
        case "start":
          state = "shadow_build";
          pending = 10;
          break;
        case "cutover":
          state = "completed";
          break;
        case "rollback":
          state = "rolled_back";
          break;
        case "abort":
          if (currentActionJob?.state === "running") {
            await route.fulfill(
              jsonResponse({ detail: "cannot abandon while durable job is running" }, 409)
            );
            return;
          }
          if (currentActionJob?.state === "queued") {
            finishCurrentJob("failed", "cancelled by migration abort");
          }
          state = "abandoned";
          break;
      }
      await route.fulfill(jsonResponse({ migration: state ? migrationJob(state, pending) : null }));
      return;
    }

    if (method === "GET" && pathname === "/api/v1/knowledge/retrieval/presets") {
      await route.fulfill(jsonResponse({ presets: [], recommended_default: "", notes: {} }));
      return;
    }

    await route.fulfill(jsonResponse([]));
  });

  return {
    setState(nextState, nextPending = 0) {
      state = nextState;
      pending = nextPending;
      currentActionJob = null;
      terminalActionJobs = [];
      currentJobPolls = 0;
    },
    setActionJob(job) {
      currentActionJob = job;
      currentJobPolls = 0;
      terminalActionJobs = [];
      if (job && ["succeeded", "failed", "cancelled"].includes(job.state)) {
        terminalActionJobs.push(job);
      }
    },
    setAutoCompleteAfterPolls(count) {
      autoCompleteAfterPolls = count;
    },
    failNextJobPoll(status) {
      nextJobPollError = status;
    },
    hideActiveJobFromDescribe(hidden) {
      hideActiveJob = hidden;
    },
    failNextAction(status) {
      nextErrorStatus = status;
    },
    jobPollCount() {
      return totalJobPolls;
    },
    currentJob() {
      return currentActionJob;
    },
    actionBodies,
  };
}

async function openSettings(page: Page) {
  await page.goto(`/knowledge/${DATASET_ID}?tab=settings`);
  await expect(page.getByTestId("embedding-migration-panel")).toBeVisible();
}

function currentJobId(harness: MockHarness): string {
  const job = harness.currentJob();
  if (!job) throw new Error("expected mock durable job to exist");
  return job.job_id;
}

test.describe("@mock KB embedding migration", () => {
  test("renders every migration state from the backend after reload", async ({ page }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const harness = await installEmbeddingMigrationHarness(page);
    await openSettings(page);
    await expect(page.getByTestId("embedding-no-job")).toBeVisible();
    await expect(page.getByTestId("embedding-lexical-blocked")).toContainText("BLOCKED");
    await expect(page.getByTestId("embedding-collection-health")).toContainText("未知");
    await expect(page.getByText("必须使用蓝绿流程", { exact: true })).toBeVisible();

    const states: Array<[MigrationState, string]> = [
      ["shadow_build", "影子已创建"],
      ["backfilling", "回填中"],
      ["verified", "完整性已校验"],
      ["gating", "评测门控执行中"],
      ["gate_failed", "评测门控失败"],
      ["ready", "可执行切换"],
      ["completed", "切换完成"],
      ["rolled_back", "已回滚"],
      ["failed", "迁移失败"],
      ["abandoned", "已中止"],
    ];
    for (const [state, label] of states) {
      harness.setState(state, state === "backfilling" || state === "failed" ? 3 : 0);
      await page.reload();
      await expect(page.getByTestId("embedding-migration-state")).toHaveText(label);
      if (state === "failed") {
        await expect(page.getByTestId("embedding-collection-health")).toContainText("存在漂移");
      }
      if (state === "abandoned") {
        await expect(page.getByTestId("embedding-collection-health")).toContainText("未知");
      }
    }
    assertNoClientErrors();
  });

  test("drives start through rollback and restores controls from backend state", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const harness = await installEmbeddingMigrationHarness(page);
    await openSettings(page);

    await page.getByTestId("embedding-start-open").click();
    await page.getByTestId("embedding-start-next").click();
    await expect(page.getByTestId("embedding-start-review")).toContainText(
      "本操作只创建影子，不会切换流量"
    );
    await page.getByTestId("embedding-start-confirm").click();
    await expect(page.getByTestId("embedding-migration-state")).toHaveText("影子已创建");
    const startBody = harness.actionBodies.find((entry) => entry.action === "start")?.body;
    expect(startBody?.embedding_provider).toBeTruthy();
    expect(startBody?.embedding_model).toBeTruthy();
    expect(startBody?.embedding_dimension).toBe(1024);

    await page.reload();
    await expect(page.getByTestId("embedding-action-backfill")).toBeVisible();
    harness.setAutoCompleteAfterPolls(3);
    await page.getByTestId("embedding-action-backfill").click();
    await expect(page.getByTestId("embedding-action-job-state")).toContainText(
      /已排队|执行中/
    );
    const backfillJobId = currentJobId(harness);
    await page.reload();
    await expect(page.getByTestId("embedding-action-job")).toContainText(
      backfillJobId
    );
    await expect(page.getByTestId("embedding-action-job-state")).toHaveText("已成功");
    await expect(page.getByTestId("embedding-migration-state")).toHaveText("回填中");
    await expect(page.getByTestId("embedding-action-verify")).toBeVisible();

    await page.getByTestId("embedding-action-verify").click();
    await expect(page.getByTestId("embedding-action-job-state")).toHaveText("已成功");
    await expect(page.getByTestId("embedding-migration-state")).toHaveText("完整性已校验");
    await page.getByTestId("embedding-action-gate").click();
    await expect(page.getByTestId("embedding-action-job-state")).toHaveText("已成功");
    await expect(page.getByTestId("embedding-migration-state")).toHaveText("可执行切换");

    await page.getByTestId("embedding-action-cutover").click();
    await expect(page.getByTestId("embedding-action-confirmation")).toBeVisible();
    await page.getByTestId("embedding-action-confirm").click();
    await expect(page.getByTestId("embedding-migration-state")).toHaveText("切换完成");
    expect(
      harness.actionBodies.find((entry) => entry.action === "cutover")?.body.retention_seconds
    ).toBe(604800);

    await page.reload();
    await expect(page.getByTestId("embedding-action-rollback")).toBeVisible();
    await page.getByTestId("embedding-action-rollback").click();
    await page.getByTestId("embedding-action-confirm").click();
    await expect(page.getByTestId("embedding-migration-state")).toHaveText("已回滚");
    expect(
      harness.actionBodies.find((entry) => entry.action === "rollback")?.body.keep_shadow
    ).toBe(true);

    await page.getByTestId("embedding-action-abort").click();
    await page.getByTestId("embedding-action-confirm").click();
    await expect(page.getByTestId("embedding-migration-state")).toHaveText("已中止");
    await expect(page.getByTestId("embedding-start-open")).toBeVisible();
    assertNoClientErrors();
  });

  test("keeps polling beyond 30s, resumes after reload, and retries a 503 poll", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page, [
      /server responded with a status of 503/,
    ]);
    const harness = await installEmbeddingMigrationHarness(page);
    harness.setState("shadow_build", 10);
    harness.setAutoCompleteAfterPolls(Number.POSITIVE_INFINITY);
    await openSettings(page);

    await page.getByTestId("embedding-action-backfill").click();
    const jobId = currentJobId(harness);
    await expect(page.getByTestId("embedding-action-job")).toContainText(
      jobId
    );
    harness.failNextJobPoll(503);
    const failedPoll = page.waitForResponse(
      (response) =>
        response.url().includes(`/embedding-migration/${MIGRATION_ID}/jobs/${jobId}`) &&
        response.status() === 503
    );
    await page.reload();
    await failedPoll;
    await expect(page.getByTestId("embedding-action-job")).toContainText(
      jobId
    );
    await expect(page.getByTestId("embedding-action-job-poll-error")).toBeVisible();
    await page
      .getByTestId("embedding-action-job-poll-error")
      .getByRole("button")
      .click();
    await expect(page.getByTestId("embedding-action-job-poll-error")).toHaveCount(0);

    await page.waitForTimeout(31_000);
    expect(harness.jobPollCount()).toBeGreaterThan(20);
    await expect(page.getByTestId("embedding-action-job-state")).toHaveText("执行中");
    assertNoClientErrors();
  });

  test("shows concurrent reuse and retries a failed action with the same job id", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const harness = await installEmbeddingMigrationHarness(page);
    harness.setState("shadow_build", 10);
    harness.setAutoCompleteAfterPolls(Number.POSITIVE_INFINITY);
    harness.hideActiveJobFromDescribe(true);
    await openSettings(page);

    const externalStatus = await page.evaluate(async ({ path, migrationId }) => {
      const response = await fetch(`${path}/${migrationId}/backfill`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: "{}",
      });
      return response.status;
    }, { path: MIGRATION_PATH, migrationId: MIGRATION_ID });
    expect(externalStatus).toBe(202);
    const reusedJobId = currentJobId(harness);
    await page.getByTestId("embedding-action-backfill").click();
    await expect(page.getByTestId("embedding-action-job-reused")).toBeVisible();
    await expect(page.getByTestId("embedding-action-job")).toContainText(
      reusedJobId
    );

    harness.hideActiveJobFromDescribe(false);
    harness.setState("failed", 3);
    harness.setActionJob(
      makeActionJob("backfill", "failed", {
        job_id: reusedJobId,
        attempt_count: 1,
        error: "provider timeout",
      })
    );
    await page.reload();
    await expect(page.getByTestId("embedding-action-job-state")).toHaveText("失败");
    await page.getByTestId("embedding-action-job-retry").click();
    await expect(page.getByTestId("embedding-action-job")).toContainText(
      reusedJobId
    );
    await expect(page.getByTestId("embedding-action-job-reused")).toBeVisible();
    await expect(page.getByTestId("embedding-action-job-state")).toContainText(
      /已排队|执行中/
    );
    assertNoClientErrors();
  });

  test("aborting a queued job records an explicit cancelled receipt", async ({ page }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const harness = await installEmbeddingMigrationHarness(page);
    harness.setState("shadow_build", 10);
    harness.setActionJob(makeActionJob("backfill", "queued"));
    harness.setAutoCompleteAfterPolls(Number.POSITIVE_INFINITY);
    await openSettings(page);

    await expect(page.getByTestId("embedding-action-job-state")).toHaveText("已排队");
    await page.getByTestId("embedding-action-abort").click();
    await page.getByTestId("embedding-action-confirm").click();
    await expect(page.getByTestId("embedding-migration-state")).toHaveText("已中止");
    await expect(page.getByTestId("embedding-action-job-state")).toHaveText("已取消");
    await expect(page.getByTestId("embedding-action-job-error")).toContainText(
      "cancelled by migration abort"
    );
    assertNoClientErrors();
  });

  test("dataset-scoped active job from an older migration remains visible and blocks actions", async ({
    page,
  }) => {
    const assertNoClientErrors = watchClientErrors(page);
    const harness = await installEmbeddingMigrationHarness(page);
    harness.setState("completed", 0);
    harness.setActionJob(
      makeActionJob("gate", "running", {
        migration_id: "5f2c1a4e-9b8d-4e6f-8a1b-2c3d4e5f6073",
      })
    );
    harness.setAutoCompleteAfterPolls(Number.POSITIVE_INFINITY);
    await openSettings(page);

    await expect(page.getByTestId("embedding-action-job")).toContainText(
      "5f2c1a4e-9b8d-4e6f-8a1b-2c3d4e5f6073"
    );
    await expect(page.getByTestId("embedding-start-open")).toHaveCount(0);
    await expect(page.getByTestId("embedding-action-rollback")).toHaveCount(0);
    assertNoClientErrors();
  });

  test("surfaces 409, 404, and 503 without treating them as success", async ({ page }) => {
    const assertNoClientErrors = watchClientErrors(page, [
      /server responded with a status of (409|404|503)/,
    ]);
    const harness = await installEmbeddingMigrationHarness(page);
    harness.setState("shadow_build", 10);
    await openSettings(page);

    for (const status of [409, 404, 503] as const) {
      harness.failNextAction(status);
      await page.getByTestId("embedding-action-backfill").click();
      await expect(page.getByTestId(`embedding-error-${status}`)).toBeVisible();
      await expect(page.getByTestId("embedding-migration-state")).toHaveText("影子已创建");
      await page.getByTestId("embedding-migration-refresh").click();
      await expect(page.getByTestId(`embedding-error-${status}`)).toHaveCount(0);
    }
    assertNoClientErrors();
  });
});
