import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";
import { Button } from "@/components/ui/button";

/** Catalog availability is a read observation, not a provider health or access guarantee. */
export function PlaygroundServiceStatus({ state, canManageServices, refreshing, onRefresh }: {
  state: "loading" | "error" | "empty" | "unavailable" | "unselected";
  canManageServices: boolean;
  refreshing: boolean;
  onRefresh: () => void;
}) {
  const { t } = useTranslation();
  const needsSetup = state === "empty" || state === "unavailable";
  return (
    <section className="mx-auto w-full max-w-4xl px-3 py-2 sm:px-6" aria-live="polite">
      <div className="rounded-lg border border-border bg-muted/30 p-3 text-sm">
        <p className="font-medium">{t(`playground.serviceEntry.${state}Title`)}</p>
        <p className="mt-1 text-muted-foreground">{t(`playground.serviceEntry.${state}Description`)}</p>
        {needsSetup && !canManageServices && <p className="mt-1 text-muted-foreground">{t("playground.serviceEntry.contactAdmin")}</p>}
        {state !== "loading" && state !== "unselected" && (
          <div className="mt-2 flex flex-wrap items-center gap-2">
            <Button type="button" variant="outline" size="sm" disabled={refreshing} onClick={onRefresh}>
              {t(refreshing ? "playground.serviceEntry.refreshing" : "playground.serviceEntry.refresh")}
            </Button>
            {needsSetup && canManageServices && <Button asChild variant="ghost" size="sm">
              <Link to="/services">{t("playground.serviceEntry.manage")}</Link>
            </Button>}
          </div>
        )}
      </div>
    </section>
  );
}
