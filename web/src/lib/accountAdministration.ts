import type { User } from "@/store/useAuthStore";
import type { UserResponse } from "@/api/users";

/** Presentation only. The server rechecks current authority for every write. */
export function canAdministerAccounts(user: User | null): boolean {
  return Boolean(user?.roles.some(role => role === "admin" || role === "platform_admin"));
}

export function isProtectedAccount(target: UserResponse, actorId?: string): boolean {
  return target.user_id === actorId || target.user_id === "admin" || target.roles.includes("platform_admin");
}
