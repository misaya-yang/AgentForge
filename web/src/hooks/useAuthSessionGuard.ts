import { useEffect } from "react";
import { getCurrentUser } from "@/api/auth";
import { useAuthStore, type User } from "@/store/useAuthStore";

function normalizeCurrentUser(user: Awaited<ReturnType<typeof getCurrentUser>>): User {
  return {
    user_id: user.user_id,
    email: user.email,
    display_name: user.display_name,
    department: user.department,
    roles: Array.isArray(user.roles) ? user.roles : [],
    permissions:
      Array.isArray(user.effective_permissions) && user.effective_permissions.length > 0
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

    if (!token || !isAuthenticated) {
      if (sessionValidation !== "validated") {
        setSessionValidation("validated");
      }
      return;
    }

    if (sessionValidation !== "idle") {
      return;
    }

    setSessionValidation("checking");

    void getCurrentUser()
      .then((currentUser) => {
        const latestState = useAuthStore.getState();
        if (latestState.token !== token || !latestState.isAuthenticated) {
          return;
        }

        useAuthStore.setState({
          user: normalizeCurrentUser(currentUser),
          isAuthenticated: true,
          forcePasswordChange: Boolean(currentUser.force_password_change),
          sessionValidation: "validated",
        });
      })
      .catch(() => {
        const latestState = useAuthStore.getState();
        if (latestState.token !== token || !latestState.isAuthenticated) {
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
