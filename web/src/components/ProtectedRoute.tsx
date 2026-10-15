import { Navigate, useLocation } from "react-router-dom";
import { useAuthStore } from "@/store/useAuthStore";

export { ForbiddenPage } from "@/components/SystemStatusPage";

interface ProtectedRouteProps {
  children: React.ReactNode;
  requiredPermission?: string;
  requiredPermissions?: string[];
  requireAll?: boolean; // if true, require all permissions; if false (default), require any
  blockOnlyRole?: string;
  blockRedirectTo?: string;
}

export function ProtectedRoute({
  children,
  requiredPermission,
  requiredPermissions = [],
  requireAll = false,
  blockOnlyRole,
  blockRedirectTo = "/403",
}: ProtectedRouteProps) {
  const location = useLocation();
  const hydrated = useAuthStore((state) => state.hydrated);
  const isAuthenticated = useAuthStore((state) => state.isAuthenticated);
  const sessionValidation = useAuthStore((state) => state.sessionValidation);
  const hasAnyPermission = useAuthStore((state) => state.hasAnyPermission);
  const hasAllPermissions = useAuthStore((state) => state.hasAllPermissions);
  const user = useAuthStore((state) => state.user);

  // Show loading while waiting for hydration
  if (!hydrated || sessionValidation === "checking") {
    return (
      <div className="min-h-screen flex items-center justify-center bg-background">
        <div className="text-muted-foreground text-sm">Loading...</div>
      </div>
    );
  }

  // Check if user is authenticated
  if (!isAuthenticated) {
    return <Navigate to="/login" state={{ from: location }} replace />;
  }

  const roles = user?.roles || [];
  if (blockOnlyRole && roles.length === 1 && roles[0] === blockOnlyRole) {
    return <Navigate to={blockRedirectTo} replace />;
  }

  // Check permissions
  const allRequiredPermissions = requiredPermission
    ? [requiredPermission, ...requiredPermissions]
    : requiredPermissions;

  if (allRequiredPermissions.length > 0) {
    const hasAccess = requireAll
      ? hasAllPermissions(allRequiredPermissions)
      : hasAnyPermission(allRequiredPermissions);

    if (!hasAccess) {
      return <Navigate to="/403" replace />;
    }
  }

  return <>{children}</>;
}
