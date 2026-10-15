/**
 * Document Version History Component
 *
 * Shows version history for a document with compare and restore functionality.
 */

import { SourceDocumentDialog } from "@/pages/assistant/components/SourceDocumentDialog";
import { useDocumentVersionNavigation, VERSION_PAGE_SIZE } from "./useDocumentVersionNavigation";
import { useTranslation } from "react-i18next";
import {
  History,
  GitCompare,
  RotateCcw,
  Clock,
  FileText,
  Cloud,
  Loader2,
  Plus,
  Minus,
  ArrowRight,
  X,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
interface DocumentVersionHistoryProps {
  datasetId: string;
  documentId: string;
  documentTitle: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onRestored?: () => void;
}

export function DocumentVersionHistory({
  datasetId,
  documentId,
  documentTitle,
  open,
  onOpenChange,
  onRestored,
}: DocumentVersionHistoryProps) {
  const { t, i18n } = useTranslation();

  const { sourcePreview, previewVersion, canPreviewVersion, closeSourcePreview, versions, currentVersion, total, loading, loadError, offset, loadPage,
    compareMode, setCompareMode, selectedForCompare, handleSelectForCompare,
    compareResult, dismissComparison, comparing, handleCompare,
    restoreVersion, setRestoreVersion, restoring, handleRestore, onDialogOpenChange,
  } = useDocumentVersionNavigation({ datasetId, documentId, open, onOpenChange, onRestored });

  function getChangeTypeBadge(changeType: string) {
    const styles: Record<string, string> = {
      created: "bg-green-500/15 text-green-700 dark:text-green-400 border-green-500/30",
      updated: "bg-blue-500/15 text-blue-700 dark:text-blue-400 border-blue-500/30",
      restored: "bg-amber-500/15 text-amber-700 dark:text-amber-400 border-amber-500/30",
      deleted: "bg-red-500/15 text-red-700 dark:text-red-400 border-red-500/30",
    };
    const labels: Record<string, string> = {
      created: t("knowledge.versionHistory.changeCreated"),
      updated: t("knowledge.versionHistory.changeUpdated"),
      restored: t("knowledge.versionHistory.changeRestored"),
      deleted: t("knowledge.versionHistory.changeDeleted"),
    };
    return (
      <Badge variant="outline" className={styles[changeType] || ""}>
        {labels[changeType] || changeType}
      </Badge>
    );
  }

  function formatDate(dateStr: string) {
    return new Date(dateStr).toLocaleString(i18n.language === "zh-CN" ? "zh-CN" : "en-US", {
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
    });
  }

  return (
    <>
      <Dialog open={open} onOpenChange={onDialogOpenChange}>
        <DialogContent className="max-w-3xl max-h-[80vh] flex flex-col">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <History className="h-5 w-5 text-primary" />
              {t("knowledge.versionHistory.title")}
              <span className="text-sm font-normal text-muted-foreground">
                - {documentTitle}
              </span>
            </DialogTitle>
          </DialogHeader>

          {/* Toolbar */}
          <div className="flex items-center justify-between border-b border-border pb-3">
            <div className="flex items-center gap-2">
              {!loading && !loadError && <>
              <Badge variant="secondary">
                {t("knowledge.versionHistory.totalVersions", { count: total })}
              </Badge>
              <Badge variant="outline">
                {t("knowledge.versionHistory.currentVersion", { version: currentVersion })}
              </Badge>
              </>}
            </div>
            <div className="flex items-center gap-2">
              {!compareMode ? (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setCompareMode(true)}
                  disabled={total < 2 || loading || loadError || restoring}
                >
                  <GitCompare className="h-4 w-4 mr-1.5" />
                  {t("knowledge.versionHistory.compare")}
                </Button>
              ) : (
                <>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => {
                      setCompareMode(false);
                    }}
                  >
                    {t("knowledge.versionHistory.cancelCompare")}
                  </Button>
                  <Button
                    size="sm"
                    onClick={handleCompare}
                    disabled={selectedForCompare.length !== 2 || comparing || loading || loadError || restoring}
                  >
                    {comparing ? (
                      <Loader2 className="h-4 w-4 mr-1.5 animate-spin" />
                    ) : (
                      <GitCompare className="h-4 w-4 mr-1.5" />
                    )}
                    {t("knowledge.versionHistory.compareSelected", { count: selectedForCompare.length })}
                  </Button>
                </>
              )}
            </div>
          </div>

          <p className="text-xs text-muted-foreground">{t("knowledge.versionHistory.pagingScope")}</p>
          {selectedForCompare.length > 0 && (
            <p role="status" className="text-sm text-muted-foreground">
              {t("knowledge.versionHistory.selectedAcrossPages", { versions: selectedForCompare.join(", ") })}
            </p>
          )}
          {!loading && !loadError && (
            <div className="flex items-center justify-between gap-3 text-sm">
              <span>{t("knowledge.versionHistory.pageRange", { start: total ? offset + 1 : 0, end: offset + versions.length, total })}</span>
              <div className="flex gap-2">
                <Button variant="outline" size="sm" disabled={offset === 0 || restoring} onClick={() => void loadPage(Math.max(0, offset - VERSION_PAGE_SIZE))}>{t("knowledge.versionHistory.previousPage")}</Button>
                <Button variant="outline" size="sm" disabled={offset + versions.length >= total || restoring} onClick={() => void loadPage(offset + VERSION_PAGE_SIZE)}>{t("knowledge.versionHistory.nextPage")}</Button>
                <Button variant="outline" size="sm" disabled={restoring} onClick={() => void loadPage(offset)}>{t("common.refresh")}</Button>
              </div>
            </div>
          )}
          {/* Content */}
          <div className="flex-1 overflow-hidden">
            {loading ? (
              <div className="flex items-center justify-center h-48">
                <Loader2 className="h-8 w-8 animate-spin text-primary" />
              </div>
            ) : loadError ? (
              <div role="alert" className="p-6 space-y-3">
                <p>{t("knowledge.versionHistory.pageReadFailed")}</p>
                <Button onClick={() => void loadPage(offset)}>{t("common.retry")}</Button>
              </div>
            ) : compareResult ? (
              /* Compare Result View */
              <div className="h-full flex flex-col">
                <div className="flex items-center justify-between mb-3">
                  <div className="flex items-center gap-2 text-sm">
                    <Badge variant="outline">{t("knowledge.versionHistory.version", { number: compareResult.from_version })}</Badge>
                    <ArrowRight className="h-4 w-4 text-muted-foreground" />
                    <Badge variant="outline">{t("knowledge.versionHistory.version", { number: compareResult.to_version })}</Badge>
                  </div>
                  <div className="flex items-center gap-3 text-sm">
                    <span className="text-green-600 flex items-center gap-1">
                      <Plus className="h-3.5 w-3.5" />
                      {t("knowledge.versionHistory.additions", { count: compareResult.stats.additions })}
                    </span>
                    <span className="text-red-600 flex items-center gap-1">
                      <Minus className="h-3.5 w-3.5" />
                      {t("knowledge.versionHistory.deletions", { count: compareResult.stats.deletions })}
                    </span>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={dismissComparison}
                    >
                      <X className="h-4 w-4" />
                    </Button>
                  </div>
                </div>
                <div className="flex-1 border rounded-lg overflow-auto">
                  <div className="p-3 font-mono text-sm">
                    {compareResult.diff.map((item, idx) => (
                      <div
                        key={idx}
                        className={`px-2 py-0.5 ${
                          item.type === "insert"
                            ? "bg-emerald-500/10 dark:bg-emerald-500/15 text-emerald-800 dark:text-emerald-300"
                            : item.type === "delete"
                            ? "bg-red-500/10 dark:bg-red-500/15 text-red-800 dark:text-red-300"
                            : "text-muted-foreground"
                        }`}
                      >
                        <span className="inline-block w-6 text-muted-foreground/50">
                          {item.type === "insert" ? "+" : item.type === "delete" ? "-" : " "}
                        </span>
                        {item.content}
                      </div>
                    ))}
                    {compareResult.diff.length === 0 && (
                      <div className="text-center py-8 text-muted-foreground">
                        {t("knowledge.versionHistory.sameContent")}
                      </div>
                    )}
                  </div>
                </div>
              </div>
            ) : versions.length === 0 ? (
              <div className="flex flex-col items-center justify-center h-48 text-muted-foreground">
                <History className="h-12 w-12 mb-3 opacity-50" />
                <p>{t("knowledge.versionHistory.noHistory")}</p>
                <p className="text-sm mt-1">{t("knowledge.versionHistory.autoSaveHint")}</p>
              </div>
            ) : (
              /* Version List */
              <div className="h-[400px] pr-4 overflow-auto">
                <div className="space-y-2">
                  {versions.map((version) => (
                    <div
                      key={version.version_id}
                      className={`
                        p-4 rounded-lg border transition-colors
                        ${
                          compareMode && selectedForCompare.includes(version.version_number)
                            ? "border-primary bg-primary/5"
                            : "border-border hover:border-primary/30 hover:bg-muted/30"
                        }
                        ${version.version_number === currentVersion ? "border-primary/50" : ""}
                      `}
                    >
                      <div className="flex items-start justify-between">
                        <div className="flex items-start gap-3">
                          {compareMode && (
                            <input
                              type="checkbox"
                              disabled={comparing || restoring}
                              checked={selectedForCompare.includes(version.version_number)}
                              onChange={() => handleSelectForCompare(version.version_number)}
                              className="mt-1 h-4 w-4 rounded border-border"
                            />
                          )}
                          <div>
                            <div className="flex items-center gap-2">
                              <span className="font-semibold">
                                {t("knowledge.versionHistory.version", { number: version.version_number })}
                              </span>
                              {version.version_number === currentVersion && (
                                <Badge className="bg-primary/10 text-primary border-primary/20">
                                  {t("knowledge.versionHistory.currentLabel")}
                                </Badge>
                              )}
                              {getChangeTypeBadge(version.change_type)}
                            </div>
                            <div className="flex items-center gap-4 mt-2 text-sm text-muted-foreground">
                              <span className="flex items-center gap-1">
                                <Clock className="h-3.5 w-3.5" />
                                {formatDate(version.created_at)}
                              </span>
                              <span className="flex items-center gap-1">
                                <FileText className="h-3.5 w-3.5" />
                                {t("knowledge.versionHistory.words", { count: version.word_count })}
                              </span>
                              {version.confluence_version && (
                                <span className="flex items-center gap-1">
                                  <Cloud className="h-3.5 w-3.5" />
                                  Confluence v{version.confluence_version}
                                </span>
                              )}
                            </div>
                            {version.change_reason && (
                              <p className="text-sm text-muted-foreground mt-1.5 line-clamp-1">
                                {version.change_reason}
                              </p>
                            )}
                          </div>
                        </div>
                        {!compareMode && <Button variant="outline" size="sm" disabled={!canPreviewVersion(version)}
                          title={!canPreviewVersion(version) ? t("knowledge.versionHistory.sourceUnavailable") : undefined}
                          onClick={() => previewVersion(version.version_number)}>{t("knowledge.versionHistory.viewSource")}</Button>}
                        {!compareMode && version.version_number !== currentVersion && (
                          <Button
                            variant="ghost"
                            size="sm"
                            onClick={() => setRestoreVersion(version.version_number)}
                            className="text-amber-600 dark:text-amber-400 hover:text-amber-700 dark:hover:text-amber-300 hover:bg-amber-500/10"
                          >
                            <RotateCcw className="h-4 w-4 mr-1.5" />
                            {t("knowledge.versionHistory.rollback")}
                          </Button>
                        )}
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            )}
          </div>
        </DialogContent>
      </Dialog>

      {sourcePreview && <SourceDocumentDialog source={sourcePreview} onClose={closeSourcePreview} />}

      {/* Restore Confirmation Dialog */}
      <AlertDialog open={restoreVersion !== null} onOpenChange={() => setRestoreVersion(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("knowledge.versionHistory.confirmRollback", { version: restoreVersion })}</AlertDialogTitle>
            <AlertDialogDescription className="space-y-2">
              <span className="block">
                {t("knowledge.versionHistory.rollbackDesc", { title: documentTitle, version: restoreVersion })}
              </span>
              <span className="block text-amber-600">
                {t("knowledge.versionHistory.rollbackWarning")}
              </span>
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={restoring}>{t("common.cancel")}</AlertDialogCancel>
            <AlertDialogAction
              onClick={handleRestore}
              disabled={restoring}
              className="bg-amber-600 hover:bg-amber-700"
            >
              {restoring ? (
                <>
                  <Loader2 className="h-4 w-4 mr-1.5 animate-spin" />
                  {t("knowledge.versionHistory.restoring")}
                </>
              ) : (
                <>
                  <RotateCcw className="h-4 w-4 mr-1.5" />
                  {t("knowledge.versionHistory.confirmRollbackBtn")}
                </>
              )}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}
