import { expect, test, type Page } from "./support/real-api";
import { buildAuthHeaders, getApiUrl } from "./support/helpers";

type Entry = { id: string; name: string; category: string; auth_kind: string };
type Connection = { connection_id: string; entry_id: string; owner_kind: string; owner_id: string; status: string; has_credential: boolean };

async function connections(page: Page): Promise<Connection[]> {
  return page.evaluate(async () => {
    if (location.protocol !== "https:") throw new Error("Live connections require trusted HTTPS BFF");
    const response = await fetch("/api/v1/connections", { credentials: "same-origin" });
    if (!response.ok) throw new Error(`Connections read failed (${response.status})`);
    return (await response.json() as { connections: Connection[] }).connections;
  });
}

test("real Connect apps persists a user connection and revokes only the created connection", async ({ page, request }) => {
  test.setTimeout(60_000);
  page.setDefaultTimeout(15_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.addInitScript(() => localStorage.setItem("i18nextLng", "en-US"));
  await page.goto("/workbench", { waitUntil: "domcontentloaded" });
  const initial = await connections(page);
  const authority = await page.evaluate(async () => {
    const [catalog, current] = await Promise.all([fetch("/api/v1/catalog"), fetch("/api/v1/auth/me")]);
    if (!catalog.ok || !current.ok) throw new Error("Live catalog or authenticated identity unavailable");
    return { entries: (await catalog.json() as { entries: Entry[] }).entries, user: await current.json() as { user_id: string } };
  });
  const selected = authority.entries.find(entry => entry.auth_kind === "none" && entry.category !== "local"
    && !initial.some(connection => connection.entry_id === entry.id && connection.status === "connected"));
  expect(selected, "The isolated fixture requires an unused credential-free builtin connection").toBeTruthy();
  const entry = selected!;
  let connectionId: string | undefined;
  let revoked = false;
  const openDialog = async () => {
    await page.getByRole("button", { name: "Connect apps", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "Connect apps", exact: true });
    await expect(dialog.getByRole("searchbox")).toBeFocused();
    await dialog.getByRole("searchbox").fill(entry.name);
    return dialog;
  };
  try {
    let dialog = await openDialog();
    const card = () => dialog.getByRole("article").filter({ has: page.getByText(entry.name, { exact: true }) });
    const creation = page.waitForResponse(response => new URL(response.url()).pathname === "/api/v1/connections"
      && response.request().method() === "POST");
    await card().getByRole("button", { name: `Connect ${entry.name}`, exact: true }).click();
    const response = await creation;
    expect(response.status()).toBe(201);
    expect(response.request().postDataJSON()).toEqual({ entry_id: entry.id, owner_kind: "user" });
    const created = await response.json() as Connection;
    connectionId = created.connection_id;
    expect(created).toMatchObject({ entry_id: entry.id, owner_kind: "user", owner_id: authority.user.user_id, status: "connected", has_credential: false });
    expect(created).not.toHaveProperty("credential_ciphertext");
    expect(created).not.toHaveProperty("secret");
    expect(initial.some(row => row.connection_id === connectionId)).toBe(false);
    await expect(card().getByRole("button", { name: "Connected", exact: true })).toBeDisabled();
    expect((await connections(page)).find(row => row.connection_id === connectionId)).toEqual(created);
    await page.reload({ waitUntil: "domcontentloaded" });
    dialog = await openDialog();
    await expect(card().getByRole("button", { name: "Connected", exact: true })).toBeDisabled();
    await page.goto("/settings/connectors", { waitUntil: "domcontentloaded" });
    const main = page.locator("main").last();
    await expect(main.getByRole("heading", { name: "Connectors", exact: true })).toBeVisible();
    await main.getByRole("searchbox").fill(entry.name);
    const managed = main.getByRole("article").filter({ has: page.getByText(entry.name, { exact: true }) });
    await expect(managed.getByRole("button", { name: "Connected", exact: true })).toBeDisabled();
    const status = await page.evaluate(async id => {
      const bootstrap = await fetch("/api/auth/session", { cache: "no-store" });
      const { csrf_token: csrfToken } = await bootstrap.json() as { csrf_token?: string };
      if (!bootstrap.ok || !csrfToken) throw new Error("BFF CSRF bootstrap required");
      return (await fetch(`/api/v1/connections/${encodeURIComponent(id)}/revoke`, {
        method: "POST", headers: { "X-CSRF-Token": csrfToken },
      })).status;
    }, connectionId);
    expect(status).toBe(200);
    revoked = true;
    const final = await connections(page);
    expect(final.find(row => row.connection_id === connectionId)).toMatchObject({ status: "revoked", has_credential: false });
    for (const original of initial) expect(final.find(row => row.connection_id === original.connection_id)).toEqual(original);
    await page.reload({ waitUntil: "domcontentloaded" });
    await main.getByRole("searchbox").fill(entry.name);
    await expect(managed.getByRole("button", { name: `Connect ${entry.name}`, exact: true })).toBeEnabled();
  } finally {
    if (connectionId && !revoked) {
      const cleanup = await request.post(`${getApiUrl()}/api/v1/connections/${encodeURIComponent(connectionId)}/revoke`, { headers: await buildAuthHeaders(request) });
      expect(cleanup.status()).toBe(200);
    }
  }
});
