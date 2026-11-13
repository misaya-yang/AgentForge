import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Loader2 } from "lucide-react";
import { useTranslation } from "react-i18next";

import {
  getActiveDocumentSource,
  getDocument,
  replaceDocumentContent,
} from "@/api/knowledge";
import type { Document } from "@/types/knowledge";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Textarea } from "@/components/ui/textarea";
import { canEditTextSource } from "./documentTextEditing";

type EditorPhase =
  | "loading"
  | "load_error"
  | "not_editable"
  | "blocked"
  | "idle"
  | "saving"
  | "processing"
  | "success"
  | "unchanged"
  | "failed"
  | "conflict"
  | "unknown";

interface TextSourceSnapshot {
  documentId: string;
  datasetId: string;
  source_type: "text";
  content: string;
  currentVersion: number;
  status: Document["status"];
  enabled?: boolean;
  archived?: boolean;
  progress?: number;
  editable: boolean;
}

interface PendingReplacement {
  previousVersion: number;
  candidateVersion: number;
}

interface EditorView {
  scope: string;
  phase: EditorPhase;
  source: TextSourceSnapshot | null;
  draft: string;
  pending: PendingReplacement | null;
  progress: number | null;
  pollPaused: boolean;
}

function emptyView(scope: string): EditorView {
  return { scope, phase: "loading", source: null, draft: "", pending: null, progress: null, pollPaused: false };
}

function asSnapshot(value: Document, datasetId: string, documentId: string): TextSourceSnapshot | null {
  if (value.dataset_id !== datasetId || value.document_id !== documentId
    || value.source_type !== "text" || typeof value.content !== "string"
    || !Number.isSafeInteger(value.current_version) || (value.current_version ?? 0) < 1) return null;
  return {
    datasetId,
    documentId,
    source_type: "text",
    content: value.content,
    currentVersion: value.current_version!,
    status: value.status,
    enabled: value.enabled,
    archived: value.archived,
    editable: canEditTextSource(value),
    progress: Number.isFinite(value.progress) && value.progress! >= 0 && value.progress! <= 100
      ? value.progress : undefined,
  };
}

function responseStatus(error: unknown): number | null {
  const status = (error as { response?: { status?: unknown } })?.response?.status;
  return typeof status === "number" ? status : null;
}

export function DocumentTextEditorDialog({
  datasetId,
  documentId,
  documentTitle,
  open,
  onOpenChange,
  onPublished,
}: {
  datasetId: string;
  documentId: string;
  documentTitle: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Refresh the document list after the request is accepted and after publication. */
  onPublished?: () => void;
}) {
  const { t } = useTranslation();
  const user = useAuthStore((state) => state.user);
  const token = useAuthStore((state) => state.token);
  const scope = JSON.stringify([datasetId, documentId, token, sdkAuthorityScope(user)]);
  const scopeRef = useRef({ value: scope, generation: 0 });
  if (scopeRef.current.value !== scope) {
    scopeRef.current = { value: scope, generation: scopeRef.current.generation + 1 };
  }
  const mounted = useRef(true);
  const openRef = useRef(open);
  openRef.current = open;
  const loadSequence = useRef(0);
  const openedScope = useRef<string | null>(null);
  const onPublishedRef = useRef(onPublished);
  onPublishedRef.current = onPublished;
  const [view, setView] = useState<EditorView>(() => emptyView(scope));
  const ownedView = useMemo(() => view.scope === scope ? view : emptyView(scope), [scope, view]);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  const isCurrent = useCallback((expectedScope: string, expectedGeneration: number) =>
    mounted.current && scopeRef.current.value === expectedScope
      && scopeRef.current.generation === expectedGeneration,
  []);

  const patchView = useCallback((expectedScope: string, patch: Partial<EditorView>) => {
    setView((previous) => ({
      ...(previous.scope === expectedScope ? previous : emptyView(expectedScope)),
      ...patch,
      scope: expectedScope,
    }));
  }, []);

  const loadCurrentSource = useCallback(async () => {
    const requestScope = scope;
    const requestGeneration = scopeRef.current.generation;
    const sequence = ++loadSequence.current;
    patchView(requestScope, { phase: "loading", progress: null, pollPaused: false });
    try {
      const document = await getActiveDocumentSource(datasetId, documentId);
      if (!isCurrent(requestScope, requestGeneration) || !openRef.current || loadSequence.current !== sequence) return;
      const source = asSnapshot(document, datasetId, documentId);
      if (!source) {
        patchView(requestScope, { phase: "not_editable", source: null, draft: "", pending: null });
        return;
      }
      if (!source.editable) {
        patchView(requestScope, {
          phase: source.status === "error" || source.status === "failed" ? "failed" : "blocked",
          source,
          draft: source.content,
          pending: null,
          progress: source.progress ?? null,
        });
        return;
      }
      patchView(requestScope, { phase: "idle", source, draft: source.content, pending: null, progress: null, pollPaused: false });
    } catch {
      if (isCurrent(requestScope, requestGeneration) && openRef.current && loadSequence.current === sequence) {
        patchView(requestScope, { phase: "load_error", source: null, draft: "", pending: null, progress: null });
      }
    }
  }, [datasetId, documentId, isCurrent, patchView, scope]);

  useEffect(() => {
    if (!open) {
      openedScope.current = null;
      return;
    }
    if (openedScope.current === scope) return;
    openedScope.current = scope;
    if (ownedView.phase === "processing" && ownedView.pending) {
      patchView(scope, { pollPaused: false });
      return;
    }
    void loadCurrentSource();
  }, [loadCurrentSource, open, ownedView.pending, ownedView.phase, patchView, scope]);

  useEffect(() => {
    const pending = ownedView.pending;
    if (!open || ownedView.phase !== "processing" || !pending || ownedView.pollPaused) return;
    const candidateVersion = pending.candidateVersion;
    const requestScope = scope;
    const requestGeneration = scopeRef.current.generation;
    const controller = new AbortController();
    const deadline = Date.now() + 120_000;
    let timer: ReturnType<typeof setTimeout> | undefined;

    async function checkStatus() {
      if (!isCurrent(requestScope, requestGeneration) || !openRef.current) return;
      try {
        const document = await getDocument(datasetId, documentId, { signal: controller.signal });
        if (!isCurrent(requestScope, requestGeneration) || controller.signal.aborted) return;
        if (document.dataset_id !== datasetId || document.document_id !== documentId) {
          patchView(requestScope, { phase: "unknown", pending: null });
          return;
        }
        if (document.status === "error" || document.status === "failed") {
          patchView(requestScope, { phase: "failed", pending: null, progress: document.progress ?? null });
          onPublishedRef.current?.();
          return;
        }
        const currentVersion = document.current_version;
        if (document.status === "completed" && Number.isSafeInteger(currentVersion)
          && (currentVersion ?? 0) >= candidateVersion) {
          const sourceDocument = await getActiveDocumentSource(datasetId, documentId, { signal: controller.signal });
          if (!isCurrent(requestScope, requestGeneration) || controller.signal.aborted) return;
          const source = asSnapshot(sourceDocument, datasetId, documentId);
          if (source?.editable && source.status === "completed" && source.currentVersion >= candidateVersion) {
            patchView(requestScope, {
              phase: "success",
              source,
              draft: source.content,
              pending: null,
              progress: null,
              pollPaused: false,
            });
            onPublishedRef.current?.();
            return;
          }
        }
        patchView(requestScope, { progress: document.progress ?? null });
      } catch {
        if (controller.signal.aborted || !isCurrent(requestScope, requestGeneration)) return;
      }
      if (Date.now() >= deadline) {
        patchView(requestScope, { pollPaused: true });
        return;
      }
      timer = setTimeout(() => { void checkStatus(); }, 1000);
    }

    void checkStatus();
    return () => {
      controller.abort();
      if (timer !== undefined) clearTimeout(timer);
    };
  }, [datasetId, documentId, isCurrent, open, ownedView.pending, ownedView.phase, ownedView.pollPaused, patchView, scope]);

  async function handleSave() {
    if (!open || ownedView.phase !== "idle" || !ownedView.source
      || ownedView.draft === ownedView.source.content || !ownedView.draft.trim()) return;
    const requestScope = scope;
    const requestGeneration = scopeRef.current.generation;
    const source = ownedView.source;
    const submitted = ownedView.draft;
    patchView(requestScope, { phase: "saving", progress: null });
    try {
      const result = await replaceDocumentContent(datasetId, documentId, {
        content: submitted,
        expected_current_version: source.currentVersion,
      });
      if (!isCurrent(requestScope, requestGeneration)) return;
      if (result.document_id !== documentId) {
        patchView(requestScope, { phase: "unknown" });
        return;
      }
      if (result.status === "unchanged" && result.current_version === source.currentVersion) {
        patchView(requestScope, { phase: "unchanged", pending: null });
        return;
      }
      const previousVersion = result.previous_version;
      const candidateVersion = result.candidate_version;
      if (result.status !== "pending" || previousVersion !== source.currentVersion
        || typeof candidateVersion !== "number" || !Number.isSafeInteger(candidateVersion)
        || candidateVersion <= source.currentVersion) {
        patchView(requestScope, { phase: "unknown" });
        return;
      }
      patchView(requestScope, {
        phase: "processing",
        pending: { previousVersion, candidateVersion },
        progress: null,
        pollPaused: false,
      });
      onPublishedRef.current?.();
    } catch (error) {
      if (!isCurrent(requestScope, requestGeneration)) return;
      patchView(requestScope, {
        phase: responseStatus(error) === 409 ? "conflict" : responseStatus(error) === null ? "unknown" : "failed",
        pending: null,
      });
    }
  }

  function handleContentChange(value: string) {
    const phase = ownedView.phase === "success" || ownedView.phase === "unchanged" ? "idle" : ownedView.phase;
    patchView(scope, { draft: value, phase });
  }

  function handleOpenChange(next: boolean) {
    if (!next && ownedView.phase === "saving") return;
    onOpenChange(next);
  }

  const busy = ownedView.phase === "loading" || ownedView.phase === "saving";
  const showReload = ["load_error", "not_editable", "blocked", "failed", "conflict", "unknown"].includes(ownedView.phase);
  const alertKey = ownedView.phase === "load_error" ? "loadFailed"
    : ownedView.phase === "not_editable" ? "notEditable"
      : ownedView.phase === "blocked" ? "alreadyProcessing"
        : ownedView.phase === "failed" ? "failed"
          : ownedView.phase === "conflict" ? "conflict"
            : ownedView.phase === "unknown" ? "outcomeUnknown"
              : null;

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="max-w-3xl">
        <DialogHeader>
          <DialogTitle>{t("knowledge.documentTextEditor.title")}</DialogTitle>
          <DialogDescription>
            {t("knowledge.documentTextEditor.description", { title: documentTitle })}
          </DialogDescription>
        </DialogHeader>

        {busy ? (
          <div role="status" className="flex items-center gap-2 py-8 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
            {t(ownedView.phase === "saving" ? "knowledge.documentTextEditor.saving" : "knowledge.documentTextEditor.loading")}
          </div>
        ) : (
          <div className="space-y-3">
            {ownedView.source && (
              <div className="flex items-center justify-between text-xs text-muted-foreground">
                <Label htmlFor="document-text-source-content">
                  {t("knowledge.documentTextEditor.contentLabel")}
                </Label>
                <span>{t("knowledge.documentTextEditor.currentVersion", { version: ownedView.source.currentVersion })}</span>
              </div>
            )}
            {ownedView.source && (
              <Textarea
                id="document-text-source-content"
                data-testid="document-text-source-content"
                value={ownedView.draft}
                rows={18}
                disabled={ownedView.phase !== "idle" && ownedView.phase !== "success" && ownedView.phase !== "unchanged"}
                className="min-h-[45vh] font-mono text-sm"
                onChange={(event) => handleContentChange(event.target.value)}
              />
            )}
            {ownedView.phase === "idle" && ownedView.draft.trim().length === 0 && (
              <p role="alert" className="text-sm text-destructive">{t("knowledge.documentTextEditor.contentRequired")}</p>
            )}
            {alertKey && (
              <p role="alert" className="rounded-md border border-destructive/20 bg-destructive/5 p-3 text-sm text-destructive">
                {t(`knowledge.documentTextEditor.${alertKey}`)}
              </p>
            )}
            {ownedView.phase === "processing" && (
              <div role="status" className="space-y-2 rounded-md border border-primary/20 bg-primary/5 p-3 text-sm">
                <p>{t("knowledge.documentTextEditor.processing", { version: ownedView.pending?.candidateVersion })}</p>
                {ownedView.progress !== null && ownedView.progress !== undefined && (
                  <p className="text-xs text-muted-foreground">
                    {t("knowledge.documentTextEditor.processingProgress", { progress: Math.round(ownedView.progress) })}
                  </p>
                )}
                {ownedView.pollPaused && (
                  <Button size="sm" variant="outline" onClick={() => patchView(scope, { pollPaused: false })}>
                    {t("knowledge.documentTextEditor.checkStatus")}
                  </Button>
                )}
              </div>
            )}
            {ownedView.phase === "success" && (
              <p role="status" className="rounded-md border border-emerald-500/20 bg-emerald-500/5 p-3 text-sm text-emerald-700 dark:text-emerald-300">
                {t("knowledge.documentTextEditor.success", { version: ownedView.source?.currentVersion })}
              </p>
            )}
            {ownedView.phase === "unchanged" && (
              <p role="status" className="text-sm text-muted-foreground">{t("knowledge.documentTextEditor.unchanged")}</p>
            )}
            {showReload && (
              <Button variant="outline" onClick={() => void loadCurrentSource()}>
                {t(ownedView.phase === "unknown" ? "knowledge.documentTextEditor.checkStatus" : "knowledge.documentTextEditor.reloadSource")}
              </Button>
            )}
          </div>
        )}

        <DialogFooter>
          <Button variant="outline" disabled={ownedView.phase === "saving"} onClick={() => handleOpenChange(false)}>
            {t("knowledge.documentTextEditor.close")}
          </Button>
          <Button
            data-testid="document-text-source-save"
            disabled={ownedView.phase !== "idle" || !ownedView.source
              || ownedView.draft === ownedView.source.content || !ownedView.draft.trim()}
            onClick={() => void handleSave()}
          >
            {t("knowledge.documentTextEditor.save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
