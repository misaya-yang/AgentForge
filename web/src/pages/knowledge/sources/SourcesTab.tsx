/** Dataset source entry points and the current recurring connector state. */
import { ArrowRight, Cloud, FileText, Link, Upload } from "lucide-react";
import { useTranslation } from "react-i18next";

import { SourceManualSync } from "./SourceManualSync";
import { SourceConnectionControls } from "./SourceConnectionControls";
import { SourceConnectionSetup } from "./SourceConnectionSetup";
import { SourceBindingSetup } from "./SourceBindingSetup";

import type { DatasetSources } from "@/api/knowledge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  bindingPresentationState,
  canShowSuccessfulSyncCount,
  connectorPresentationState,
} from "./sourcePresentation";

interface SourcesTabProps {
  datasetId?: string;
  onUploadClick: () => void;
  onUrlClick: () => void;
  sources: DatasetSources | null;
  sourcesLoading: boolean;
  sourcesFailed: boolean;
  onRetrySources: () => void;
}

export function SourcesTab({
  datasetId,
  onUploadClick,
  onUrlClick,
  sources,
  sourcesLoading,
  sourcesFailed,
  onRetrySources,
}: SourcesTabProps) {
  const { t, i18n } = useTranslation();
  const connectorState = connectorPresentationState(sources, sourcesLoading, sourcesFailed);
  const countKnown = !!sources && !sourcesFailed;
  const sourceCards = [
    {
      key: "file",
      title: t("knowledge.sources.fileUpload"),
      description: t("knowledge.sources.fileUploadDesc"),
      icon: Upload,
      stat: t("knowledge.sources.uploadedFiles"),
      count: countKnown ? sources.file_uploads.count : null,
      action: t("knowledge.sources.uploadFile"),
      onClick: onUploadClick,
    },
    {
      key: "url",
      title: t("knowledge.sources.webImport"),
      description: t("knowledge.sources.webImportDesc"),
      icon: Link,
      stat: t("knowledge.sources.importedPages"),
      count: countKnown ? sources.url_imports.count : null,
      action: t("knowledge.sources.importPage"),
      onClick: onUrlClick,
    },
  ];

  function lastSuccessLabel(value: string | null): string | null {
    if (!value) return null;
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? null : date.toLocaleString(i18n.language);
  }

  return (
    <div className="space-y-6">
      <div>
        <h3 className="text-lg font-semibold">{t("knowledge.sources.sourceManagement")}</h3>
        <p className="mt-1 text-sm text-muted-foreground">
          {t("knowledge.sources.sourceManagementDesc")}
        </p>
      </div>

      {sourcesFailed && (
        <div role="alert" className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm">
          <p>{t("knowledge.sources.loadFailed")}</p>
        </div>
      )}

      <Button
        size="sm"
        variant="outline"
        disabled={sourcesLoading}
        onClick={onRetrySources}
        aria-label={t("knowledge.sources.refreshStatus")}
      >
        {t(sourcesLoading ? "common.loading" : sourcesFailed ? "common.retry" : "knowledge.sources.refreshStatus")}
      </Button>

      <div className="grid grid-cols-1 gap-4 lg:grid-cols-3 lg:gap-6">
        {sourceCards.map((card) => {
          const Icon = card.icon;
          return (
            <Card key={card.key} variant="interactive" className="border-border/80 p-5 sm:p-6">
              <div className="mb-4 flex h-12 w-12 items-center justify-center rounded-xl border border-primary/15 bg-primary/10">
                <Icon className="h-6 w-6 text-primary" />
              </div>
              <h4 className="mb-2 text-base font-semibold">{card.title}</h4>
              <p className="mb-4 text-sm text-muted-foreground">{card.description}</p>
              <div className="mb-4 flex items-center gap-2 text-sm">
                <FileText className="h-4 w-4 text-muted-foreground" />
                <span className="text-muted-foreground">{card.stat}:</span>
                <span className="font-medium">{card.count ?? t("knowledge.sources.countUnknown")}</span>
              </div>
              <Button variant="outline" size="sm" className="h-10 w-full sm:h-8" onClick={card.onClick}>
                {card.action}<ArrowRight className="ml-1.5 h-4 w-4" />
              </Button>
            </Card>
          );
        })}

        <Card className="border-border/80 p-5 sm:p-6" data-testid="recurring-source-card">
          <div className="mb-4 flex h-12 w-12 items-center justify-center rounded-xl border border-primary/15 bg-primary/10">
            <Cloud className="h-6 w-6 text-primary" />
          </div>
          <h4 className="mb-2 text-base font-semibold">{t("knowledge.sources.recurringSync")}</h4>
          <p className="mb-3 text-sm text-muted-foreground">{t("knowledge.sources.recurringSyncDesc")}</p>
          <p role="status" className="text-sm font-medium">
            {t(`knowledge.sources.connectorState.${connectorState}`)}
          </p>
          {connectorState === "available" && sources && (
            <div className="mt-3 space-y-3 text-xs">
              {sources.confluence_bindings.length === 0 ? (
                <p className="text-muted-foreground">{t("knowledge.sources.noRecurringBinding")}</p>
              ) : sources.confluence_bindings.map((binding) => {
                const state = bindingPresentationState(binding);
                const lastSuccess = lastSuccessLabel(binding.last_success_at);
                return (
                  <div key={binding.binding_id} className="rounded-lg border border-border/70 p-2">
                    <p className="font-medium">{binding.space_name || binding.binding_id}</p>
                    <p className="text-muted-foreground">
                      {t("knowledge.sources.bindingStatus", { status: binding.status || t("knowledge.sources.statusUnknown") })}
                    </p>
                    <p className={state === "problem" ? "text-amber-700 dark:text-amber-400" : "text-muted-foreground"}>
                      {t(`knowledge.sources.bindingState.${state}`)}
                    </p>
                    {lastSuccess && (
                      <p className="text-muted-foreground">
                        {t("knowledge.sources.lastSuccessfulSync", { date: lastSuccess })}
                      </p>
                    )}
                    {datasetId && <SourceManualSync datasetId={datasetId} binding={binding} disabled={sourcesLoading || sourcesFailed} />}
                    {canShowSuccessfulSyncCount(binding) && (
                      <p className="text-muted-foreground">
                        {t("knowledge.sources.lastKnownPages", { count: binding.page_count })}
                      </p>
                    )}
                  </div>
                );
              })}
            </div>
          )}
          {datasetId && <SourceConnectionControls datasetId={datasetId}
            reconnectEnabled={!sourcesLoading && !sourcesFailed && sources?.source_connection_reconnect?.available === true}
            enabled={!sourcesLoading && !sourcesFailed && sources?.source_connection_controls_available === true}
            onChanged={onRetrySources} />}
          {datasetId && <SourceConnectionSetup datasetId={datasetId}
            enabled={!sourcesLoading && !sourcesFailed && sources?.source_connection_setup?.available === true}
            reason={sources?.source_connection_setup?.reason ?? null} />}
          {datasetId && <SourceBindingSetup datasetId={datasetId}
            enabled={!sourcesLoading && !sourcesFailed && sources?.source_binding_available === true}
            onSaved={onRetrySources} />}
          <p className="mt-4 text-xs text-muted-foreground">{t("knowledge.sources.noSyncAction")}</p>
        </Card>
      </div>

      {countKnown ? (
        <p className="border-t pt-4 text-center text-sm text-muted-foreground">
          {t("knowledge.sources.totalDocSources", { count: sources.total_documents })}
        </p>
      ) : null}
    </div>
  );
}
