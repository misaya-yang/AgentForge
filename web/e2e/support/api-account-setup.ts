/** Explicit API/SDK bearer fixture. Never seeds a Console browser session. */
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
function repoRoot(): string { return path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../.."); }
function modelTesterPassword(): string { return process.env.E2E_MODEL_TESTER_PASSWORD || "ModelTester-ChangeMe-2026!"; }
function passwordFromEnvLine(line: string): string | null {
  const trimmed = line.trim();
  if (!trimmed || trimmed.startsWith("#") || !trimmed.startsWith("DEFAULT_USER_PASSWORD=")) {
    return null;
  }
  let value = trimmed.slice("DEFAULT_USER_PASSWORD=".length);
  if (
    (value.startsWith('"') && value.endsWith('"')) ||
    (value.startsWith("'") && value.endsWith("'"))
  ) {
    value = value.slice(1, -1);
  }
  return value || null;
}

async function detectDefaultPassword(): Promise<string> {
  const fromEnv = process.env.DEFAULT_USER_PASSWORD || process.env.E2E_BOOTSTRAP_PASSWORD;
  if (fromEnv) {
    return fromEnv;
  }

  try {
    const envText = await fs.readFile(path.join(repoRoot(), ".env"), "utf-8");
    for (const line of envText.split("\n")) {
      const value = passwordFromEnvLine(line);
      if (value) {
        return value;
      }
    }
  } catch {
    // .env is optional when the password is already in the process environment.
  }

  throw new Error(
    "DEFAULT_USER_PASSWORD is not set. Export it or add it to .env for Playwright setup."
  );
}

async function detectBootstrapPasswords(defaultPassword: string): Promise<string[]> {
  const candidates = new Set<string>();

  if (process.env.E2E_BOOTSTRAP_PASSWORD) {
    candidates.add(process.env.E2E_BOOTSTRAP_PASSWORD);
  }

  candidates.add(defaultPassword);

  const migrationPath = path.join(
    repoRoot(),
    "database/migrations/005_account_permission_system.sql"
  );
  const migration = await fs.readFile(migrationPath, "utf-8").catch(() => "");
  const inlineCommentMatch = migration.match(/Password:\s*([^\s]+)/);
  const hashCommentMatch = migration.match(/bcrypt hash of '([^']+)'/);

  if (inlineCommentMatch?.[1]) {
    candidates.add(inlineCommentMatch[1]);
  }
  if (hashCommentMatch?.[1]) {
    candidates.add(hashCommentMatch[1]);
  }

  return Array.from(candidates);
}

function buildNextPassword(): string {
  return `Agw!${Date.now().toString(36)}Z9`;
}

async function login(apiURL: string, email: string, password: string) {
  const response = await fetch(`${apiURL}/api/v1/auth/login`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ email, password }),
  });

  const payload = (await response.json().catch(() => ({}))) as Record<string, unknown>;
  if (!response.ok) {
    throw new Error(`E2E login failed (${response.status}). Check the existing test account credentials.`);
  }
  return payload;
}

async function loginWithCandidates(
  apiURL: string,
  email: string,
  passwords: string[]
): Promise<{ password: string; payload: Record<string, unknown> }> {
  let lastError: Error | null = null;

  for (const password of passwords) {
    try {
      const payload = await login(apiURL, email, password);
      return { password, payload };
    } catch (error) {
      lastError = error as Error;
    }
  }

  throw lastError || new Error(`Unable to authenticate bootstrap user: ${email}`);
}

async function createE2EAdminUser(
  apiURL: string,
  bootstrapToken: string,
  email: string
) {
  const response = await fetch(`${apiURL}/api/v1/users`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      Authorization: `Bearer ${bootstrapToken}`,
    },
    body: JSON.stringify({
      email,
      display_name: "Playwright E2E",
      roles: ["admin"],
    }),
  });

  if (!response.ok) {
    const body = await response.text();
    throw new Error(`Failed to provision E2E admin user (${response.status}): ${body}`);
  }
}

async function resetE2EUserPassword(
  apiURL: string,
  adminToken: string,
  userId: string
) {
  const response = await fetch(`${apiURL}/api/v1/users/${userId}/reset-password`, {
    method: "POST",
    headers: { Authorization: `Bearer ${adminToken}` },
  });

  if (!response.ok) {
    const text = await response.text();
    throw new Error(`Failed to reset E2E user ${userId} (${response.status}): ${text}`);
  }
}

async function assertFreshModelTesterAccounts(apiURL: string, adminToken: string) {
  for (let index = 1; index <= 5; index += 1) {
    const response = await fetch(`${apiURL}/api/v1/users/model_tester_${index}`, {
      headers: { Authorization: `Bearer ${adminToken}` },
    });
    if (response.status === 200) {
      throw new Error("Account provisioning requires a fresh isolated environment; model-tester accounts already exist.");
    }
    if (response.status !== 404) {
      throw new Error(`Could not verify fresh model-tester accounts (${response.status}).`);
    }
  }
}

async function createModelTesterUsers(
  apiURL: string,
  adminToken: string,
  authEmailDomain: string,
  defaultPassword: string
) {
  for (let index = 1; index <= 5; index += 1) {
    const userId = `model_tester_${index}`;
    const email = `${userId}@${authEmailDomain}`;
    const displayName = `Model Tester ${index}`;

    const createResponse = await fetch(`${apiURL}/api/v1/users`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        Authorization: `Bearer ${adminToken}`,
      },
      body: JSON.stringify({
        email,
        display_name: displayName,
        roles: ["model_tester"],
      }),
    });

    if (!createResponse.ok) {
      throw new Error(`Failed to provision a new model-tester account (${createResponse.status}).`);
    }
    await resetE2EUserPassword(apiURL, adminToken, userId);

    let loginPayload = await login(apiURL, email, defaultPassword);
    if (defaultPassword === modelTesterPassword() && loginPayload.force_password_change === true) {
      const temporaryPassword = `${modelTesterPassword()}-Tmp1!`;
      await changePassword(
        apiURL,
        String(loginPayload.access_token || ""),
        defaultPassword,
        temporaryPassword
      );
      loginPayload = await login(apiURL, email, temporaryPassword);
      await changePassword(
        apiURL,
        String(loginPayload.access_token || ""),
        temporaryPassword,
        modelTesterPassword()
      );
    } else if (
      defaultPassword !== modelTesterPassword() ||
      loginPayload.force_password_change === true
    ) {
      await changePassword(
        apiURL,
        String(loginPayload.access_token || ""),
        defaultPassword,
        modelTesterPassword()
      );
    }
  }
}

async function changePassword(
  apiURL: string,
  token: string,
  currentPassword: string,
  nextPassword: string
) {
  const response = await fetch(`${apiURL}/api/v1/auth/change-password`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      Authorization: `Bearer ${token}`,
    },
    body: JSON.stringify({
      current_password: currentPassword,
      new_password: nextPassword,
      confirm_password: nextPassword,
    }),
  });

  if (!response.ok) {
    const body = await response.text();
    throw new Error(`E2E password change failed (${response.status}): ${body}`);
  }
}

async function validateToken(apiURL: string, token: string) {
  const response = await fetch(`${apiURL}/api/v1/auth/me`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!response.ok) {
    const body = await response.text();
    throw new Error(`Token validation failed (${response.status}): ${body}`);
  }
  return response.json();
}

type E2ECredentials = { email: string; password: string };

/** Omission is safe: only an explicit fresh-environment flag may provision accounts. */
export function shouldProvisionE2EAccounts(env: Record<string, string | undefined> = process.env): boolean {
  return env.E2E_EXISTING_ACCOUNT_ONLY !== "1" && env.E2E_PROVISION_ACCOUNTS === "1";
}

/** Auth setup is separate from browser storage so the no-write default can be tested. */
export async function prepareE2EAccount(
  apiURL: string,
  options: {
    provisionAccounts: boolean;
    providedEmail?: string;
    providedPassword?: string;
    persistedCredentials: E2ECredentials | null;
  },
) {
  const { provisionAccounts, providedEmail, providedPassword, persistedCredentials } = options;
  if ((providedEmail && !providedPassword) || (!providedEmail && providedPassword)) {
    throw new Error("E2E_USER_EMAIL and E2E_USER_PASSWORD must be provided together.");
  }
  if (provisionAccounts && (providedEmail || persistedCredentials)) {
    throw new Error("Account provisioning requires a fresh isolated environment without existing E2E credentials.");
  }
  if (!provisionAccounts && !persistedCredentials && !(providedEmail && providedPassword)) {
    throw new Error("Existing-account E2E requires configured credentials; account creation is disabled by default.");
  }

  const defaultPassword = provisionAccounts ? await detectDefaultPassword() : "";
  const authEmailDomain = process.env.E2E_AUTH_EMAIL_DOMAIN || "example.com";
  const email = providedEmail || persistedCredentials?.email || `assistant.e2e.${Date.now()}@${authEmailDomain}`;
  let password = providedPassword || persistedCredentials?.password || defaultPassword;
  let loginPayload: Record<string, unknown>;
  let provisioningToken: string | null = null;

  if (!provisionAccounts) {
    loginPayload = await login(apiURL, email, password);
  } else {
    const bootstrapEmail = process.env.E2E_BOOTSTRAP_EMAIL || `admin@${authEmailDomain}`;
    const bootstrapPasswords = await detectBootstrapPasswords(defaultPassword);
    const bootstrapLogin = await loginWithCandidates(apiURL, bootstrapEmail, bootstrapPasswords);
    provisioningToken = String(bootstrapLogin.payload.access_token || "");
    await assertFreshModelTesterAccounts(apiURL, provisioningToken);
    await createE2EAdminUser(apiURL, provisioningToken, email);
    loginPayload = await login(apiURL, email, defaultPassword);
    password = defaultPassword;
  }

  if (loginPayload.force_password_change === true) {
    if (!provisionAccounts) throw new Error("Existing-account E2E cannot change credentials. Complete account setup outside this test run.");
    const nextPassword = buildNextPassword();
    await changePassword(apiURL, String(loginPayload.access_token), password, nextPassword);
    password = nextPassword;
    loginPayload = await login(apiURL, email, password);
  }

  const token = String(loginPayload.access_token || "");
  if (!token) throw new Error("Login succeeded but access_token is missing.");
  const currentUser = await validateToken(apiURL, token);
  if (provisionAccounts) await createModelTesterUsers(apiURL, provisioningToken || token, authEmailDomain, defaultPassword);
  return { email, password, token, currentUser, loginPayload };
}
