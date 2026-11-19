import { createHash } from "node:crypto";
import { expect, test, type Page } from "./support/real-api";
import { buildAuthHeaders, getApiUrl } from "./support/helpers";

async function skillRequest(page: Page, name: string, method = "GET", suffix = "", body?: object) {
  return page.evaluate(async ({ name, method, suffix, body }) => {
    if (location.protocol !== "https:") throw new Error("Live Skills require trusted HTTPS BFF");
    const bootstrap = await fetch("/api/auth/session", { credentials: "same-origin", cache: "no-store" });
    if (!bootstrap.ok) throw new Error(`BFF bootstrap failed (${bootstrap.status})`);
    const { csrf_token: csrfToken } = await bootstrap.json() as { csrf_token?: string };
    if (!csrfToken) throw new Error("BFF CSRF bootstrap missing");
    const response = await fetch(`/api/v1/skills/${encodeURIComponent(name)}${suffix}`, {
      method,
      headers: { "X-CSRF-Token": csrfToken, ...(body ? { "Content-Type": "application/json" } : {}) },
      ...(body ? { body: JSON.stringify(body) } : {}),
    });
    return { status: response.status, body: await response.json() as Record<string, unknown> };
  }, { name, method, suffix, body });
}

test("real generated Skills preserve versioned content and activation gates through reload and confirmed deletion", async ({ page, request }, testInfo) => {
  test.setTimeout(60_000);
  page.setDefaultTimeout(15_000);
  const transactions: Array<{ method: string; path: string; status?: number }> = [];
  page.on("request", request => {
    const path = new URL(request.url()).pathname;
    if (path.startsWith("/api/v1/skills")) transactions.push({ method: request.method(), path });
  });
  page.on("response", response => {
    const path = new URL(response.url()).pathname;
    if (path.startsWith("/api/v1/skills")) transactions.push({ method: response.request().method(), path, status: response.status() });
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.addInitScript(() => localStorage.setItem("i18nextLng", "en-US"));
  await page.goto("/assistant", { waitUntil: "domcontentloaded" });
  const name = `browser-verification-${Date.now()}`;
  const title = `Browser verification ${Date.now()}`;
  const instructions = `# Isolated browser verification\nRead the marker ${name} without external actions.\n`;
  const markdown = `---\nname: ${name}\ntitle: ${title}\ndescription: Disposable instruction skill for this isolated browser test.\nversion: 1.0.0\nenabled: false\n---\n${instructions}`;
  let deleted = false;
  let created = false;

  const openSkills = async () => {
    const customize = page.getByRole("button", { name: "Customize", exact: true });
    const showHistory = page.locator('button[aria-label="Show history"]');
    await expect(customize.or(showHistory).first()).toBeVisible();
    if (!(await customize.isVisible())) await showHistory.click();
    await customize.click();
    const dialog = page.getByRole("dialog", { name: "Customize AI Assistant" });
    await expect(dialog.getByRole("button", { name: "Upload SKILL.md" })).toBeVisible();
    return dialog;
  };

  try {
    let dialog = await openSkills();
    const upload = page.waitForResponse(response => new URL(response.url()).pathname === "/api/v1/skills/upload"
      && response.request().method() === "POST");
    await dialog.locator('input[type="file"]').setInputFiles({ name: "SKILL.md", mimeType: "text/markdown", buffer: Buffer.from(markdown) });
    const uploaded = await upload;
    expect(uploaded.status()).toBe(201);
    created = true;
    const record = await uploaded.json() as { skill_id: string; version_id: string; content_hash: string; entrypoint: string };
    expect(record.skill_id).toMatch(/^[0-9a-f-]{36}$/);
    expect(record.version_id).toMatch(/^[0-9a-f-]{36}$/);
    expect(record.entrypoint).toBe(`db://${record.skill_id}/${record.version_id}`);
    expect(record.content_hash).toBe(createHash("sha256").update(markdown).digest("hex"));

    const card = () => dialog.getByRole("group", { name: title, exact: true });
    await expect(card().getByRole("switch")).toHaveAccessibleName(`Enable skill ${title}`);
    await expect(card().getByRole("button")).toHaveAccessibleName(`Delete skill ${title}`);
    await expect(card().getByRole("switch")).not.toBeChecked();
    const assertActivationGated = async () => {
      const update = page.waitForResponse(response => new URL(response.url()).pathname === `/api/v1/skills/${name}/enable`
        && response.request().method() === "POST");
      await card().getByRole("switch").click();
      const denied = await update;
      expect(denied.status()).toBe(409);
      expect(JSON.stringify(await denied.json())).toContain("SKILL_ACTIVATION_GATES_REQUIRED");
      await expect(dialog.getByRole("alert")).toContainText("requires independent review, evaluation, and rollback evidence");
      await expect(card().getByRole("switch")).not.toBeChecked();
      const persisted = await skillRequest(page, name);
      expect(persisted.status).toBe(200);
      expect(persisted.body).toMatchObject({ enabled: false, version_id: record.version_id, content_hash: record.content_hash });
    };
    await assertActivationGated();

    // This endpoint reads the actual pinned instructions; it does not claim a model ran.
    const tested = await skillRequest(page, name, "POST", "/test", { input: name });
    expect(tested.status).toBe(200);
    expect(tested.body).toEqual({ success: true, type: "skill_instructions", result: instructions.trim() });
    await assertActivationGated(); // Reading instructions is not independent activation evidence.
    await page.reload({ waitUntil: "domcontentloaded" });
    dialog = await openSkills();
    await expect(card().getByRole("switch")).not.toBeChecked();
    page.once("dialog", async confirmation => {
      expect(confirmation.type()).toBe("confirm");
      expect(confirmation.message()).toBe(`Delete skill "${name}"?`);
      await confirmation.accept();
    });
    const removal = page.waitForResponse(response => new URL(response.url()).pathname === `/api/v1/skills/${name}`
      && response.request().method() === "DELETE");
    await card().getByRole("button").click();
    expect((await removal).status()).toBe(200);
    deleted = true;
    await expect(dialog.getByText(title, { exact: true })).toHaveCount(0);
    expect((await skillRequest(page, name)).status).toBe(404);
    await page.reload({ waitUntil: "domcontentloaded" });
    dialog = await openSkills();
    await expect(dialog.getByText(title, { exact: true })).toHaveCount(0);
  } finally {
    await testInfo.attach("skill-transactions", { body: JSON.stringify(transactions), contentType: "application/json" });
    if (created && !deleted) {
      const cleanup = await request.delete(`${getApiUrl()}/api/v1/skills/${encodeURIComponent(name)}`, { headers: await buildAuthHeaders(request) });
      expect([200, 404]).toContain(cleanup.status());
    }
  }
});
