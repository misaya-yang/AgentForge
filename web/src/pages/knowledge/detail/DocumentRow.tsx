import { useState } from "react";
import { Archive, ArchiveRestore, Flame, History, Pencil, RefreshCcw, Tags, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";

import {
  documentNeedsLifecyclePolling,
  resolveDisplayStatus,
  type Document,
} from "@/types/knowledge";
import { StatusBadge } from "@/pages/knowledge/detail/StatusBadge";
import {
  buildStageTimings,
  formatStageDuration,
  runningStageDurationMs,
  type DocumentStage,
} from "@/pages/knowledge/detail/documentStages";
import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
import { Switch } from "@/components/ui/switch";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
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
import { DocumentVersionHistory } from "./DocumentVersionHistory";
import { DocumentProcessingDiagnostics } from "./DocumentProcessingDiagnostics";
import { DocumentTextEditorDialog } from "./DocumentTextEditorDialog";
import { canEditTextSource } from "./documentTextEditing";

const STAGE_LABEL_KEYS: Record<DocumentStage, string> = {
  parsing: "knowledge.documentRow.stageParsing",
  splitting: "knowledge.documentRow.stageSplitting",
  indexing: "knowledge.documentRow.stageIndexing",
};

type DocumentPipelineAction = "reembed" | "reprocess" | "recover" | "retry";

export function DocumentRow({
  doc,
  datasetId,
  selected,
  checked,
  onSelect,
  onCheck,
  onReembed,
  onReprocess,
  onRecover,
  onRetry,
  onDelete,
  onToggleEnabled,
  onArchive,
  onUnarchive,
  onEditMetadata,
  busyLifecycle = false,
  canManageProcessing = false,
  onVersionRestored,
  onTextSourcePublished,
  showCheckbox = false,
}: {
  doc: Document;
  datasetId: string;
  selected: boolean;
  checked?: boolean;
  onSelect: () => void;
  onCheck?: (checked: boolean) => void;
  onReembed: () => Promise<void>;
  onReprocess: () => Promise<void>;
  onRecover: () => Promise<void>;
  onRetry: () => Promise<void>;
  onDelete: () => Promise<void>;
  onToggleEnabled?: (enabled: boolean) => void;
  onArchive?: () => void;
  onUnarchive?: () => void;
  onEditMetadata?: () => void;
  /** Document has an in-flight enable/disable/archive mutation. */
  busyLifecycle?: boolean;
  canManageProcessing?: boolean;
  onVersionRestored?: () => void;
  onTextSourcePublished?: () => void;
  showCheckbox?: boolean;
}) {
  const { t, i18n } = useTranslation();
  const [loading, setLoading] = useState(false);
  const [pipelineAction, setPipelineAction] = useState<DocumentPipelineAction | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [versionHistoryOpen, setVersionHistoryOpen] = useState(false);
  const [textEditorOpen, setTextEditorOpen] = useState(false);

  // Lifecycle badges are driven by the resolved display status, so they stay
  // correct whether the payload carries a backend stamp (post-D1 lists and
  // every mutation response) or only raw fields.
  const display = resolveDisplayStatus(doc);
  const isArchived = display === "archived";
  const isDisabled = display === "disabled";
  const inactive = isArchived || isDisabled;
  const documentBusy = busyLifecycle || documentNeedsLifecyclePolling(doc);
  const pipelineActionsDisabled =
    !canManageProcessing || loading || documentBusy || doc.enabled === false || doc.archived === true;
  const retryAdviceBlocked = Boolean(doc.processing_diagnostics?.retry_blocked_reason);
  const retryActionDisabled = pipelineActionsDisabled || retryAdviceBlocked;
  const canRecover = display === "error";

  // Per-stage durations (migration 101 forward contract, PRD A10): rendered
  // only for actively-processing rows whose payloads carry stage timestamps;
  // anything else keeps the existing coarse StatusBadge progress.
  const stageSummary = (() => {
    if (display !== "queuing" && display !== "indexing") return null;
    const timings = buildStageTimings(doc);
    if (timings.length === 0) return null;
    const now = Date.now();
    return timings
      .map((timing) => {
        const label = t(STAGE_LABEL_KEYS[timing.stage]);
        const duration = timing.running
          ? `${formatStageDuration(runningStageDurationMs(timing, now))}…`
          : formatStageDuration(timing.durationMs ?? 0);
        return `${label} ${duration}`;
      })
      .join(" · ");
  })();

  const pipelineCallbacks: Record<DocumentPipelineAction, () => Promise<void>> = {
    reembed: onReembed,
    reprocess: onReprocess,
    recover: onRecover,
    retry: onRetry,
  };
  const pipelineDialog = pipelineAction
    ? {
        title: t(`knowledge.documentRow.${pipelineAction}DialogTitle`),
        description: t(`knowledge.documentRow.${pipelineAction}Desc`, { title: doc.title }),
        hint: t(`knowledge.documentRow.${pipelineAction}Hint`),
        confirm: t(`knowledge.documentRow.${pipelineAction}Confirm`),
        run: pipelineCallbacks[pipelineAction],
      }
    : null;

  const handleAction = async (action: () => Promise<void>) => {
    if (loading) return;
    setLoading(true);
    try {
      await action();
    } finally {
      setLoading(false);
    }
  };

  const formatFileSize = (bytes?: number) => {
    if (!bytes) return "-";
    if (bytes < 1024) return `${bytes}B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`;
    return `${(bytes / (1024 * 1024)).toFixed(2)}MB`;
  };

  const getFileIcon = () => {
    const ext = doc.title?.split(".").pop()?.toLowerCase() || "";
    if (["pdf"].includes(ext)) {
      return (
        <div className="w-6 h-6 rounded bg-rose-500 flex items-center justify-center text-white text-xs font-bold">
          P
        </div>
      );
    }
    if (["doc", "docx"].includes(ext)) {
      return (
        <div className="w-6 h-6 rounded bg-amber-500 flex items-center justify-center text-white text-xs font-bold">
          W
        </div>
      );
    }
    if (["xls", "xlsx"].includes(ext)) {
      return (
        <div className="w-6 h-6 rounded bg-emerald-500 flex items-center justify-center text-white text-xs font-bold">
          X
        </div>
      );
    }
    return (
      <div className="w-6 h-6 rounded bg-slate-500 flex items-center justify-center text-white text-xs font-bold">
        T
      </div>
    );
  };

  return (
    <>
      <div
        data-testid={`doc-row-${doc.document_id}`}
        className={`
          flex flex-wrap items-center gap-y-3 px-4 py-3 border-b border-border/60 last:border-b-0 hover:bg-muted/40 transition-colors
          sm:flex-nowrap sm:gap-y-0 sm:px-5
          ${selected ? "bg-primary/5" : ""} ${checked ? "bg-primary/10" : ""}
          ${inactive && !selected && !checked ? "bg-muted/30" : ""}
        `}
      >
        {showCheckbox && (
          <div className="mr-3 flex items-center">
            <Checkbox
              data-testid={`doc-select-${doc.document_id}`}
              checked={checked}
              onCheckedChange={(val) => onCheck?.(val === true)}
              onClick={(e) => e.stopPropagation()}
              aria-label={t("knowledge.documentRow.selectDoc")}
            />
          </div>
        )}
        <div className="flex min-w-0 basis-full items-center gap-3 sm:basis-auto sm:flex-1">
          {getFileIcon()}
          <div className="flex min-w-0 flex-1 flex-col gap-0.5">
            <button
              type="button"
              className={`min-w-0 truncate text-left text-sm font-medium ${
                inactive
                  ? "text-muted-foreground hover:text-foreground"
                  : "text-primary hover:text-primary/90"
              }`}
              onClick={onSelect}
            >
              {doc.title}
            </button>
            <DocumentProcessingDiagnostics document={doc} compact />
            {stageSummary && (
              <div
                data-testid={`doc-stage-times-${doc.document_id}`}
                className="truncate text-xs text-muted-foreground"
                title={stageSummary}
              >
                {stageSummary}
              </div>
            )}
          </div>
        </div>

        <div className="hidden w-24 text-sm text-muted-foreground text-center sm:block">
          {formatFileSize(doc.size_bytes)}
        </div>

        <div className="order-2 flex w-auto flex-wrap items-center justify-start gap-1 sm:order-none sm:w-28 sm:justify-center">
          <StatusBadge
            status={doc.status}
            progress={doc.progress}
            metadata={doc.metadata}
          />
          {isArchived && (
            <Badge
              variant="outline"
              data-testid={`doc-archived-badge-${doc.document_id}`}
              className="text-xs font-medium bg-amber-500/15 text-amber-700 dark:text-amber-400 border-amber-500/30"
            >
              {t("knowledge.displayStatus.archived")}
            </Badge>
          )}
          {isDisabled && (
            <Badge
              variant="outline"
              data-testid={`doc-disabled-badge-${doc.document_id}`}
              className="text-xs font-medium bg-slate-500/15 text-slate-700 dark:text-slate-400 border-slate-500/30"
            >
              {t("knowledge.displayStatus.disabled")}
            </Badge>
          )}
          {/* Retrieval telemetry (PRD §5-#16): the backend writer lands with
              T2; show the badge whenever a count is already present. */}
          {typeof doc.hit_count === "number" && doc.hit_count > 0 && (
            <Badge
              variant="outline"
              data-testid={`doc-hit-count-${doc.document_id}`}
              title={t("knowledge.documentRow.hitCountTitle", { count: doc.hit_count })}
              className="gap-1 text-xs font-medium bg-sky-500/10 text-sky-700 dark:text-sky-400 border-sky-500/30"
            >
              <Flame className="h-3 w-3" aria-hidden="true" />
              {doc.hit_count}
            </Badge>
          )}
        </div>

        <div className="hidden w-28 text-sm text-muted-foreground text-center sm:block">{t("knowledge.documentRow.defaultCategory")}</div>

        <div className="hidden w-40 text-sm text-muted-foreground text-center sm:block">
          {doc.created_at
            ? new Date(doc.created_at).toLocaleString(i18n.language === "zh-CN" ? "zh-CN" : "en-US", {
                year: "numeric",
                month: "2-digit",
                day: "2-digit",
                hour: "2-digit",
                minute: "2-digit",
              })
            : "-"}
        </div>

        <div className="order-3 ml-auto flex w-auto flex-wrap items-center justify-end gap-1 text-sm sm:order-none sm:ml-0 sm:w-64 sm:gap-2">
          <button
            className="min-h-10 px-1 text-primary hover:text-primary/90 disabled:cursor-not-allowed disabled:opacity-50 sm:min-h-0 sm:px-0"
            onClick={(e) => {
              e.stopPropagation();
              onSelect();
            }}
            disabled={loading}
          >
            {t("knowledge.documentRow.segments")}
          </button>
          {onToggleEnabled && (
            <Switch
              data-testid={`doc-switch-${doc.document_id}`}
              checked={doc.enabled !== false}
              disabled={loading || documentBusy || isArchived}
              onCheckedChange={(checked) => onToggleEnabled(checked)}
              title={
                isArchived
                  ? t("knowledge.documentRow.switchArchivedHint")
                  : doc.enabled !== false
                    ? t("knowledge.documentRow.disableDoc")
                    : t("knowledge.documentRow.enableDoc")
              }
              aria-label={
                doc.enabled !== false
                  ? t("knowledge.documentRow.disableDoc")
                  : t("knowledge.documentRow.enableDoc")
              }
            />
          )}
          {(isArchived ? onUnarchive : onArchive) && (
            <button
              data-testid={
                isArchived
                  ? `doc-unarchive-${doc.document_id}`
                  : `doc-archive-${doc.document_id}`
              }
              className="inline-flex h-10 w-10 items-center justify-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50 sm:h-7 sm:w-7"
              onClick={(e) => {
                e.stopPropagation();
                (isArchived ? onUnarchive : onArchive)?.();
              }}
              disabled={loading || documentBusy}
              title={
                isArchived
                  ? t("knowledge.documentRow.unarchiveTitle")
                  : t("knowledge.documentRow.archiveTitle")
              }
              aria-label={
                isArchived
                  ? t("knowledge.documentRow.unarchiveTitle")
                  : t("knowledge.documentRow.archiveTitle")
              }
            >
              {isArchived ? (
                <ArchiveRestore className="h-3.5 w-3.5" />
              ) : (
                <Archive className="h-3.5 w-3.5" />
              )}
            </button>
          )}
          <button
            data-testid={`doc-metadata-${doc.document_id}`}
            className="inline-flex h-10 w-10 items-center justify-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50 sm:h-7 sm:w-7"
            onClick={(event) => {
              event.stopPropagation();
              onEditMetadata?.();
            }}
            disabled={loading || !onEditMetadata}
            title={t("knowledge.metadata.editAction")}
            aria-label={t("knowledge.metadata.editAction")}
          >
            <Tags className="h-3.5 w-3.5" />
          </button>
          {canEditTextSource(doc) && (
            <button
              data-testid={`doc-edit-text-source-${doc.document_id}`}
              type="button"
              className="inline-flex h-10 w-10 items-center justify-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50 sm:h-7 sm:w-7"
              onClick={(event) => {
                event.stopPropagation();
                setTextEditorOpen(true);
              }}
              disabled={loading || documentBusy}
              title={t("knowledge.documentRow.editTextSource")}
              aria-label={t("knowledge.documentRow.editTextSource")}
            >
              <Pencil className="h-3.5 w-3.5" aria-hidden="true" />
            </button>
          )}
          <button
            className="inline-flex h-10 w-10 items-center justify-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50 sm:h-7 sm:w-7"
            onClick={(e) => {
              e.stopPropagation();
              setVersionHistoryOpen(true);
            }}
            disabled={loading}
            title={t("knowledge.documentRow.versionHistoryTitle")}
            aria-label={t("knowledge.documentRow.versionHistoryTitle")}
          >
            <History className="h-3.5 w-3.5" />
          </button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <button
                data-testid={`doc-index-actions-${doc.document_id}`}
                className="inline-flex h-10 w-10 items-center justify-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50 sm:h-7 sm:w-7"
                onClick={(event) => event.stopPropagation()}
                disabled={pipelineActionsDisabled}
                title={t("knowledge.documentRow.pipelineActionsTitle")}
                aria-label={t("knowledge.documentRow.pipelineActionsTitle")}
              >
                <RefreshCcw className={`h-3.5 w-3.5 ${loading ? "animate-spin" : ""}`} />
              </button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-72">
              <DropdownMenuItem
                data-testid={`doc-reembed-${doc.document_id}`}
                onSelect={() => setPipelineAction("reembed")}
                className="items-start"
              >
                <span>
                  <span className="block font-medium">
                    {t("knowledge.documentRow.reembedTitle")}
                  </span>
                  <span className="block text-xs text-muted-foreground">
                    {t("knowledge.documentRow.reembedMenuHint")}
                  </span>
                </span>
              </DropdownMenuItem>
              <DropdownMenuItem
                data-testid={`doc-reprocess-${doc.document_id}`}
                onSelect={() => setPipelineAction("reprocess")}
                className="items-start"
              >
                <span>
                  <span className="block font-medium">
                    {t("knowledge.documentRow.reprocessTitle")}
                  </span>
                  <span className="block text-xs text-muted-foreground">
                    {t("knowledge.documentRow.reprocessMenuHint")}
                  </span>
                </span>
              </DropdownMenuItem>
              {canRecover && (
                <>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem
                    data-testid={`doc-recover-${doc.document_id}`}
                    disabled={retryActionDisabled}
                    onSelect={() => setPipelineAction("recover")}
                    className="items-start"
                  >
                    <span>
                      <span className="block font-medium">
                        {t("knowledge.documentRow.recoverTitle")}
                      </span>
                      <span className="block text-xs text-muted-foreground">
                        {t("knowledge.documentRow.recoverMenuHint")}
                      </span>
                    </span>
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    data-testid={`doc-retry-${doc.document_id}`}
                    disabled={retryActionDisabled}
                    onSelect={() => setPipelineAction("retry")}
                    className="items-start text-amber-700 focus:text-amber-800 dark:text-amber-400"
                  >
                    <span>
                      <span className="block font-medium">
                        {t("knowledge.documentRow.retryTitle")}
                      </span>
                      <span className="block text-xs opacity-80">
                        {t("knowledge.documentRow.retryMenuHint")}
                      </span>
                    </span>
                  </DropdownMenuItem>
                </>
              )}
            </DropdownMenuContent>
          </DropdownMenu>
          <button
            data-testid={`doc-delete-${doc.document_id}`}
            className="inline-flex h-10 w-10 items-center justify-center rounded-md text-muted-foreground hover:bg-destructive/10 hover:text-destructive disabled:cursor-not-allowed disabled:opacity-50 sm:h-7 sm:w-7"
            onClick={(e) => {
              e.stopPropagation();
              setDeleteOpen(true);
            }}
            disabled={loading}
            title={t("common.delete")}
            aria-label={t("common.delete")}
          >
            <Trash2 className="h-3.5 w-3.5" />
          </button>
        </div>
      </div>

      <AlertDialog
        open={pipelineDialog !== null}
        onOpenChange={(open) => {
          if (!open) setPipelineAction(null);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{pipelineDialog?.title}</AlertDialogTitle>
            <AlertDialogDescription className="space-y-2">
              <span className="block">{pipelineDialog?.description}</span>
              <span className="block text-xs text-muted-foreground">
                {pipelineDialog?.hint}
              </span>
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
            <AlertDialogAction
              disabled={pipelineActionsDisabled || ((pipelineAction === "retry" || pipelineAction === "recover") && retryAdviceBlocked)}
              onClick={() => {
                if (pipelineActionsDisabled || ((pipelineAction === "retry" || pipelineAction === "recover") && retryAdviceBlocked)) return;
                const run = pipelineDialog?.run;
                setPipelineAction(null);
                if (run) handleAction(run);
              }}
            >
              {pipelineDialog?.confirm}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog open={deleteOpen} onOpenChange={setDeleteOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("knowledge.documentRow.confirmDeleteDoc")}</AlertDialogTitle>
            <AlertDialogDescription>
              {t("knowledge.documentRow.deleteDocDesc", { title: doc.title })}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
            <AlertDialogAction
              className="bg-rose-600 hover:bg-rose-700"
              onClick={() => {
                setDeleteOpen(false);
                handleAction(onDelete);
              }}
            >
              {t("knowledge.documentRow.confirmDelete")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <DocumentVersionHistory
        datasetId={datasetId}
        documentId={doc.document_id}
        documentTitle={doc.title || t("knowledge.documentRow.untitledDocument")}
        open={versionHistoryOpen}
        onOpenChange={setVersionHistoryOpen}
        onRestored={onVersionRestored}
      />
      <DocumentTextEditorDialog
        datasetId={datasetId}
        documentId={doc.document_id}
        documentTitle={doc.title || t("knowledge.documentRow.untitledDocument")}
        open={textEditorOpen}
        onOpenChange={setTextEditorOpen}
        onPublished={onTextSourcePublished}
      />
    </>
  );
}
