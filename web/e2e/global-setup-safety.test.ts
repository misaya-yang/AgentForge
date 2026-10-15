import assert from "node:assert/strict";
import test from "node:test";
import { prepareE2EAccount, shouldProvisionE2EAccounts } from "./global.setup.ts";

const API = "http://127.0.0.1:9999";
const credentials = { email: "existing@example.com", password: "fixture-only-password" };

function mockAccountApi(options: { loginStatus?: number; forcedPasswordChange?: boolean; existingModelTester?: boolean; allowFakeWrites?: boolean } = {}) {
  const requests: Array<{ method: string; path: string }> = [];
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async (input, init) => {
    const url = new URL(String(input));
    const method = init?.method || "GET";
    requests.push({ method, path: url.pathname });
    if (url.pathname === "/api/v1/auth/login") {
      return Response.json(options.loginStatus === 401 ? { detail: "rejected" } : {
        access_token: "fixture-token", force_password_change: options.forcedPasswordChange === true,
      }, { status: options.loginStatus || 200 });
    }
    if (url.pathname === "/api/v1/auth/me") {
      return Response.json({ user_id: "existing-user", roles: ["admin"] });
    }
    if (url.pathname.startsWith("/api/v1/users/model_tester_")) {
      if (options.allowFakeWrites && method === "POST") return Response.json({});
      return new Response(null, { status: options.existingModelTester ? 200 : 404 });
    }
    if (options.allowFakeWrites && method !== "GET" && (
      url.pathname === "/api/v1/users" ||
      url.pathname === "/api/v1/auth/change-password"
    )) return Response.json({});
    throw new Error(`Unexpected controlled endpoint ${method} ${url.pathname}`);
  };
  return { requests, restore: () => { globalThis.fetch = originalFetch; } };
}

function accountWrites(requests: Array<{ method: string; path: string }>) {
  return requests.filter(({ method, path }) =>
    (path.startsWith("/api/v1/users") && method !== "GET") ||
    path === "/api/v1/auth/change-password",
  );
}

test("omitted flags and the legacy guard always select read-only existing-account setup", async () => {
  assert.equal(shouldProvisionE2EAccounts({}), false);
  assert.equal(shouldProvisionE2EAccounts({ E2E_PROVISION_ACCOUNTS: "0" }), false);
  assert.equal(shouldProvisionE2EAccounts({ E2E_PROVISION_ACCOUNTS: "1" }), true);
  assert.equal(shouldProvisionE2EAccounts({ E2E_PROVISION_ACCOUNTS: "1", E2E_EXISTING_ACCOUNT_ONLY: "1" }), false);
  const mock = mockAccountApi();
  try {
    const account = await prepareE2EAccount(API, {
      provisionAccounts: shouldProvisionE2EAccounts({}), persistedCredentials: credentials,
    });
    assert.equal(account.currentUser.user_id, "existing-user");
    assert.deepEqual(mock.requests.map(({ path }) => path), ["/api/v1/auth/login", "/api/v1/auth/me"]);
    assert.deepEqual(accountWrites(mock.requests), []);
  } finally { mock.restore(); }
});

test("missing, rejected, and password-change-required existing accounts stop without management writes", async () => {
  for (const scenario of ["missing", "rejected", "forced"] as const) {
    const mock = mockAccountApi({ loginStatus: scenario === "rejected" ? 401 : undefined, forcedPasswordChange: scenario === "forced" });
    try {
      await assert.rejects(
        prepareE2EAccount(API, { provisionAccounts: false, persistedCredentials: scenario === "missing" ? null : credentials }),
        /requires configured credentials|login failed|cannot change credentials/,
      );
      assert.deepEqual(accountWrites(mock.requests), []);
    } finally { mock.restore(); }
  }
});

test("explicit initialization refuses persisted accounts and preexisting model testers before writes", async () => {
  const mock = mockAccountApi({ existingModelTester: true });
  const previousPassword = process.env.DEFAULT_USER_PASSWORD;
  process.env.DEFAULT_USER_PASSWORD = "fixture-only-bootstrap";
  try {
    await assert.rejects(
      prepareE2EAccount(API, { provisionAccounts: true, persistedCredentials: credentials }),
      /fresh isolated environment/,
    );
    assert.deepEqual(mock.requests, []);
    await assert.rejects(
      prepareE2EAccount(API, { provisionAccounts: true, persistedCredentials: null }),
      /model-tester accounts already exist/,
    );
    assert.deepEqual(accountWrites(mock.requests), []);
  } finally {
    mock.restore();
    if (previousPassword === undefined) delete process.env.DEFAULT_USER_PASSWORD;
    else process.env.DEFAULT_USER_PASSWORD = previousPassword;
  }
});

test("explicit fresh initialization remains available only against a fake account API", async () => {
  const mock = mockAccountApi({ allowFakeWrites: true });
  const previousPassword = process.env.DEFAULT_USER_PASSWORD;
  process.env.DEFAULT_USER_PASSWORD = "fixture-only-bootstrap";
  try {
    const account = await prepareE2EAccount(API, { provisionAccounts: true, persistedCredentials: null });
    assert.equal(account.currentUser.user_id, "existing-user");
    const writes = accountWrites(mock.requests);
    assert.equal(writes.filter(r => r.path === "/api/v1/users").length, 6);
    assert.equal(writes.filter(r => r.path.endsWith("/reset-password")).length, 5);
    assert.equal(writes.filter(r => r.path === "/api/v1/auth/change-password").length, 5);
  } finally {
    mock.restore();
    if (previousPassword === undefined) delete process.env.DEFAULT_USER_PASSWORD;
    else process.env.DEFAULT_USER_PASSWORD = previousPassword;
  }
});
