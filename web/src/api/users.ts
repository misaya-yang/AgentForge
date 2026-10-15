import { api } from "../lib/api";

// ============================================================
// Types
// ============================================================

export interface UserResponse {
  user_id: string;
  email: string | null;
  display_name: string | null;
  username: string | null;
  department: string | null;  // 部门 - 由管理员分配
  roles: string[];
  extra_permissions: string[];  // 额外权限 - 直接分配，非角色
  service_access_mode: "all" | "allowlist";
  allowed_services: string[];
  denied_services: string[];
  status: string;
  tier: string;
  force_password_change: boolean;
  last_login_at: string | null;
  created_at: string | null;
}

export interface UserListResponse {
  users: UserResponse[];
  total: number;
  page: number;
  page_size: number;
}

export interface UserCreateRequest {
  email: string;
  display_name: string;
  department?: string;  // 部门 - 由管理员分配
  roles?: string[];
}

export interface UserUpdateRequest {
  display_name?: string;
  department?: string;  // 部门 - 由管理员分配
  roles?: string[];
  extra_permissions?: string[];  // 额外权限 - 直接分配，非角色
  service_access_mode?: "all" | "allowlist";
  allowed_services?: string[];
  denied_services?: string[];
  status?: string;
  tier?: string;
}

export interface RoleResponse {
  role_name: string;
  description: string | null;
  permissions: string[];
  is_system: boolean;
  user_count: number | null;
  created_at: string | null;
  updated_at: string | null;
}

export interface RoleListResponse {
  roles: RoleResponse[];
  total: number;
}

export interface PermissionResponse {
  permission_code: string;
  name: string;
  description: string | null;
  category: string;
  resource: string;
  action: string;
  is_system: boolean;
}

export interface PermissionListResponse {
  permissions: PermissionResponse[];
  total: number;
}

// ============================================================
// User API Functions
// ============================================================

/**
 * List users with pagination
 */
export async function listUsers(params?: {
  page?: number;
  page_size?: number;
  status?: string;
  search?: string;
}): Promise<UserListResponse> {
  const response = await api.get<UserListResponse>("/api/v1/users", { params });
  return response.data;
}

/**
 * Get user by ID
 */
export async function getUser(userId: string): Promise<UserResponse> {
  const response = await api.get<UserResponse>(`/api/v1/users/${encodeURIComponent(userId)}`);
  return response.data;
}

/**
 * Create a new user
 */
export async function createUser(data: UserCreateRequest): Promise<UserResponse> {
  const response = await api.post<UserResponse>("/api/v1/users", data);
  return response.data;
}

/**
 * Update user
 */
export async function updateUser(
  userId: string,
  data: UserUpdateRequest
): Promise<UserResponse> {
  const response = await api.put<UserResponse>(`/api/v1/users/${encodeURIComponent(userId)}`, data);
  return response.data;
}

/**
 * Delete user
 */
export async function deleteUser(userId: string): Promise<void> {
  await api.delete(`/api/v1/users/${encodeURIComponent(userId)}`);
}

/**
 * Reset user password to default
 */
export async function resetUserPassword(userId: string): Promise<void> {
  await api.post(`/api/v1/users/${encodeURIComponent(userId)}/reset-password`);
}

/**
 * Enable user
 */
export async function enableUser(userId: string): Promise<void> {
  await api.post(`/api/v1/users/${encodeURIComponent(userId)}/enable`);
}

/**
 * Disable user
 */
export async function disableUser(userId: string): Promise<void> {
  await api.post(`/api/v1/users/${encodeURIComponent(userId)}/disable`);
}

// ============================================================
// Role API Functions
// ============================================================

/**
 * List all roles
 */
export async function listRoles(): Promise<RoleListResponse> {
  const response = await api.get<RoleListResponse>("/api/v1/roles");
  return response.data;
}


/**
 * List all permissions
 */
export async function listPermissions(category?: string): Promise<PermissionListResponse> {
  const params = category ? { category } : undefined;
  const response = await api.get<PermissionListResponse>("/api/v1/roles/permissions", { params });
  return response.data;
}
