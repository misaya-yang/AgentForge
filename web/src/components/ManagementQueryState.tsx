import { AlertCircle, RefreshCw } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";

/** A failed catalog fetch must never masquerade as an empty catalog. */
export function ManagementQueryState({ onRetry, retrying, stale = false }: {
  onRetry: () => void;
  retrying: boolean;
  stale?: boolean;
}) {
  const { t } = useTranslation();
  return <div role="status" className="flex flex-wrap items-center gap-3 rounded-lg border border-destructive/25 bg-destructive/5 p-4">
    <AlertCircle className="h-4 w-4 shrink-0 text-destructive" aria-hidden="true" />
    <p className="min-w-0 flex-1 text-sm">{t(stale ? "management.staleError" : "management.loadError")}</p>
    <Button variant="outline" size="sm" onClick={onRetry} disabled={retrying}><RefreshCw className={`mr-2 h-3.5 w-3.5 ${retrying ? "animate-spin" : ""}`} aria-hidden="true" />{t("management.retry")}</Button>
  </div>;
}
