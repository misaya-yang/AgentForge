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
  access_token: string;
  token_type: string;
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
  const response = await api.post<LoginResponse>("/api/v1/auth/login", data);
  return response.data;
}

/**
 * User logout
 */
export async function logout(): Promise<void> {
  await api.post("/api/v1/auth/logout");
}

/**
 * Change user password
 */
export async function changePassword(data: PasswordChangeRequest): Promise<void> {
  await api.post("/api/v1/auth/change-password", data);
}

/**
 * Get current user information
 */
export async function getCurrentUser(): Promise<CurrentUserResponse> {
  const response = await api.get<CurrentUserResponse>("/api/v1/auth/me");
  return response.data;
}
