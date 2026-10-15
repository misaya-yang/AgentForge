import { expect, test } from "@playwright/test";
import { installClientAuth, seedClientPrefs } from "./support/helpers";

test("controlled lost create response reopens the same dataset without a second create", async ({ page }) => {
  await installClientAuth(page, {
    permissions: ["console:dashboard:view", "knowledge:dataset:view", "knowledge:dataset:create"],
    effective_permissions: ["console:dashboard:view", "knowledge:dataset:view", "knowledge:dataset:create"],
  });
  await seedClientPrefs(page, { locale: "zh-CN" });

  let createCalls = 0;
  let createdId = "";
  let createdName = "";
  await page.route("**/api/v1/knowledge/datasets**", async (route) => {
    const request = route.request();
    const pathname = new URL(request.url()).pathname;
    if (request.method() === "POST" && pathname === "/api/v1/knowledge/datasets") {
      createCalls += 1;
      const body = request.postDataJSON() as { dataset_id: string; name: string };
      createdId = body.dataset_id;
      createdName = body.name;
      expect(createdId).toMatch(/^kb_[a-f0-9]{32}$/);
      // Fault injection: the server accepted the create, but the response was lost.
      await route.abort("failed");
      return;
    }
    if (request.method() === "GET" && pathname === `/api/v1/knowledge/datasets/${createdId}`) {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          dataset_id: createdId,
          name: createdName,
          description: "",
          visibility: "private",
          embedding_provider: "dashscope",
          embedding_model: "text-embedding-v4",
          embedding_dimension: 1024,
          my_permission: "owner",
        }),
      });
      return;
    }
    if (request.method() === "GET" && pathname.startsWith("/api/v1/knowledge/datasets/")) {
      await route.fulfill({ status: 404, contentType: "application/json", body: "{}" });
      return;
    }
    await route.continue();
  });

  await page.goto("/knowledge/create");
  await page.getByPlaceholder("请输入知识库名称").fill("R2 响应丢失验收库");
  await page.getByRole("button", { name: "下一步" }).click();
  await page.getByRole("button", { name: "下一步" }).click();
  await page.getByRole("button", { name: "确认", exact: true }).click();

  await expect(page.getByText("知识库创建结果尚未确认", { exact: false })).toBeVisible();
  await expect(page.getByText("创建结果未知", { exact: true })).toBeVisible();
  await expect(page.getByText("创建失败", { exact: true })).toHaveCount(0);
  expect(createCalls).toBe(1);
  await page.reload();
  await expect(page.getByText("已找到上次创建的知识库：R2 响应丢失验收库")).toBeVisible();
  await expect(page.getByRole("button", { name: "打开已有知识库" })).toBeVisible();
  expect(createCalls).toBe(1);

  await page.getByRole("button", { name: "结束上次流程并新建" }).first().click();
  await expect(page.getByText("上次创建请求可能仍在处理中", { exact: false })).toBeVisible();
  await page.getByRole("alertdialog").getByRole("button", { name: "结束上次流程并新建" }).click();
  await expect(page.getByPlaceholder("请输入知识库名称")).toBeVisible();
  await expect(page.getByText("已找到上次创建的知识库：R2 响应丢失验收库")).toHaveCount(0);
  expect(createCalls).toBe(1);
});

test("controlled soft-deleted draft identity can be explicitly abandoned", async ({ page }) => {
  await installClientAuth(page, {
    permissions: ["console:dashboard:view", "knowledge:dataset:view", "knowledge:dataset:create"],
    effective_permissions: ["console:dashboard:view", "knowledge:dataset:view", "knowledge:dataset:create"],
  });
  await seedClientPrefs(page, { locale: "zh-CN" });
  let createCalls = 0;
  await page.route("**/api/v1/knowledge/datasets**", async (route) => {
    if (route.request().method() === "POST") createCalls += 1;
    await route.fulfill({ status: 404, contentType: "application/json", body: "{}" });
  });
  await page.goto("/knowledge/create");
  await page.evaluate(() => sessionStorage.setItem(
    "kb-create-dataset:e2e-client-user",
    "kb_soft_deleted_previous_request"
  ));
  await page.reload();
  await expect(page.getByRole("button", { name: "结束上次流程并新建" })).toBeVisible();
  await page.getByRole("button", { name: "结束上次流程并新建" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "结束上次流程并新建" }).click();
  await expect(page.getByPlaceholder("请输入知识库名称")).toBeVisible();
  expect(await page.evaluate(() => sessionStorage.getItem("kb-create-dataset:e2e-client-user"))).toBeNull();
  expect(createCalls).toBe(0);
});
