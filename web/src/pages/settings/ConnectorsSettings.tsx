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
    <section className="forge-page" aria-labelledby="connectors-settings-title">
      <header className="forge-page-header">
        <div><h1 id="connectors-settings-title">{t("connect.manageTitle")}</h1>
        <p>{t("connect.manageSummary")}</p></div>
      </header>
      <CatalogBoard manage />
    </section>
  );
}
