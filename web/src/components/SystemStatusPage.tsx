import { lazy } from "react";
import { ArrowRight } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";

import { Button } from "@/components/ui/button";
import { useAuthStore } from "@/store/useAuthStore";

// Keep the shared return action eager; defer only status-specific presentation.
const SystemStatusContent = lazy(() => import("./SystemStatusContent"));

type SystemStatusKind = "forbidden" | "notFound";

interface SystemStatusPageProps {
  kind: SystemStatusKind;
}

export function SystemStatusPage({ kind }: SystemStatusPageProps) {
  const { t } = useTranslation();
  const isAuthenticated = useAuthStore((state) => state.isAuthenticated);
  const hasPermission = useAuthStore((state) => state.hasPermission);
  const destination = !isAuthenticated
    ? "/login"
    : hasPermission("console:dashboard:view")
      ? "/dashboard"
      : hasPermission("conversation:playground:access")
        ? "/playground"
        : "/login";

  return (
    <SystemStatusContent kind={kind}>
      <Button asChild className="mt-8 h-10 px-5">
        <Link to={destination}>
          {t("systemPages.returnToWorkspace")}
          <ArrowRight aria-hidden="true" className="ml-2 h-4 w-4" />
        </Link>
      </Button>
    </SystemStatusContent>
  );
}

export function ForbiddenPage() {
  return <SystemStatusPage kind="forbidden" />;
}

export function NotFoundPage() {
  return <SystemStatusPage kind="notFound" />;
}
