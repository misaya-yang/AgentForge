import { useEffect } from "react";
import { getConsoleSession, type CurrentUserResponse } from "@/api/auth";
import { useAuthStore, consoleAuthGeneration, type User } from "@/store/useAuthStore";

function normalizeCurrentUser(user: CurrentUserResponse): User {
  return {
    user_id: user.user_id,
    tenant_id: user.tenant_id,
    email: user.email,
    display_name: user.display_name,
    department: user.department,
    roles: Array.isArray(user.roles) ? user.roles : [],
    permissions:
      Array.isArray(user.effective_permissions)
        ? user.effective_permissions
        : Array.isArray(user.permissions)
          ? user.permissions
          : [],
    tier: user.tier,
  };
}

export function useAuthSessionGuard() {
  const hydrated = useAuthStore((state) => state.hydrated);
  const token = useAuthStore((state) => state.token);
  const isAuthenticated = useAuthStore((state) => state.isAuthenticated);
  const sessionValidation = useAuthStore((state) => state.sessionValidation);
  const clearAuth = useAuthStore((state) => state.clearAuth);
  const setSessionValidation = useAuthStore((state) => state.setSessionValidation);

  useEffect(() => {
    if (!hydrated) return;

    if (sessionValidation !== "idle" || useAuthStore.getState().sessionValidation !== "idle") {
      return;
    }

    const generation = consoleAuthGeneration();
    setSessionValidation("checking");

    void getConsoleSession()
      .then(({user: currentUser, csrf_token}) => {
        const latestState = useAuthStore.getState();
        if (consoleAuthGeneration() !== generation || latestState.token !== token || latestState.isAuthenticated !== isAuthenticated) {
          return;
        }

        latestState.setAuth(csrf_token, normalizeCurrentUser(currentUser), Boolean(currentUser.force_password_change), latestState.rememberMe);
      })
      .catch(() => {
        const latestState = useAuthStore.getState();
        if (consoleAuthGeneration() !== generation || latestState.token !== token || latestState.isAuthenticated !== isAuthenticated) {
          return;
        }
        clearAuth();
      });
  }, [
    clearAuth,
    hydrated,
    isAuthenticated,
    sessionValidation,
    setSessionValidation,
    token,
  ]);
}
