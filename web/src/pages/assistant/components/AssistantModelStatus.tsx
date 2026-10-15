import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";
import { Button } from "@/components/ui/button";

/** A successful catalog read does not establish live provider health or execution access. */
export function AssistantModelStatus({ state, canManageModels, onRefresh }: {
  state: "loading" | "error" | "empty" | "unselected" | "unavailable" | "ready";
  canManageModels: boolean;
  onRefresh: () => void;
}) {
  const { t } = useTranslation();
  if (state === "ready") return null;
  const needsSetup = state === "empty" || state === "unavailable";
  return <section aria-live="polite" className="mb-4 rounded-lg border border-border bg-muted/30 p-3 text-sm">
    <p className="font-medium">{t(`assistant.modelEntry.${state}Title`)}</p>
    <p className="mt-1 text-muted-foreground">{t(`assistant.modelEntry.${state}Description`)}</p>
    {needsSetup && !canManageModels && <p className="mt-1 text-muted-foreground">{t("assistant.modelEntry.contactAdmin")}</p>}
    {state !== "loading" && <div className="mt-2 flex flex-wrap gap-2">
      <Button type="button" variant="outline" size="sm" onClick={onRefresh}>{t("assistant.modelEntry.refresh")}</Button>
      {needsSetup && canManageModels && <Button asChild variant="ghost" size="sm"><Link to="/services?tab=providers">{t("assistant.modelEntry.manage")}</Link></Button>}
    </div>}
  </section>;
}
