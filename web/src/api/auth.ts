import { api } from "../lib/api";

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

export interface LoginRequest {
  email: string;
  password: string;
}

export interface LoginResponse {
  session_type: "cookie";
  csrf_token: string;
  expires_in: number;
  user: LoginUser;
  force_password_change: boolean;
}

export interface PasswordChangeRequest {
  current_password: string;
  new_password: string;
  confirm_password: string;
}

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
  const response = await api.post<LoginResponse>("/api/auth/session", data);
  if (response.data.session_type !== "cookie" || !response.data.csrf_token || !response.data.user?.user_id) throw new Error("Invalid Console session login");
  return response.data;
}

/**
 * User logout
 */
export async function logout(): Promise<void> {
  await api.delete("/api/auth/session");
}

/**
 * Change user password
 */
export async function changePassword(data: PasswordChangeRequest): Promise<void> {
  await api.post("/api/auth/session/password", data);
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
  const response = await api.get<ConsoleSessionResponse>("/api/auth/session");
  if (!response.data.csrf_token || !response.data.user?.user_id) throw new Error("Invalid Console session bootstrap");
  return response.data;
}
export async function getCurrentUser(): Promise<CurrentUserResponse> {
  return (await getConsoleSession()).user;
}
