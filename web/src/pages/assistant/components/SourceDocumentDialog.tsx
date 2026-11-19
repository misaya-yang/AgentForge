import type { RequestQuery } from "@/api/contracts";
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { getActiveDocumentSource } from "@/api/knowledge";
import { api } from "@/lib/api";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import type { CitationSource } from "./citationSource";

interface SourceDocumentDialogProps {
  source: CitationSource | null;
  onClose: () => void;
}

interface HistoricalSourceResponse {
  document_id: string;
  version_number: number;
  title?: string;
  content: string;
}

async function contentHash(content: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(content));
  return Array.from(new Uint8Array(digest), (value) => value.toString(16).padStart(2, "0")).join("");
}

/** Recheck current access, then show only the exact saved source version. */
export function SourceDocumentDialog({ source, onClose }: SourceDocumentDialogProps) {
  const { t } = useTranslation();
  const token = useAuthStore((auth) => auth.token);
  const user = useAuthStore((auth) => auth.user);
  const userId = user?.user_id;
  const authority = sdkAuthorityScope(user);
  // A transient identity fences account, permission and session changes without
  // putting credentials into a key, URL or persisted source reference.
  const access = useMemo(() => ({ authenticated: Boolean(token && userId), authority }), [token, userId, authority]);
  const [view, setView] = useState<{
    key: string; access: typeof access;
    state: "verified" | "excerpt" | "unavailable";
    title?: string; content?: string | null;
  } | null>(null);
  const datasetId = source?.datasetId;
  const documentId = source?.documentId;
  const sourceVersion = source?.sourceVersion;
  const sourceHash = source?.sourceHash;
  const provenance = source?.provenance;
  const sourceKey = JSON.stringify([datasetId, documentId, provenance, sourceVersion, sourceHash]);
  const currentView = view?.key === sourceKey && view.access === access ? view : null;
  const state = currentView?.state ?? "loading";
  const currentDocument = state === "verified" || state === "excerpt" ? currentView : null;

  useEffect(() => {
    if (!access.authenticated || !datasetId || !documentId || provenance === "invalid") {
      setView({ key: sourceKey, access, state: "unavailable" });
      return;
    }
    let active = true;
    const controller = new AbortController();
    let requestNumber = 0;
    const load = async () => {
      const currentRequest = ++requestNumber;
      try {
        if (provenance === "versioned") {
          if (!Number.isInteger(sourceVersion) || !sourceVersion || sourceVersion <= 0
            || !sourceHash || !/^[0-9a-f]{64}$/.test(sourceHash)) {
            throw new Error("Invalid saved source identity");
          }
          const { data } = await api.get<HistoricalSourceResponse>(
            `/api/v1/knowledge/${encodeURIComponent(datasetId)}/documents/${encodeURIComponent(documentId)}`
              + `/versions/${sourceVersion}/source`,
            { params: ({ hash: sourceHash } satisfies RequestQuery<"GET /api/v1/knowledge/{dataset_id}/documents/{document_id}/versions/{version_number}/source">), signal: controller.signal },
          );
          if (data.document_id !== documentId || data.version_number !== sourceVersion
            || typeof data.content !== "string" || await contentHash(data.content) !== sourceHash) {
            throw new Error("Historical source identity mismatch");
          }
          if (!active || currentRequest !== requestNumber) return;
          setView({ key: sourceKey, access, state: "verified", title: data.title || "", content: data.content });
          return;
        }
        // Old events have no immutable version. Check current rights, but do
        // not display the potentially newer body returned by this endpoint.
        const result = await getActiveDocumentSource(datasetId, documentId);
        if (!active || currentRequest !== requestNumber) return;
        setView({ key: sourceKey, access, state: "excerpt", title: result.title, content: null });
      } catch {
        if (!active || currentRequest !== requestNumber) return;
        setView({ key: sourceKey, access, state: "unavailable" });
      }
    };
    setView(null);
    void load();
    const interval = window.setInterval(() => void load(), 3000);
    return () => {
      active = false;
      controller.abort();
      window.clearInterval(interval);
    };
  }, [datasetId, documentId, provenance, sourceVersion, sourceHash, sourceKey, access]);

  return (
    <Dialog open={Boolean(source)} onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent className="max-w-2xl">
        <DialogTitle>{currentDocument?.title || t("assistant.openSource", "Open source")}</DialogTitle>
        <DialogDescription>
          {state === "verified"
            ? t("assistant.historicalSourceVerified", "Saved source version verified against this citation and your current access.")
            : t("assistant.sourceCurrentAccess", "This document is checked against your current access.")}
        </DialogDescription>
        {(state === "verified" || state === "excerpt") && currentDocument && source?.excerpt && (
          <section className="space-y-1 rounded-lg border border-[hsl(var(--assistant-border-soft))] p-3">
            <p className="text-xs font-medium">{t("assistant.citedExcerpt", "Excerpt saved with this answer")}</p>
            <p className="max-h-40 overflow-y-auto whitespace-pre-wrap break-words text-sm">{source.excerpt}</p>
          </section>
        )}
        {state === "loading" && <p role="status">{t("common.loading", "Loading...")}</p>}
        {state === "unavailable" && <p role="alert">{t("assistant.sourceUnavailable", "This source is no longer available to you. Ask the owner for access or choose another source.")}</p>}
        {state === "excerpt" && currentDocument && (
          <p role="note" className="text-xs text-amber-700 dark:text-amber-300">
            {t("assistant.historicalSourceUnverified", "Only the excerpt saved with this answer is available. Its historical full document version cannot be verified.")}
          </p>
        )}
        {state === "verified" && currentDocument && (
          <section className="space-y-2">
            <p role="note" className="text-xs text-[hsl(var(--assistant-text-secondary))]">
              {t("assistant.savedSourceVersion", "Saved source version")} #{sourceVersion}
            </p>
            <div className="max-h-[45dvh] overflow-y-auto whitespace-pre-wrap break-words text-sm" data-testid="assistant-source-content">
              {currentDocument.content || t("assistant.emptySource", "This document has no previewable text.")}
            </div>
          </section>
        )}
      </DialogContent>
    </Dialog>
  );
}
