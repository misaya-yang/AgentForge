import { useCallback, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listDocumentVersions, compareDocumentVersions, restoreDocumentVersion, type DocumentVersion, type VersionCompareResponse } from "@/api/knowledge";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import type { CitationSource } from "@/pages/assistant/components/citationSource";
import { toast } from "@/hooks/use-toast";

export const VERSION_PAGE_SIZE = 20;
type Scope = { datasetId: string; documentId: string; open: boolean; token: string | null; authority: string; epoch: number; retired: boolean };
type Page = { scope: Scope; offset: number; versions: DocumentVersion[]; total: number; currentVersion: number; loading: boolean; error: boolean };

export function useDocumentVersionNavigation({ datasetId, documentId, open, onOpenChange, onRestored }: {
  datasetId: string; documentId: string; open: boolean; onOpenChange: (open: boolean) => void; onRestored?: () => void;
}) {
  const { t } = useTranslation();
  const token = useAuthStore(state => state.token);
  const authority = useAuthStore(state => sdkAuthorityScope(state.user));
  const [epoch, setEpoch] = useState(0);
  const scope = useMemo<Scope>(() => ({ datasetId, documentId, open, token, authority, epoch, retired: false }), [datasetId, documentId, open, token, authority, epoch]);
  const live = useRef(scope); live.current = scope;
  const request = useRef(0); const comparison = useRef(0); const restoreBusy = useRef(false);
  const [page, setPage] = useState<Page | null>(null);
  const [selection, setSelection] = useState<{ scope: Scope; items: DocumentVersion[] }>({ scope, items: [] });
  const [compareModeState, setCompareModeState] = useState(false);
  const [result, setResult] = useState<{ scope: Scope; value: VersionCompareResponse } | null>(null);
  const [comparing, setComparing] = useState(false);
  const [confirmation, setConfirmation] = useState<{ scope: Scope; version: DocumentVersion; request: number } | null>(null);
  const [restoring, setRestoring] = useState(false);
  const previewEpoch = useRef(0);
  const [preview, setPreview] = useState<{ scope: Scope; request: number; source: CitationSource } | null>(null);
  const isCurrent = useCallback(() => {
    const auth = useAuthStore.getState();
    return live.current === scope && !scope.retired && scope.open && Boolean(scope.datasetId && scope.documentId && auth.isAuthenticated && auth.token && auth.user)
      && auth.token === scope.token && sdkAuthorityScope(auth.user) === scope.authority;
  }, [scope]);

  const loadPage = useCallback(async (offset: number) => {
    if (!isCurrent() || restoreBusy.current) return;
    setPreview(null);
    const ticket = ++request.current; comparison.current++; setResult(null); setComparing(false); setConfirmation(null);
    setPage({ scope, offset, versions: [], total: 0, currentVersion: 0, loading: true, error: false });
    try {
      let actualOffset = offset;
      let data = await listDocumentVersions(datasetId, documentId, VERSION_PAGE_SIZE, actualOffset);
      if (!isCurrent() || request.current !== ticket) return;
      if (!Number.isInteger(data.total) || data.total < 0) throw new Error("Invalid version total");
      // One bounded correction handles a deleted/shrunk last page. It is not a historical snapshot guarantee.
      if (actualOffset > 0 && actualOffset >= data.total) {
        actualOffset = Math.max(0, Math.floor((data.total - 1) / VERSION_PAGE_SIZE) * VERSION_PAGE_SIZE);
        data = await listDocumentVersions(datasetId, documentId, VERSION_PAGE_SIZE, actualOffset);
        if (!isCurrent() || request.current !== ticket) return;
      }
      if (!Number.isInteger(data.total) || data.total < 0 || !Array.isArray(data.versions) || data.versions.length > VERSION_PAGE_SIZE
        || !Number.isInteger(data.current_version) || data.current_version < 1
        || (actualOffset > 0 && actualOffset >= data.total) || actualOffset + data.versions.length > data.total
        || (data.total > actualOffset && data.versions.length === 0) || data.versions.some(v => v.document_id !== documentId || !v.version_id || !Number.isInteger(v.version_number) || v.version_number < 1)) throw new Error("Invalid version page");
      setPage({ scope, offset: actualOffset, versions: data.versions, total: data.total, currentVersion: data.current_version, loading: false, error: false });
    } catch {
      if (!isCurrent() || request.current !== ticket) return;
      setPage({ scope, offset, versions: [], total: 0, currentVersion: 0, loading: false, error: true });
      setSelection({ scope, items: [] });
    }
  }, [datasetId, documentId, isCurrent, scope]);

  useLayoutEffect(() => {
    scope.retired = false;
    const unsubscribe = useAuthStore.subscribe((state, prior) => {
      if (state.token !== prior.token || state.isAuthenticated !== prior.isAuthenticated || sdkAuthorityScope(state.user) !== sdkAuthorityScope(prior.user)) {
        scope.retired = true; setEpoch(value => value + 1);
      }
    });
    setCompareModeState(false); setSelection({ scope, items: [] }); setConfirmation(null); setResult(null); setComparing(false); setRestoring(false); restoreBusy.current = false;
    void loadPage(0);
    return () => { scope.retired = true; unsubscribe(); };
  }, [loadPage, scope]);

  const ownedPage = page?.scope === scope ? page : null;
  const selected = selection.scope === scope ? selection.items : [];
  const actionable = isCurrent() && Boolean(ownedPage && !ownedPage.loading && !ownedPage.error);
  const renderedRequest = request.current; const renderedPreview = previewEpoch.current;
  const selectedForCompare = selected.map(item => item.version_number);
  function handleSelectForCompare(versionNumber: number) {
    if (!isCurrent() || request.current !== renderedRequest || !actionable || restoreBusy.current || comparing) return;
    const version = ownedPage?.versions.find(item => item.version_number === versionNumber);
    if (!version) return;
    const already = selected.some(item => item.version_id === version.version_id);
    setSelection({ scope, items: already ? selected.filter(item => item.version_id !== version.version_id) : [...selected.slice(-1), version] });
    comparison.current++; setResult(null);
  }
  async function handleCompare() {
    if (!isCurrent() || request.current !== renderedRequest || !actionable || selected.length !== 2 || restoreBusy.current) return;
    const ticket = ++comparison.current; const pageTicket = request.current;
    const [from, to] = [...selected].sort((a, b) => a.version_number - b.version_number);
    setComparing(true); setResult(null); setConfirmation(null);
    try {
      const value = await compareDocumentVersions(datasetId, documentId, from.version_number, to.version_number);
      if (!isCurrent() || comparison.current !== ticket || request.current !== pageTicket) return;
      if (value.from_version !== from.version_number || value.to_version !== to.version_number) throw new Error("Wrong comparison pair");
      setResult({ scope, value });
    } catch {
      if (isCurrent() && comparison.current === ticket && request.current === pageTicket) toast({ title: t("knowledge.versionHistory.compareFailed"), description: t("knowledge.versionHistory.pageReadFailed"), variant: "destructive" });
    } finally { if (isCurrent() && comparison.current === ticket) setComparing(false); }
  }
  function setRestoreVersion(versionNumber: number | null) {
    if (!isCurrent() || request.current !== renderedRequest || restoreBusy.current) return;
    const version = actionable ? ownedPage?.versions.find(item => item.version_number === versionNumber) : undefined;
    previewEpoch.current++; setPreview(null);
    setConfirmation(version ? { scope, version, request: request.current } : null);
  }
  async function handleRestore() {
    if (!isCurrent() || request.current !== renderedRequest || !actionable || !confirmation || confirmation.scope !== scope || confirmation.request !== request.current || restoreBusy.current) return;
    restoreBusy.current = true; setRestoring(true);
    try {
      await restoreDocumentVersion(datasetId, documentId, confirmation.version.version_number);
      if (!isCurrent()) return;
      toast({ title: t("knowledge.versionHistory.restoreQueued"), description: t("knowledge.versionHistory.restorePending", { version: confirmation.version.version_number }) });
      setConfirmation(null); onDialogOpenChange(false); onRestored?.();
    } catch (error) {
      if (!isCurrent()) return;
      const detail = (error as { response?: { data?: { detail?: { code?: string } } } })?.response?.data?.detail;
      const specialized = detail?.code === "specialized_rebuild_unavailable";
      toast({ title: t(specialized ? "knowledge.detail.specializedRebuildUnavailableTitle" : "knowledge.versionHistory.restoreFailed"), description: t(specialized ? "knowledge.detail.specializedRebuildUnavailableDetail" : "knowledge.versionHistory.restoreFailureNext", { documentId }), variant: "destructive" });
    } finally { if (isCurrent()) { restoreBusy.current = false; setRestoring(false); } }
  }
  function onDialogOpenChange(value: boolean) {
    if (!isCurrent()) return;
    if (!value) { scope.retired = true; request.current++; comparison.current++; }
    onOpenChange(value);
  }
  function setCompareMode(value: boolean) {
    if (!isCurrent() || request.current !== renderedRequest || !actionable || restoreBusy.current) return;
    previewEpoch.current++; setPreview(null);
    comparison.current++; setCompareModeState(value); setSelection({ scope, items: [] }); setResult(null); setComparing(false); setConfirmation(null);
  }
  function dismissComparison() { if (isCurrent()) { comparison.current++; setResult(null); setComparing(false); } }
  function canPreviewVersion(version: DocumentVersion) {
    return actionable && !restoring && !comparing && !compareModeState
      && typeof version.content_hash === "string" && /^[0-9a-f]{64}$/i.test(version.content_hash)
      && ["created", "updated", "restored", "deleted"].includes(version.change_type);
  }
  function previewVersion(versionNumber: number) {
    if (!isCurrent() || request.current !== renderedRequest || previewEpoch.current !== renderedPreview || restoreBusy.current) return;
    const version = ownedPage?.versions.find(item => item.version_number === versionNumber);
    if (!version || !canPreviewVersion(version)) return;
    setPreview({ scope, request: request.current, source: { datasetId, documentId, provenance: "versioned",
      sourceVersion: version.version_number, sourceHash: version.content_hash.toLowerCase(), excerpt: "" } });
  }
  const sourcePreview = actionable && !restoring && !compareModeState && preview?.scope === scope
    && preview.request === request.current ? preview.source : null;
  function closeSourcePreview() { if (isCurrent() && request.current === renderedRequest) { previewEpoch.current++; setPreview(null); } }
  return { sourcePreview, previewVersion, canPreviewVersion, closeSourcePreview, versions: actionable ? ownedPage!.versions : [], total: ownedPage?.total ?? 0, currentVersion: ownedPage?.currentVersion ?? 0,
    offset: ownedPage?.offset ?? 0, loading: !ownedPage || ownedPage.loading, loadError: ownedPage?.error ?? false,
    loadPage: (offset: number) => { if (request.current === renderedRequest) return loadPage(offset); }, onDialogOpenChange, compareMode: compareModeState, setCompareMode, selectedForCompare, handleSelectForCompare,
    compareResult: result?.scope === scope ? result.value : null, dismissComparison, comparing, handleCompare,
    restoreVersion: confirmation?.scope === scope ? confirmation.version.version_number : null, setRestoreVersion, restoring, handleRestore };
}
