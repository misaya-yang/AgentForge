import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { CatalogBoard } from "@/components/connect/ConnectAppsDialog";
import { bindShell, installOverlayFallback } from "@/pages/spaces/overlay";

export function ConnectorsSettingsPage() {
  const { t } = useTranslation();
  useEffect(() => {
    bindShell(document.body);
    installOverlayFallback(document.body);
  }, []);
  return (
    <section className="mx-auto flex max-w-6xl flex-col gap-4" aria-labelledby="connectors-settings-title">
      <header>
        <h1 id="connectors-settings-title" className="text-lg font-semibold">{t("connect.manageTitle")}</h1>
        <p className="text-sm text-muted-foreground">{t("connect.manageSummary")}</p>
      </header>
      <CatalogBoard manage />
    </section>
  );
}
