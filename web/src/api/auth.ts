import type { ApiRequest } from "../../../sdk/typescript/generated";
import { api } from "../lib/api";
import { request } from "./generatedTransport";
import { installConsoleStepUp, stepUpClient, currentStepUpScope } from "../lib/installConsoleStepUp";
import { authorizeConsoleMutation, validateStepUpStatus } from "../lib/consoleStepUp";

// Auth bootstrap is imported before console routes. Install once for every
// sensitive caller, including direct settings API requests.
installConsoleStepUp(api);

export async function getPasskeyStepUpStatus() {
  return validateStepUpStatus(await stepUpClient(api).status());
}
export async function verifyConsolePasskey(): Promise<void> {
  await authorizeConsoleMutation(stepUpClient(api), currentStepUpScope(), undefined, true);
}

// ============================================================
// Types
// ============================================================

export interface LoginUser {
  user_id: string;
  email: string | null;
  display_name: string | null;
  department: string | null;
  roles: string[];
  permissions: string[];
  tier: string;
}

export type LoginRequest = ApiRequest<"POST /api/auth/session">["body"];

export interface LoginResponse {
  session_type: "cookie";
  csrf_token: string;
  expires_in: number;
  user: LoginUser;
  force_password_change: boolean;
}

export type PasswordChangeRequest = ApiRequest<"POST /api/auth/session/password">["body"];

export interface CurrentUserResponse {
  user_id: string;
  tenant_id: string;
  email: string | null;
  display_name: string | null;
  department: string | null;
  roles: string[];
  permissions: string[];
  effective_permissions: string[];
  tier: string;
  force_password_change: boolean;
}

// ============================================================
// API Functions
// ============================================================

/**
 * User login with email and password
 */
export async function login(data: LoginRequest): Promise<LoginResponse> {
  const response = await request("POST /api/auth/session", { body: data });
  if (response.session_type !== "cookie" || !response.csrf_token || !response.user?.user_id) throw new Error("Invalid Console session login");
  return response;
}

/**
 * User logout
 */
export async function logout(): Promise<void> {
  await request("DELETE /api/auth/session", {});
}

/**
 * Change user password
 */
export async function changePassword(data: PasswordChangeRequest): Promise<void> {
  await request("POST /api/auth/session/password", { body: data });
}

/**
 * Get current user information
 */
export interface ConsoleSessionResponse {
  user: CurrentUserResponse;
  csrf_token: string;
  expires_at: number;
}
export async function getConsoleSession(): Promise<ConsoleSessionResponse> {
  const response = await request("GET /api/auth/session", {});
  if (!response.csrf_token || !response.user?.user_id) throw new Error("Invalid Console session bootstrap");
  return response;
}
export async function getCurrentUser(): Promise<CurrentUserResponse> {
  return (await getConsoleSession()).user;
}
