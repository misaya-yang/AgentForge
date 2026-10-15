/**
 * Dataset detail page shell (C2 split of the ~3900-line monolith).
 *
 * Keeps only dataset loading, `?tab=` routing, header/tab bar, error banner,
 * dialog open state, and the shared hit-test bundle for Retrieval/QA tabs.
 * documents/retrieval/qa/settings/permissions stay mounted with visibility
 * toggled via `hidden` so local state survives tab switches as before;
 * eval and sources keep their original conditional mount/unmount.
 */

import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import {
  ArrowLeft,
  RefreshCcw,
  Trash2,
  Search,
  FileText,
  Edit3,
  MessageSquare,
  Sliders,
  Globe,
  Lock,
  Users,
  Cloud,
  FlaskConical,
  Activity,
} from "lucide-react";

import { useDataset, useDatasetSources, useDocuments } from "@/hooks/useKnowledge";

import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

import { useHitTestConsole } from "@/pages/knowledge/detail/useHitTestConsole";
import { useDatasetUploadDialog } from "@/pages/knowledge/detail/useDatasetUploadDialog";
import { DocumentsTab } from "@/pages/knowledge/detail/DocumentsTab";
import { RetrievalTab } from "@/pages/knowledge/detail/RetrievalTab";
import { QATab } from "@/pages/knowledge/detail/QATab";
import { SettingsTab } from "@/pages/knowledge/detail/SettingsTab";
import { PermissionsTab } from "@/pages/knowledge/detail/PermissionsTab";
import { RetrievalEvalWorkbench } from "@/pages/knowledge/detail/RetrievalEvalWorkbench";
import { QueryObservabilityTab } from "@/pages/knowledge/detail/QueryObservabilityTab";
import {
  CreateTextDocumentDialog,
  CreateUrlDocumentDialog,
  DeleteDatasetDialog,
  EditDatasetDialog,
} from "@/pages/knowledge/detail/DatasetDialogs";
import { SourcesTab } from "@/pages/knowledge/sources";
import { useAuthStore, usePermission } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { queryDiagnosticTraceUrl, type QueryDiagnosticHandoff } from "./detail/queryDiagnosticHandoff";

type DatasetMainTab =
  | "documents"
  | "retrieval"
  | "eval"
  | "qa"
  | "queries"
  | "sources"
  | "settings"
  | "permissions";

const DATASET_MAIN_TABS: DatasetMainTab[] = [
  "documents",
  "retrieval",
  "eval",
  "qa",
  "queries",
  "sources",
  "settings",
  "permissions",
];

function getDatasetMainTab(searchParams: URLSearchParams): DatasetMainTab {
  const requestedTab = searchParams.get("tab");
  return DATASET_MAIN_TABS.includes(requestedTab as DatasetMainTab)
    ? (requestedTab as DatasetMainTab)
    : "documents";
}

export function KnowledgeDatasetDetailPage() {
  const { datasetId } = useParams();
  const nav = useNavigate();
  const qc = useQueryClient();
  const { t } = useTranslation();

  const dsQuery = useDataset(datasetId);
  const dataset = dsQuery.data;
  const {
    fileInputRef,
    uploading,
    openFilePicker,
    handleFilesSelected,
    dialog: uploadDialog,
  } = useDatasetUploadDialog({ datasetId, dataset });
  const [documentOffset, setDocumentOffset] = useState(0);
  const documentLimit = 50;
  const docsQuery = useDocuments(datasetId, {
    limit: documentLimit,
    offset: documentOffset,
  });
  const docs = useMemo(() => docsQuery.data?.items ?? [], [docsQuery.data?.items]);
  const documentTotal = docsQuery.data?.total ?? 0;
  const sourcesQuery = useDatasetSources(datasetId);

  const [searchParams, setSearchParams] = useSearchParams();
  const [mainTab, setMainTab] = useState<DatasetMainTab>(() =>
    getDatasetMainTab(searchParams)
  );
  const [settingsUnsaved, setSettingsUnsaved] = useState(false);

  useEffect(() => {
    const nextTab = getDatasetMainTab(searchParams);
    setMainTab((currentTab) => currentTab === nextTab ? currentTab : nextTab);
  }, [searchParams, setSearchParams]);

  function handleMainTabChange(tab: DatasetMainTab) {
    if (tab !== "settings" && mainTab === "settings" && settingsUnsaved
      && !window.confirm(t("knowledge.detail.unsavedConfigLeave"))) return;
    const nextParams = new URLSearchParams(searchParams);
    nextParams.set("tab", tab);
    if (tab !== "sources") {
      nextParams.delete("source");
      nextParams.delete("binding");
    }
    setMainTab(tab);
    setSearchParams(nextParams, { replace: true });
  }

  // Page-level dialog open state (form state lives inside each dialog).
  const [textDialogOpen, setTextDialogOpen] = useState(false);
  const [urlDialogOpen, setUrlDialogOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [deleteConfirmOpen, setDeleteConfirmOpen] = useState(false);

  // Shared retrieval hit-test console (Retrieval tab + QA tab).
  const authority = useAuthStore(state => sdkAuthorityScope(state.user));
  const token = useAuthStore(state => state.token);
  const canViewTrace = usePermission("console:eval:view");
  const observationPermission = !dsQuery.isError && dataset?.dataset_id === datasetId ? dataset?.my_permission : undefined;
  const [observationAccessLost, setObservationAccessLost] = useState(false);
  const observationAccess = useRef(true);
  useEffect(() => {
    observationAccess.current = true;
    setObservationAccessLost(false);
  }, [datasetId, authority, token]);
  const [authorityEpoch, setAuthorityEpoch] = useState(0);
  const handoffScope = useMemo(() => {
    const lifecycle = { retired: false };
    return {
      datasetId, authority, token, permission: observationPermission, accessLost: observationAccessLost, authorityEpoch,
      retire: () => { lifecycle.retired = true; },
      isCurrent: () => !lifecycle.retired && observationAccess.current && useAuthStore.getState().token === token
        && sdkAuthorityScope(useAuthStore.getState().user) === authority,
    };
  }, [datasetId, authority, token, observationPermission, observationAccessLost, authorityEpoch]);
  useEffect(() => useAuthStore.subscribe(() => {
    const current = useAuthStore.getState();
    if (current.token !== handoffScope.token || sdkAuthorityScope(current.user) !== handoffScope.authority) {
      handoffScope.retire();
      setAuthorityEpoch(value => value + 1);
    }
  }), [handoffScope]);
  const hitTest = useHitTestConsole(datasetId, handoffScope);
  const currentHandoffScope = useRef(handoffScope);
  currentHandoffScope.current = handoffScope;
  const deliveredScope = useRef<typeof handoffScope | null>(null);
  const [queryHandoff, setQueryHandoff] = useState<{
    scope: typeof handoffScope; observation: QueryDiagnosticHandoff;
  } | null>(null);

  useEffect(() => {
    if (deliveredScope.current && deliveredScope.current !== handoffScope) {
      hitTest.invalidateHitTestResults();
      hitTest.setQuery("");
      setQueryHandoff(null);
      deliveredScope.current = null;
    }
    // Clear only the draft delivered by this workflow when its authority changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [handoffScope]);

  function testObservedQuestion(observation: QueryDiagnosticHandoff) {
    if (currentHandoffScope.current !== handoffScope || !handoffScope.isCurrent() || !token || !datasetId
      || (observationPermission !== "owner" && observationPermission !== "editor")
      || observation.datasetId !== datasetId || !observation.question.trim()) return;
    if (hitTest.query.trim() && hitTest.query.trim() !== observation.question
      && !window.confirm(t("knowledge.observability.replaceDraft"))) return;
    if (currentHandoffScope.current !== handoffScope || !handoffScope.isCurrent()) return;
    hitTest.invalidateHitTestResults();
    hitTest.setQuery(observation.question);
    deliveredScope.current = handoffScope;
    setQueryHandoff({ scope: handoffScope, observation });
    handleMainTabChange("retrieval");
  }

  const currentObservation = queryHandoff?.scope === handoffScope
    && queryHandoff.observation.question === hitTest.query ? queryHandoff.observation : null;
  const observedTraceUrl = currentObservation ? queryDiagnosticTraceUrl(currentObservation) : null;

  const visibilityIcons: Record<string, ReactNode> = {
    private: <Lock className="h-4 w-4" />,
    tenant: <Users className="h-4 w-4" />,
    public: <Globe className="h-4 w-4" />,
  };

  const tabStyles = {
    documents: "border-primary text-primary bg-primary/10",
    retrieval: "border-primary text-primary bg-primary/10",
    qa: "border-primary text-primary bg-primary/10",
    queries: "border-primary text-primary bg-primary/10",
    sources: "border-primary text-primary bg-primary/10",
    settings: "border-primary text-primary bg-primary/10",
    eval: "border-primary text-primary bg-primary/10",
    permissions: "border-primary text-primary bg-primary/10",
  } as const;

  const tabIconStyles = {
    documents: "text-primary",
    retrieval: "text-primary",
    qa: "text-primary",
    queries: "text-primary",
    eval: "text-primary",
    sources: "text-primary",
    settings: "text-primary",
    permissions: "text-primary",
  } as const;

  return (
    <div className="min-h-full bg-background">
      {/* 顶部导航栏 */}
      <div className="bg-card border-b border-border sticky top-0 z-20">
        <div className="max-w-[1600px] mx-auto px-4 sm:px-6">
          <div className="flex min-h-14 items-center justify-between gap-3 py-2">
            {/* 左侧：面包屑导航 */}
            <div className="flex min-w-0 items-center gap-2 sm:gap-3">
              <button
                onClick={() => {
                  if (settingsUnsaved && !window.confirm(t("knowledge.detail.unsavedConfigLeave"))) return;
                  nav("/knowledge");
                }}
                className="shrink-0 text-primary hover:text-primary/90 font-medium text-sm flex items-center gap-1"
              >
                <ArrowLeft className="h-4 w-4" />
                <span className="hidden sm:inline">{t("knowledge.detail.knowledgeBase")}</span>
              </button>
              <span className="hidden text-muted-foreground/70 sm:inline">/</span>
              <span className="truncate font-semibold text-foreground">{dataset?.name || t("knowledge.detail.loading")}</span>
              {dataset?.visibility && (
                <Badge variant="outline" className="hidden text-xs bg-muted/40 text-muted-foreground border-border sm:flex items-center gap-1">
                  {visibilityIcons[dataset.visibility]}
                  <span>{dataset.visibility === "private" ? t("knowledge.detail.visPrivate") : dataset.visibility === "tenant" ? t("knowledge.detail.visTenant") : t("knowledge.detail.visPublic")}</span>
                </Badge>
              )}
            </div>

            {/* 右侧：操作按钮 */}
            <div className="flex shrink-0 items-center gap-2 sm:gap-3">
              <Button
                variant="outline"
                size="icon"
                onClick={() => {
                  qc.invalidateQueries({ queryKey: ["kb-documents", datasetId] });
                  qc.invalidateQueries({ queryKey: ["kb-dataset", datasetId] });
                }}
                className="h-9 w-9 bg-card"
                title={t("knowledge.detail.refreshData")}
              >
                <RefreshCcw className={`h-4 w-4 ${docsQuery.isFetching ? "animate-spin" : ""}`} />
              </Button>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button variant="primary">
                    <Edit3 className="h-4 w-4 mr-1.5" />
                    <span className="hidden sm:inline">{t("knowledge.detail.edit")}</span>
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="w-48">
                  <DropdownMenuItem onClick={() => setSettingsOpen(true)} className="cursor-pointer">
                    <Edit3 className="h-4 w-4 mr-2" />
                    {t("knowledge.detail.editInfo")}
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem
                    className="text-red-600 focus:text-red-600 cursor-pointer"
                    onClick={() => {
                      setDeleteConfirmOpen(true);
                    }}
                  >
                    <Trash2 className="h-4 w-4 mr-2" />
                    {t("knowledge.detail.deleteKB")}
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
          </div>

          {/* Tabs */}
          <div className="ui-tabs-rail -mb-px mt-1" role="tablist" aria-label={t("knowledge.detail.knowledgeBase")}>
            {[
              { key: "documents", label: t("knowledge.detail.tabDocuments"), icon: FileText },
              { key: "retrieval", label: t("knowledge.detail.tabRetrieval"), icon: Search },
              { key: "eval", label: t("knowledge.detail.tabEval"), icon: FlaskConical },
              { key: "qa", label: t("knowledge.detail.tabQA"), icon: MessageSquare },
              { key: "queries", label: t("knowledge.detail.tabQueries"), icon: Activity },
              { key: "sources", label: t("knowledge.detail.tabSources"), icon: Cloud },
              { key: "settings", label: t("knowledge.detail.tabSettings"), icon: Sliders },
              { key: "permissions", label: t("knowledge.detail.tabPermissions"), icon: Lock },
            ].map((tab) => (
              <button
                key={tab.key}
                role="tab"
                aria-selected={mainTab === tab.key}
                onClick={() => handleMainTabChange(tab.key as DatasetMainTab)}
                className={`
                  group flex shrink-0 items-center gap-2 px-4 py-3 text-sm font-medium border-b-2 transition-colors duration-200 sm:px-5 sm:py-3.5
                  ${mainTab === tab.key
                    ? tabStyles[tab.key as keyof typeof tabStyles]
                    : "border-transparent text-muted-foreground hover:text-foreground hover:bg-muted/40"
                  }
                `}
              >
                <tab.icon className={`h-4 w-4 transition-transform group-hover:scale-110 ${mainTab === tab.key ? tabIconStyles[tab.key as keyof typeof tabIconStyles] : ""
                  }`} />
                {tab.label}
              </button>
            ))}
          </div>
        </div>
      </div>

      {/* 主内容区 */}
      <div className="max-w-[1600px] mx-auto px-4 py-4 sm:px-6 sm:py-6">
        {(dsQuery.isError || docsQuery.isError) && (
          <div role="alert" className="mb-4 flex flex-col gap-3 rounded-xl border border-destructive/25 bg-destructive/5 p-4 text-sm sm:flex-row sm:items-center sm:justify-between">
            <span>{t("common.loadFailed")}</span>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                void dsQuery.refetch();
                void docsQuery.refetch();
              }}
            >
              {t("common.retry", { defaultValue: "Retry" })}
            </Button>
          </div>
        )}

        {/* 文档管理 Tab（常驻挂载，隐藏切换以保留状态） */}
        <div className={mainTab === "documents" ? undefined : "hidden"}>
          <DocumentsTab
            key={datasetId}
            datasetId={datasetId}
            tenantId={dataset?.tenant_id}
            docs={docs}
            docsQuery={docsQuery}
            totalDocuments={documentTotal}
            documentLimit={documentLimit}
            documentOffset={documentOffset}
            onDocumentOffsetChange={setDocumentOffset}
            permission={dataset?.my_permission}
            fileInputRef={fileInputRef}
            uploading={uploading}
            openFilePicker={openFilePicker}
            onFilesSelected={handleFilesSelected}
            onOpenTextDialog={() => setTextDialogOpen(true)}
            onOpenUrlDialog={() => setUrlDialogOpen(true)}
          />
        </div>

        {/* 检索测试 Tab */}
        <div className={mainTab === "retrieval" ? undefined : "hidden"}>
          {currentObservation && (
            <div role="status" data-testid="query-diagnostic-handoff" className="mb-4 space-y-1 rounded-lg border border-border bg-muted/40 p-3 text-sm">
              <p className="font-medium">{t(currentObservation.origin === "feedback"
                ? "knowledge.observability.fromFeedback" : "knowledge.observability.fromQuery")}</p>
              <p className="text-xs text-muted-foreground">{t("knowledge.observability.handoffHint")}</p>
              <p className="text-xs text-muted-foreground">{t("knowledge.observability.sourceVersionsHint")}</p>
              {observedTraceUrl && canViewTrace && <a className="text-xs underline" href={observedTraceUrl}
                onClick={event => { if (currentHandoffScope.current !== handoffScope || !handoffScope.isCurrent()) event.preventDefault(); }}>
                {t("knowledge.observability.openTrace")}
              </a>}
            </div>
          )}
          <RetrievalTab datasetId={datasetId} hitTest={hitTest} />
        </div>

        {/* 检索评测 Tab（自包含组件，保持原条件挂载） */}
        {mainTab === "eval" && (
          <RetrievalEvalWorkbench datasetId={datasetId ?? ""} />
        )}

        {/* QA 测试 Tab */}
        <div className={mainTab === "qa" ? undefined : "hidden"}>
          <QATab datasetId={datasetId} hitTest={hitTest} identityScope={handoffScope} />
        </div>

        {mainTab === "queries" && <QueryObservabilityTab datasetId={datasetId} permission={observationPermission}
          onTestQuestion={testObservedQuestion}
          onAccessChange={available => {
            const current = currentHandoffScope.current;
            if (current.datasetId !== datasetId || current.token !== token || current.authority !== authority
              || current.permission !== observationPermission || useAuthStore.getState().token !== token
              || sdkAuthorityScope(useAuthStore.getState().user) !== authority) return;
            observationAccess.current = available;
            setObservationAccessLost(!available);
            if (!available) {
              hitTest.invalidateHitTestResults();
              if (deliveredScope.current) hitTest.setQuery("");
              setQueryHandoff(null);
              deliveredScope.current = null;
            }
          }}
          onInspect={tab => { if (currentHandoffScope.current === handoffScope && handoffScope.isCurrent() && token) handleMainTabChange(tab); }}
        />}

        {/* 数据来源 Tab（自包含组件，保持原条件挂载） */}
        {mainTab === "sources" && datasetId && (
          <SourcesTab
            datasetId={datasetId}
            onUploadClick={openFilePicker}
            onUrlClick={() => setUrlDialogOpen(true)}
            sources={sourcesQuery.data ?? null}
            sourcesLoading={sourcesQuery.isFetching}
            sourcesFailed={sourcesQuery.isError}
            onRetrySources={() => void sourcesQuery.refetch({ cancelRefetch: false })}
          />
        )}

        {/* 配置 Tab */}
        <div className={mainTab === "settings" ? undefined : "hidden"}>
          <SettingsTab
            key={datasetId}
            datasetId={datasetId}
            dataset={dataset}
            active={mainTab === "settings"}
            permission={dataset?.my_permission}
            onUnsavedChange={setSettingsUnsaved}
            onDatasetRefetch={() => void dsQuery.refetch()}
          />
        </div>

        {/* 权限 Tab */}
        <div className={mainTab === "permissions" ? undefined : "hidden"}>
          <PermissionsTab
            datasetId={datasetId}
            dataset={dataset}
            onDatasetRefetch={() => void dsQuery.refetch()}
          />
        </div>
      </div>

      {/* Dialogs */}
      {uploadDialog}

      <CreateTextDocumentDialog datasetId={datasetId} open={textDialogOpen} onOpenChange={setTextDialogOpen} />
      <CreateUrlDocumentDialog datasetId={datasetId} open={urlDialogOpen} onOpenChange={setUrlDialogOpen} />
      <EditDatasetDialog datasetId={datasetId} dataset={dataset} open={settingsOpen} onOpenChange={setSettingsOpen} />
      <DeleteDatasetDialog datasetId={datasetId} datasetName={dataset?.name} open={deleteConfirmOpen} onOpenChange={setDeleteConfirmOpen} />
    </div>
  );
}
