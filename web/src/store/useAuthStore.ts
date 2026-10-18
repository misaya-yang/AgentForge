import { create } from "zustand";
import { CONSOLE_SESSION_PREFIX, createConsoleFetch } from "@/lib/consoleSessionTransport";
import { clearUploadRecoverySession } from "@/lib/knowledgeUploadRecovery";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";

export interface User {
  user_id: string;
  tenant_id?: string;
  email: string | null;
  display_name: string | null;
  department: string | null;  // 部门 - 由管理员分配
  roles: string[];
  permissions: string[];
  tier: string;
}

export type SessionValidationState = "idle" | "checking" | "validated";

export interface AuthState {
  // State
  /** Non-secret, memory-only session generation; never a bearer token. */
  token: string | null;
  csrfToken: string | null;
  user: User | null;
  isAuthenticated: boolean;
  forcePasswordChange: boolean;
  isLoading: boolean;
  rememberMe: boolean;  // UI preference; cookie lifetime is owned by the server
  hydrated: boolean;
  sessionValidation: SessionValidationState;

  // Actions
  setAuth: (csrfToken: string, user: User, forcePasswordChange: boolean, rememberMe?: boolean) => void;
  clearAuth: () => void;
  setLoading: (loading: boolean) => void;
  updateUser: (user: Partial<User>) => void;
  setForcePasswordChange: (force: boolean) => void;
  setHydrated: (hydrated: boolean) => void;
  setSessionValidation: (state: SessionValidationState) => void;

  // Permission helpers
  hasPermission: (permission: string) => boolean;
  hasRole: (role: string) => boolean;
  hasAnyPermission: (permissions: string[]) => boolean;
  hasAllPermissions: (permissions: string[]) => boolean;
}

const STORAGE_KEY = "agent-gateway-auth";

function normalizeUser(user: User | Partial<User> | null | undefined): User | null {
  if (!user) return null;
  return {
    user_id: String(user.user_id || ""),
    ...(typeof user.tenant_id === "string" ? { tenant_id: user.tenant_id } : {}),
    email: user.email ?? null,
    display_name: user.display_name ?? null,
    department: user.department ?? null,
    roles: Array.isArray(user.roles) ? user.roles : [],
    permissions: Array.isArray(user.permissions) ? user.permissions : [],
    tier: typeof user.tier === "string" && user.tier ? user.tier : "normal",
  };
}

// Retire legacy persisted bearer credentials; never hydrate them into memory.
export function clearLegacyAuthStorage(): void {
  for (const storage of [() => localStorage, () => sessionStorage]) {
    try {
      storage().removeItem(STORAGE_KEY);
    } catch {
      // Cookie sessions work without Web Storage.
    }
  }
}

function clearSessionPreferences(): void {
  try {
    const staleKeys: string[] = [];
    for (let i = 0; i < localStorage.length; i += 1) {
      const key = localStorage.key(i);
      if (key && (
        key === "assistant.lastModelId.v1"
        || key.startsWith("assistant.lastModelId.v1:")
        || key === "setup-banner-dismissed"
        || key.startsWith("setup-banner-dismissed:")
      )) {
        staleKeys.push(key);
      }
    }
    staleKeys.forEach((key) => localStorage.removeItem(key));
  } catch {
    // Storage unavailable — auth is still cleared by the caller.
  }
}

clearLegacyAuthStorage();
let sessionSerial = 0;
let authGeneration = 0;

export function consoleAuthGeneration(): number {
  return authGeneration;
}

export const useAuthStore = create<AuthState>()((set, get) => ({
  // Initial state
  token: null,
  csrfToken: null,
  user: null,
  isAuthenticated: false,
  forcePasswordChange: false,
  isLoading: false,
  rememberMe: false,
  hydrated: true,
  sessionValidation: "idle",

  // Set authentication data after successful login
  setAuth: (csrfToken, user, forcePasswordChange, rememberMe = false) => {
    if (!csrfToken || !user?.user_id) {
      throw new Error("Invalid Console session bootstrap");
    }
    set({
      token: `${CONSOLE_SESSION_PREFIX}${++sessionSerial}`,
      csrfToken,
      user: normalizeUser(user),
      isAuthenticated: true,
      forcePasswordChange,
      isLoading: false,
      rememberMe,
      sessionValidation: "validated",
    });
  },

  // Clear authentication data on logout
  clearAuth: () => {
    authGeneration++; // Also retire pending bootstrap when already anonymous.
    clearLegacyAuthStorage();
    clearSessionPreferences();
    set({
      token: null,
      csrfToken: null,
      user: null,
      isAuthenticated: false,
      forcePasswordChange: false,
      isLoading: false,
      rememberMe: false,
      sessionValidation: "validated",
    });
  },

  // Set loading state
  setLoading: (loading) => set({ isLoading: loading }),

  // Update user data
  updateUser: (userData) =>
    set((state) => ({
      user: normalizeUser(state.user ? { ...state.user, ...userData } : null),
    })),

  // Update force password change flag
  setForcePasswordChange: (force) => set({ forcePasswordChange: force }),
  setHydrated: (hydrated) => set({ hydrated }),
  setSessionValidation: (sessionValidation) => set({ sessionValidation }),

  // Check if user has a specific permission
  hasPermission: (permission: string) => {
    const { user } = get();
    if (!user) return false;
    const permissions = Array.isArray(user.permissions) ? user.permissions : [];

    // Admin wildcard permission
    if (permissions.includes("admin:*")) return true;

    // Direct permission check
    if (permissions.includes(permission)) return true;

    // Wildcard permission matching (e.g., "console:*" matches "console:dashboard:view")
    const parts = permission.split(":");
    for (let i = parts.length - 1; i > 0; i--) {
      const wildcardPerm = parts.slice(0, i).join(":") + ":*";
      if (permissions.includes(wildcardPerm)) return true;
    }

    return false;
  },

  // Check if user has a specific role
  hasRole: (role: string) => {
    const { user } = get();
    if (!user) return false;
    const roles = Array.isArray(user.roles) ? user.roles : [];
    return roles.includes(role);
  },

  // Check if user has any of the specified permissions
  hasAnyPermission: (permissions: string[]) => {
    const { hasPermission } = get();
    return permissions.some((p) => hasPermission(p));
  },

  // Check if user has all specified permissions
  hasAllPermissions: (permissions: string[]) => {
    const { hasPermission } = get();
    return permissions.every((p) => hasPermission(p));
  },
}));

// Helper hook for common permission checks
export const usePermission = (permission: string) => {
  return useAuthStore((state) => state.hasPermission(permission));
};

// Helper hook for role checks
export const useRole = (role: string) => {
  return useAuthStore((state) => state.hasRole(role));
};

// Helper hook for checking if user is admin
export const useIsAdmin = () => {
  return useAuthStore((state) => state.hasRole("admin"));
};

// Upload recovery metadata belongs to one login/authority session. This observer
// is global so leaving the Knowledge route cannot bypass logout cleanup.
useAuthStore.subscribe((state, previous) => {
  const sessionChanged = state.token !== previous.token
    || state.csrfToken !== previous.csrfToken
    || state.isAuthenticated !== previous.isAuthenticated
    || sdkAuthorityScope(state.user) !== sdkAuthorityScope(previous.user);
  if (!sessionChanged) return;

  authGeneration++;
  clearUploadRecoverySession();
});

// Legacy raw-fetch callers keep the non-secret marker as their UI ownership key.
// The shared transport removes it from headers and supplies cookie + CSRF instead.
if (typeof window !== "undefined" && typeof window.fetch === "function") {
  const originalFetch = window.fetch;
  const wrapped = createConsoleFetch(
    originalFetch.bind(window),
    window.location.origin,
    () => ({ ...useAuthStore.getState(), generation: authGeneration }),
    () => {
      useAuthStore.getState().clearAuth();
      if (!window.location.pathname.includes("/login")) {
        window.location.href = "/login";
      }
    },
  );
  window.fetch = wrapped;
  if (import.meta.hot) {
    import.meta.hot.dispose(() => {
      if (window.fetch === wrapped) window.fetch = originalFetch;
    });
  }
}
