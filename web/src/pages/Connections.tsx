import { useNavigate, useSearchParams } from "react-router-dom";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import ConnectorsPanel from "@/pages/assistant/components/ConnectorsPanel";

/** Personal OAuth return surface. Only authenticated reads establish connection state. */
export function ConnectionsPage() {
  const navigate = useNavigate();
  const [query] = useSearchParams();
  const scope = useAuthenticatedReadScope();
  return scope.isCurrent() ? <ConnectorsPanel key={scope.id} open isCurrent={scope.isCurrent}
    authorizationReturnFailed={query.get("status") === "authorization_failed"}
    onClose={() => navigate("/", { replace: true })} /> : null;
}
