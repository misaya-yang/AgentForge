import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import { useEvalRunActions } from "./useEvalRunActions";
import { Alert, App as AntApp, Button, Descriptions, Input, InputNumber, Pagination, Progress, Select, Space, Tabs } from "antd";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Activity,
  Beaker,
  CheckCircle2,
  Database,
  Download,
  GitCompare,
  Play,
  SearchCheck,
  ShieldCheck,
  Sparkles,
} from "lucide-react";
import type { ReactNode } from "react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useLocation, useSearchParams } from "react-router-dom";

import { createAgentVersion, listAgentPublications, listAgents, listAgentVersions } from "@/api/agents";
import type { AgentSummary } from "@/types/agents";

import {
  createEvalDataset,
  createEvalEvaluator,
  createEvalExampleFromTrace,
  createEvalExperiment,
  createAgentTraceScore,
  compareEvalExperimentRuns,
  dryRunEvalGate,
  exportAgentTrace,
  exportEvalExamples,
  getAgentTraceDetail,
  getEvalDashboard,
  getEvalExperiment,
  getEvalExperimentRun,
  getEvalExperimentRunResults,
  getEvalSummary,
  importEvalExamples,
  listAllEvalExamples,
  listAgentTraces,
  listAllEvalDatasets,
  listEvalEvaluators,
  listEvalExamples,
  listEvalExperiments,
  promoteEvalExperimentBaseline,
  runEvalExperiment,
  runEvalEvaluatorAsync,
  updateEvalExample,
  type EvalDataset,
  type EvalExample,
  type EvalEvaluator,
  type EvalExperiment,
  type EvalExperimentCaseResult,
  type EvalExperimentRunMode,
  type EvalExperimentRunComparisonResponse,
  type EvalGateDryRunResponse,
  type EvalExamplesExportResponse,
  type EvalReviewStatus,
  type AgentTraceScoreCreate,
  type AgentTraceSummary,
  type ListAgentTracesParams,
  type TraceFamily,
  type TraceStatus,
} from "@/api/eval";
import { useAuthStore, usePermission } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { generateUUID } from "@/lib/utils";

import {
  type AssistantTraceFilters,
} from "./components/AssistantTraceList";
import { GoldenJsonlImport } from "./components/GoldenJsonlImport";
import { BehaviorContractEditor } from "./components/BehaviorContractEditor";
import { ExperimentRunComparison } from "./components/ExperimentRunComparison";
import { ExperimentRunResults } from "./components/ExperimentRunResults";
import { KbRagasPanel } from "./components/KbRagasPanel";
import { TraceExplorerShell } from "./components/TraceExplorerShell";
import { canApproveReviewExample, kbDeepLinkNavigationKey, kbFailureSourceVersions, latestReviewCandidates } from "./reviewQueue";
import { agentVersionTarget, buildExperimentTargetConfig } from "./agentVersionTarget";

import {
  currentTraceExport, sameTraceExportBinding, selectedTraceFromNavigation, traceFamilyFromParam, traceNavigationParams,
  type BoundTraceExport, type TraceExportBinding,
} from "./traceNavigation";

import "./styles.css";

function toError(value: unknown): Error {
  return value instanceof Error ? value : new Error(String(value || "Unknown error"));
}

function isWithinScoreFilter(trace: AgentTraceSummary, filters: AssistantTraceFilters) {
  if (!filters.score_status || filters.score_status === "all") return true;
  if (filters.score_status === "scored") return trace.scores_count > 0;
  return trace.scores_count === 0;
}

function WorkbenchPanel({
  title,
  description,
  icon,
  children,
}: {
  title: string;
  description: string;
  icon: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="eval-panel eval-workbench-panel">
      <div className="eval-panel-heading">
        <div>
          <h2>{title}</h2>
          <p>{description}</p>
        </div>
        <div className="eval-workbench-icon">{icon}</div>
      </div>
      <div className="eval-workbench-body">{children}</div>
    </section>
  );
}

function parseJsonObjectDraft(text: string, fallback: Record<string, unknown>): Record<string, unknown> {
  const trimmed = text.trim();
  if (!trimmed) return fallback;
  const parsed: unknown = JSON.parse(trimmed);
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("JSON config must be an object");
  }
  return parsed as Record<string, unknown>;
}

function evaluatorPresetForType(
  evaluatorType: EvalEvaluator["evaluator_type"],
  traceFamily: TraceFamily,
  translate: (key: string, fallback?: string) => string,
): {
  rubric: string;
  filterConfigText: string;
  samplingConfigText: string;
} {
  const defaultSampling = JSON.stringify(
    {
      online: {
        enabled: false,
        rate: 0.05,
        trace_families: [traceFamily],
        only_failed: false,
      },
    },
    null,
    2,
  );

  if (evaluatorType === "llm" || evaluatorType === "llm_judge") {
    const rubric =
      traceFamily === "rag"
        ? "Score RAG faithfulness and retrieval usefulness from 0 to 1 using bounded previews and trajectory summary."
        : "Score helpfulness, grounding, tool-use quality, and safety from 0 to 1 using bounded previews and trajectory summary.";
    return { rubric, filterConfigText: "{}", samplingConfigText: defaultSampling };
  }
  if (evaluatorType === "span") {
    const spanKinds =
      traceFamily === "rag"
        ? ["retriever", "model_invocation", "lifecycle"]
        : ["tool_execution", "model_invocation", "retriever"];
    return {
      rubric: "Score each execution span for correctness, grounding, and failure-free completion.",
      filterConfigText: JSON.stringify(
        {
          mode: "rule",
          span_kinds: spanKinds,
          rules: [{ type: "no_error_spans" }, { type: "output_not_empty" }],
          pass_threshold: 0.8,
        },
        null,
        2,
      ),
      samplingConfigText: defaultSampling,
    };
  }
  if (evaluatorType === "rule") {
    const rules =
      traceFamily === "rag"
        ? [
            { type: "status_eq", value: "succeeded" },
            { type: "retrieval_document_count_gte", value: 1 },
            { type: "output_not_empty" },
            { type: "no_error_spans" },
          ]
        : [
            { type: "status_eq", value: "succeeded" },
            { type: "output_not_empty" },
            { type: "no_error_spans" },
          ];
    return {
      rubric: "",
      filterConfigText: JSON.stringify({ rules, pass_threshold: 0.8 }, null, 2),
      samplingConfigText: defaultSampling,
    };
  }
  if (evaluatorType === "trajectory") {
    const required =
      traceFamily === "assistant"
        ? ["lifecycle", "model_invocation"]
        : traceFamily === "rag"
          ? ["lifecycle", "retriever"]
          : ["lifecycle"];
    return {
      rubric: "",
      filterConfigText: JSON.stringify({ required_span_kinds: required }, null, 2),
      samplingConfigText: defaultSampling,
    };
  }
  if (evaluatorType === "composite") {
    return {
      rubric: "",
      filterConfigText: JSON.stringify(
        {
          pass_threshold: 0.8,
          components: [
            { type: "rule", weight: 0.4, config: { rules: [{ type: "output_not_empty" }] } },
            { type: "trajectory", weight: 0.3, config: { required_span_kinds: ["lifecycle"] } },
            { type: "llm_judge", weight: 0.3 },
          ],
        },
        null,
        2,
      ),
      samplingConfigText: defaultSampling,
    };
  }
  if (evaluatorType === "ragas") {
    return {
      rubric: translate("eval.ragas.evaluatorRubric"),
      filterConfigText: JSON.stringify(
        {
          metrics: ["context_relevancy"],
          required_span_kinds: ["retriever"],
          pass_threshold: 0.7,
        },
        null,
        2,
      ),
      samplingConfigText: JSON.stringify(
        {
          online: {
            enabled: traceFamily === "rag",
            rate: 0.05,
            trace_families: ["rag"],
            only_failed: false,
          },
        },
        null,
        2,
      ),
    };
  }
  return {
    rubric: "Score helpfulness, grounding, and safety from bounded trace previews.",
    filterConfigText: "{}",
    samplingConfigText: defaultSampling,
  };
}

function buildServerFilters(
  filters: AssistantTraceFilters,
  traceFamily: TraceFamily,
  kbDatasetId?: string,
  offset = 0,
): ListAgentTracesParams {
  const datasetFilter = kbDatasetId?.trim() || undefined;
  const startedAfter = filters.start_date
    ? new Date(`${filters.start_date}T00:00:00`).toISOString()
    : undefined;
  const startedBefore = filters.end_date
    ? new Date(`${filters.end_date}T23:59:59.999`).toISOString()
    : undefined;
  return {
    trace_family: traceFamily,
    metadata_dataset_id: traceFamily === "rag" && datasetFilter ? datasetFilter : undefined,
    status: filters.status && filters.status !== "all" ? (filters.status as TraceStatus) : undefined,
    model_id: filters.model_id?.trim() || undefined,
    user_id: filters.user_id?.trim() || undefined,
    session_id: filters.session_id?.trim() || undefined,
    run_id: filters.run_id?.trim() || undefined,
    request_id: filters.request_id?.trim() || undefined,
    transcript_query: filters.transcript_query?.trim() || undefined,
    turn_index: filters.turn_index,
    span_kind: filters.span_kind?.trim() || undefined,
    score_name: filters.score_name?.trim() || undefined,
    min_score: filters.min_score,
    max_score: filters.max_score,
    min_latency_ms: filters.min_latency_ms,
    max_latency_ms: filters.max_latency_ms,
    started_after: startedAfter,
    started_before: startedBefore,
    limit: 50,
    offset,
  };
}

function asNumber(value: unknown, fallback = 0) {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function pct(value: unknown) {
  return `${Math.round(asNumber(value) * 100)}%`;
}

type ExampleActionOptions = {
  split?: string;
  reviewStatus?: EvalReviewStatus;
  metadata?: Record<string, unknown>;
  expectedOutput?: Record<string, unknown>;
};

export function EvalPage() {
  const { t } = useTranslation();
  const location = useLocation();
  // POP reuses a history key. Each observed visit needs a fresh export identity.
  const traceVisit = useMemo(() => ({ location, epoch: generateUUID() }), [location]);
  const [searchParams, setSearchParams] = useSearchParams();
  const requestedTraceFamily = searchParams.get("family");
  const activeTraceFamily = traceFamilyFromParam(requestedTraceFamily);
  const requestedTraceId = searchParams.get("trace_id") || undefined;
  const token = useAuthStore((state) => state.token);
  const user = useAuthStore((state) => state.user);
  const authorityScope = sdkAuthorityScope(user);
  // Fresh cache identity on every auth change; credentials never enter query keys.
  const authenticatedScope = useAuthenticatedReadScope();
  const traceAccess = useMemo(() => ({ key: authenticatedScope.id, token, authorityScope }), [authenticatedScope.id, token, authorityScope]);
  const queryClient = useQueryClient();
  const { message, modal } = AntApp.useApp();
  const canRunEvaluations = usePermission("console:eval:run");
  const [filters, setFilters] = useState<AssistantTraceFilters>({
    status: "all",
    score_status: "all",
  });
  const [scoreError, setScoreError] = useState<Error | null>(null);
  const [createdDataset, setCreatedDataset] = useState<EvalDataset | null>(null);
  const [createdEvaluator, setCreatedEvaluator] = useState<EvalEvaluator | null>(null);
  const [createdExperiment, setCreatedExperiment] = useState<EvalExperiment | null>(null);
  const [selectedDatasetId, setSelectedDatasetId] = useState<string | undefined>();
  const [selectedEvaluatorId, setSelectedEvaluatorId] = useState<string | undefined>();
  const [selectedExperimentId, setSelectedExperimentId] = useState<string | undefined>();
  const [datasetDraft, setDatasetDraft] = useState({
    name: "assistant-regression",
    description: "Assistant trace regression dataset",
    version: "v1",
    schemaText: JSON.stringify(
      { input: "bounded trace preview", expected_output: "bounded output preview" },
      null,
      2
    ),
  });
  const [exampleDraft, setExampleDraft] = useState({
    split: "regression",
    expectedOutputText: JSON.stringify({ output_preview: "expected bounded answer" }, null, 2),
  });
  const [evaluatorDraft, setEvaluatorDraft] = useState<{
    name: string;
    evaluator_type: EvalEvaluator["evaluator_type"];
    rubric: string;
    version: string;
    samplingConfigText: string;
    filterConfigText: string;
  }>({
    name: "quality",
    evaluator_type: "human",
    rubric: "Score helpfulness, grounding, and safety from bounded trace previews.",
    version: "v1",
    samplingConfigText: "{}",
    filterConfigText: "{}",
  });
  const [experimentDraft, setExperimentDraft] = useState({
    name: "assistant-baseline",
    description: "",
    targetConfigText: JSON.stringify({ trace_family: "assistant" }, null, 2),
  });
  const [observedExport, setObservedExport] = useState<BoundTraceExport | null>(null);
  const [examplesExport, setExamplesExport] = useState<EvalExamplesExportResponse | null>(null);
  const [runComparison, setRunComparison] = useState<EvalExperimentRunComparisonResponse | null>(null);
  const [comparisonLoading, setComparisonLoading] = useState(false);
  const [comparisonError, setComparisonError] = useState<Error | null>(null);
  const comparisonRequest = useRef<object | null>(null);
  const [gateResult, setGateResult] = useState<EvalGateDryRunResponse | null>(null);
  const [queuedRunId, setQueuedRunId] = useState<string | undefined>();
  const [runMode, setRunMode] = useState<EvalExperimentRunMode>("live_candidate");
  const [candidateSource, setCandidateSource] = useState<"builtin" | "agent_version">("builtin");
  const [selectedAgentId, setSelectedAgentId] = useState<string>();
  const [selectedAgentVersionId, setSelectedAgentVersionId] = useState<string>();
  const [repetitions, setRepetitions] = useState(3);
  const [systemPromptOverride, setSystemPromptOverride] = useState("");
  const [baselineRunId, setBaselineRunId] = useState<string>();
  const [candidateRunId, setCandidateRunId] = useState<string>();
  const [traceRunFocusRevision, setTraceRunFocusRevision] = useState(0);
  const kbDatasetFilter = searchParams.get("dataset_id") || "";
  const requestedWorkbenchTab = searchParams.get("tab");
  const requestedSessionId = searchParams.get("session_id") || undefined;
  const [activeWorkbenchTab, setActiveWorkbenchTab] = useState(
    ["overview", "runs", "traces", "assets", "gates"].includes(requestedWorkbenchTab || "")
      ? requestedWorkbenchTab!
      : "overview"
  );
  const [traceOffset, setTraceOffset] = useState(0);
  const [activeRunTab, setActiveRunTab] = useState("experiment_runs");
  const [activeAssetsTab, setActiveAssetsTab] = useState(
    searchParams.get("review") === "pending" ? "review_queue" : "golden_sets"
  );
  const [reviewPage, setReviewPage] = useState(1);
  const [reviewBusyIds, setReviewBusyIds] = useState<Set<string>>(new Set());
  const reviewInFlightRef = useRef(new Set<string>());
  const selectedKbDatasetRef = useRef("");
  useEffect(() => {
    if (searchParams.get("review") === "pending") {
      setActiveWorkbenchTab("assets");
      setActiveAssetsTab("review_queue");
    }
  }, [location.key, searchParams]);
  const [contractPrefillRevision, setContractPrefillRevision] = useState(0);
  const serverFilters = useMemo(
    () => buildServerFilters(filters, activeTraceFamily, kbDatasetFilter, traceOffset),
    [activeTraceFamily, filters, kbDatasetFilter, traceOffset]
  );
  const traceFamilyEnabled =
    activeTraceFamily === "assistant"
    || activeTraceFamily === "langgraph_proxy"
    || activeTraceFamily === "rag";

  const summaryQuery = useQuery({
    queryKey: ["eval", "summary", traceAccess.key],
    queryFn: () => getEvalSummary(7),
    staleTime: 30_000,
  });

  const dashboardQuery = useQuery({
    queryKey: ["eval", "dashboard", traceAccess.key],
    queryFn: () => getEvalDashboard(7),
    staleTime: 30_000,
  });

  const tracesQuery = useQuery({
    queryKey: ["eval", "traces", traceAccess.key, activeTraceFamily, serverFilters],
    queryFn: () => listAgentTraces(serverFilters),
    enabled: traceFamilyEnabled && Boolean(token && user),
    staleTime: 20_000,
  });

  const visibleTraces = useMemo(() => {
    if (!traceFamilyEnabled) return [];
    const traces = tracesQuery.data?.traces || [];
    return traces.filter((trace) => isWithinScoreFilter(trace, filters));
  }, [filters, traceFamilyEnabled, tracesQuery.data?.traces]);

  const selectedTraceId = selectedTraceFromNavigation(requestedTraceId, visibleTraces);
  const exportBinding = useMemo<TraceExportBinding>(() => ({
    accessKey: traceAccess.key, navigationEpoch: traceVisit.epoch,
    family: activeTraceFamily, traceId: selectedTraceId,
  }), [traceAccess.key, traceVisit.epoch, activeTraceFamily, selectedTraceId]);
  const liveExportBinding = useRef<TraceExportBinding | null>(null);
  useLayoutEffect(() => {
    liveExportBinding.current = exportBinding;
    return () => { liveExportBinding.current = null; };
  }, [exportBinding]);
  const exportPreview = currentTraceExport(observedExport, exportBinding);

  useEffect(() => {
    if (
      requestedWorkbenchTab
      && ["overview", "runs", "traces", "assets", "gates"].includes(requestedWorkbenchTab)
    ) {
      setActiveWorkbenchTab(requestedWorkbenchTab);
    }
  }, [requestedWorkbenchTab]);

  // Deep links (e.g. the Agent Studio preview "open trace" link) pass a
  // session_id so the console opens already filtered to that session's
  // traces. buildServerFilters already forwards filters.session_id.
  useEffect(() => {
    if (requestedSessionId) {
      setFilters((prev) => ({ ...prev, session_id: requestedSessionId }));
    }
  }, [requestedSessionId]);

  useEffect(() => {
    setTraceOffset(0);
  }, [activeTraceFamily, filters, kbDatasetFilter]);


  const datasetsQuery = useQuery({
    queryKey: ["eval", "datasets"],
    queryFn: listAllEvalDatasets,
    staleTime: 30_000,
  });

  const evaluatorsQuery = useQuery({
    queryKey: ["eval", "evaluators"],
    queryFn: () => listEvalEvaluators(),
    staleTime: 30_000,
  });

  const experimentsQuery = useQuery({
    queryKey: ["eval", "experiments", traceAccess.key],
    queryFn: () => listEvalExperiments(),
    staleTime: 30_000,
  });
  const agentsQuery = useQuery({
    queryKey: ["eval", "agent-candidates"],
    queryFn: () => listAgents({ limit: 100 }),
    enabled: runMode === "live_candidate" && candidateSource === "agent_version",
    staleTime: 30_000,
  });
  const agentVersionsQuery = useQuery({
    queryKey: ["eval", "agent-versions", selectedAgentId],
    queryFn: () => listAgentVersions(selectedAgentId || ""),
    enabled: runMode === "live_candidate" && candidateSource === "agent_version" && Boolean(selectedAgentId),
    staleTime: 10_000,
  });
  const experimentDetailQuery = useQuery({
    queryKey: ["eval", "experiment", selectedExperimentId, traceAccess.key],
    queryFn: () => getEvalExperiment(selectedExperimentId || ""),
    enabled: Boolean(selectedExperimentId),
    staleTime: 10_000,
  });
  const datasets = useMemo(() => datasetsQuery.data?.datasets || [], [datasetsQuery.data?.datasets]);
  useEffect(() => {
    const navigation = kbDeepLinkNavigationKey(location.key, kbDatasetFilter);
    if (!kbDatasetFilter || selectedKbDatasetRef.current === navigation) return;
    const linked = datasets.find((dataset) => dataset.metadata?.kb_dataset_id === kbDatasetFilter);
    if (!linked) return;
    setSelectedDatasetId(linked.dataset_id);
    selectedKbDatasetRef.current = navigation;
  }, [datasets, kbDatasetFilter, location.key]);
  const evaluators = useMemo(() => evaluatorsQuery.data?.evaluators || [], [evaluatorsQuery.data?.evaluators]);
  const ragasEvaluators = useMemo(() => {
    const listed = evaluators.filter((evaluator) => evaluator.evaluator_type === "ragas");
    if (
      createdEvaluator?.evaluator_type === "ragas"
      && !listed.some((evaluator) => evaluator.evaluator_id === createdEvaluator.evaluator_id)
    ) {
      return [createdEvaluator, ...listed];
    }
    return listed;
  }, [createdEvaluator, evaluators]);
  const evaluatorTypeOptions = useMemo(
    () =>
      (["human", "rule", "trajectory", "span", "llm", "llm_judge", "composite", "ragas"] as const).map((value) => ({
        label: t(`eval.workbench.evaluatorTypes.${value}`),
        value,
      })),
    [t],
  );
  const experiments = useMemo(() => experimentsQuery.data?.experiments || [], [experimentsQuery.data?.experiments]);
  const selectedAgent: AgentSummary | undefined = agentsQuery.data?.items.find((agent) => agent.agent_id === selectedAgentId);
  const selectedAgentVersion = agentVersionsQuery.data?.find((version) => version.agent_version_id === selectedAgentVersionId);
  const activeDataset = useMemo(() => {
    if (selectedDatasetId) {
      return datasets.find((dataset) => dataset.dataset_id === selectedDatasetId)
        || (createdDataset?.dataset_id === selectedDatasetId ? createdDataset : null);
    }
    return createdDataset;
  }, [createdDataset, datasets, selectedDatasetId]);
  const activeEvaluator = useMemo(() => {
    if (selectedEvaluatorId) {
      return evaluators.find((evaluator) => evaluator.evaluator_id === selectedEvaluatorId)
        || (createdEvaluator?.evaluator_id === selectedEvaluatorId ? createdEvaluator : null);
    }
    return createdEvaluator;
  }, [createdEvaluator, evaluators, selectedEvaluatorId]);
  const activeRagasEvaluator = useMemo(() => {
    if (selectedEvaluatorId) {
      return ragasEvaluators.find((evaluator) => evaluator.evaluator_id === selectedEvaluatorId) || null;
    }
    return createdEvaluator?.evaluator_type === "ragas" ? createdEvaluator : null;
  }, [createdEvaluator, ragasEvaluators, selectedEvaluatorId]);
  const activeExperiment = useMemo(() => {
    if (selectedExperimentId) {
      return experimentDetailQuery.data
        || experiments.find((experiment) => experiment.experiment_id === selectedExperimentId)
        || (createdExperiment?.experiment_id === selectedExperimentId ? createdExperiment : null);
    }
    return createdExperiment;
  }, [createdExperiment, experimentDetailQuery.data, experiments, selectedExperimentId]);
  const comparableRuns = useMemo(() => activeExperiment?.runs || [], [activeExperiment?.runs]);
  const selectedRunQuery = useQuery({
    queryKey: ["eval", "run", queuedRunId, traceAccess.key],
    queryFn: () => getEvalExperimentRun(queuedRunId || ""),
    enabled: Boolean(queuedRunId),
    refetchInterval: (query) => {
      const run = query.state.data;
      return run?.status === "queued" || run?.status === "running" ? 2_000 : false;
    },
    retry: 2,
  });
  const latestRun = selectedRunQuery.data;
  const latestRunAgentTarget = agentVersionTarget(latestRun?.target_snapshot);
  const agentPublicationsQuery = useQuery({
    queryKey: ["eval", "agent-publications", latestRunAgentTarget?.agent_id],
    queryFn: () => listAgentPublications(latestRunAgentTarget!.agent_id),
    enabled: Boolean(latestRunAgentTarget?.agent_id),
    staleTime: 10_000,
  });
  const exactVersionPublications = (agentPublicationsQuery.data || []).filter((publication) =>
    publication.status === "active" && publication.version_id === latestRunAgentTarget?.agent_version_id,
  );
  const terminalRunId = latestRun && latestRun.status !== "queued" && latestRun.status !== "running"
    ? latestRun.run_id
    : undefined;

  useEffect(() => {
    if (!selectedExperimentId && experiments[0]?.experiment_id) {
      setSelectedExperimentId(experiments[0].experiment_id);
      if (!kbDatasetFilter) setSelectedDatasetId(experiments[0].dataset_id || undefined);
    }
  }, [experiments, selectedExperimentId, kbDatasetFilter]);

  useEffect(() => {
    setRunComparison(null);
    setGateResult(null);
    setQueuedRunId(undefined);
    setCandidateRunId(undefined);
    setBaselineRunId(undefined);
  }, [selectedExperimentId]);

  useEffect(() => {
    const officialBaseline = activeExperiment?.baseline_run_id || undefined;
    const successful = comparableRuns.filter((run) => run.status === "succeeded");
    setBaselineRunId((current) => current && comparableRuns.some((run) => run.run_id === current)
      ? current
      : officialBaseline || successful[1]?.run_id);
    setCandidateRunId((current) => current && comparableRuns.some((run) => run.run_id === current)
      ? current
      : successful.find((run) => run.run_id !== officialBaseline)?.run_id || comparableRuns[0]?.run_id);
  }, [activeExperiment?.baseline_run_id, comparableRuns]);

  useEffect(() => {
    if (!terminalRunId) return;
    void Promise.all([
      queryClient.invalidateQueries({ queryKey: ["eval", "experiment", selectedExperimentId] }),
      queryClient.invalidateQueries({ queryKey: ["eval", "run-results", terminalRunId] }),
      queryClient.invalidateQueries({ queryKey: ["eval", "dashboard"] }),
    ]);
  }, [queryClient, selectedExperimentId, terminalRunId]);

  useEffect(() => {
    const runId = searchParams.get("run_id");
    if (runId && runId !== queuedRunId) {
      setQueuedRunId(runId);
      setCandidateRunId(runId);
    }
  }, [queuedRunId, searchParams]);

  const examplesQuery = useQuery({
    queryKey: ["eval", "examples", activeDataset?.dataset_id],
    queryFn: () => listEvalExamples(activeDataset?.dataset_id || ""),
    enabled: Boolean(activeDataset?.dataset_id),
    staleTime: 20_000,
  });
  const reviewExamplesQuery = useQuery({
    queryKey: ["eval", "review-examples", activeDataset?.dataset_id],
    queryFn: () => listAllEvalExamples(activeDataset?.dataset_id || ""),
    enabled: Boolean(activeDataset?.dataset_id) && activeWorkbenchTab === "assets" && activeAssetsTab === "review_queue",
    staleTime: 20_000,
  });
  useEffect(() => setReviewPage(1), [activeDataset?.dataset_id]);

  const detailQuery = useQuery({
    queryKey: ["eval", "trace-detail", traceAccess.key, activeTraceFamily, selectedTraceId],
    queryFn: () => getAgentTraceDetail(selectedTraceId || "", activeTraceFamily),
    enabled: traceFamilyEnabled && Boolean(token && user && selectedTraceId),
    staleTime: 20_000,
  });

  const scoreMutation = useMutation({
    mutationFn: async (payload: AgentTraceScoreCreate) => {
      if (!selectedTraceId) throw new Error(t("eval.score.selectTraceFirst"));
      return createAgentTraceScore(selectedTraceId, payload, activeTraceFamily);
    },
    onSuccess: async () => {
      setScoreError(null);
      message.success(t("eval.score.submitted"));
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["eval", "traces"] }),
        queryClient.invalidateQueries({ queryKey: ["eval", "trace-detail", traceAccess.key, activeTraceFamily, selectedTraceId] }),
      ]);
    },
    onError: (error) => {
      setScoreError(toError(error));
    },
  });

  const exportMutation = useMutation({
    mutationFn: async (binding: TraceExportBinding) => {
      if (!binding.traceId || !liveExportBinding.current
        || !sameTraceExportBinding(binding, liveExportBinding.current)
        || !token || useAuthStore.getState().token !== token
        || sdkAuthorityScope(useAuthStore.getState().user) !== authorityScope) {
        throw new Error(t("eval.workbench.selectTrace"));
      }
      return exportAgentTrace(binding.traceId, "openinference", binding.family);
    },
    onSuccess: (result, binding) => {
      // The render guard also rejects late results after navigation or account changes.
      const observed = { binding, result };
      if (liveExportBinding.current && currentTraceExport(observed, liveExportBinding.current)
        && useAuthStore.getState().token === token
        && sdkAuthorityScope(useAuthStore.getState().user) === authorityScope) {
        setObservedExport(observed);
        message.success(t("eval.workbench.exportReady"));
      }
    },
    onError: (error, binding) => {
      if (liveExportBinding.current && sameTraceExportBinding(binding, liveExportBinding.current)
        && useAuthStore.getState().token === token
        && sdkAuthorityScope(useAuthStore.getState().user) === authorityScope) {
        message.error(toError(error).message);
      }
    },
  });

  const datasetMutation = useMutation({
    mutationFn: () =>
      createEvalDataset({
        name: datasetDraft.name.trim() || "assistant-regression",
        description: datasetDraft.description.trim(),
        version: datasetDraft.version.trim() || "v1",
        schema: parseJsonObjectDraft(datasetDraft.schemaText, {
          input: "bounded trace preview",
          expected_output: "bounded output preview",
        }),
        metadata: { source: "eval_console", trace_family: activeTraceFamily },
      }),
    onSuccess: async (dataset) => {
      setCreatedDataset(dataset);
      setSelectedDatasetId(dataset.dataset_id);
      message.success(t("eval.workbench.datasetCreated"));
      await queryClient.invalidateQueries({ queryKey: ["eval", "datasets"] });
    },
    onError: (error) => message.error(toError(error).message),
  });

  const exampleMutation = useMutation({
    mutationFn: async (options?: ExampleActionOptions) => {
      if (!activeDataset) throw new Error(t("eval.workbench.createDatasetFirst"));
      if (!selectedTraceId) throw new Error(t("eval.workbench.selectTrace"));
      return createEvalExampleFromTrace(activeDataset.dataset_id, {
        source_trace_id: selectedTraceId,
        trace_family: activeTraceFamily,
        split: options?.split || exampleDraft.split.trim() || "regression",
        expected_output: options?.expectedOutput || parseJsonObjectDraft(exampleDraft.expectedOutputText, {}),
        metadata: {
          source: "eval_console",
          trace_family: activeTraceFamily,
          review_status: options?.reviewStatus || "approved",
          ...options?.metadata,
        },
      });
    },
    onSuccess: async () => {
      message.success(t("eval.workbench.exampleCreated"));
      await queryClient.invalidateQueries({ queryKey: ["eval", "examples"] });
    },
    onError: (error) => message.error(toError(error).message),
  });

  const prefillSelectedTraceContract = () => {
    setActiveAssetsTab("golden_sets");
    setContractPrefillRevision((revision) => revision + 1);
    setActiveWorkbenchTab("assets");
  };

  const promoteSelectedTraceToGolden = prefillSelectedTraceContract;

  const addSelectedTraceToReview = () =>
    exampleMutation.mutate({ reviewStatus: "pending", metadata: { tags: ["review"] } });

  const createSelectedFailureCase = prefillSelectedTraceContract;

  const createRagasEvaluatorMutation = useMutation({
    mutationFn: () => {
      const preset = evaluatorPresetForType("ragas", "rag", (key, fallback) =>
        fallback === undefined ? t(key) : t(key, fallback));
      return createEvalEvaluator({
        name: "kb-ragas",
        evaluator_type: "ragas",
        rubric: preset.rubric,
        version: "v1",
        sampling_config: parseJsonObjectDraft(preset.samplingConfigText, {}),
        filter_config: parseJsonObjectDraft(preset.filterConfigText, {}),
        metadata: { source: "eval_console", trace_family: "rag" },
      });
    },
    onSuccess: async (evaluator) => {
      setCreatedEvaluator(evaluator);
      setSelectedEvaluatorId(evaluator.evaluator_id);
      message.success(t("eval.ragas.evaluatorCreated"));
      await queryClient.invalidateQueries({ queryKey: ["eval", "evaluators"] });
    },
    onError: (error) => message.error(toError(error).message),
  });

  const evaluatorMutation = useMutation({
    mutationFn: () => {
      const metadata: Record<string, unknown> = {
        source: "eval_console",
        trace_family: activeTraceFamily,
      };
      if (evaluatorDraft.evaluator_type === "llm" || evaluatorDraft.evaluator_type === "llm_judge") {
        metadata.judge_model_id = "qwen3.7-plus";
      }
      return createEvalEvaluator({
        name: evaluatorDraft.name.trim() || "quality",
        evaluator_type: evaluatorDraft.evaluator_type,
        rubric: evaluatorDraft.rubric.trim(),
        version: evaluatorDraft.version.trim() || "v1",
        sampling_config: parseJsonObjectDraft(evaluatorDraft.samplingConfigText, {}),
        filter_config: parseJsonObjectDraft(evaluatorDraft.filterConfigText, {}),
        metadata,
      });
    },
    onSuccess: async (evaluator) => {
      setCreatedEvaluator(evaluator);
      setSelectedEvaluatorId(evaluator.evaluator_id);
      message.success(t("eval.workbench.evaluatorCreated"));
      await queryClient.invalidateQueries({ queryKey: ["eval", "evaluators"] });
    },
    onError: (error) => message.error(toError(error).message),
  });

  const experimentMutation = useMutation({
    mutationFn: () =>
      createEvalExperiment({
        name: experimentDraft.name.trim() || "assistant-baseline",
        description: experimentDraft.description.trim(),
        dataset_id: activeDataset?.dataset_id || null,
        target_config: buildExperimentTargetConfig(
          parseJsonObjectDraft(experimentDraft.targetConfigText, {}), activeTraceFamily,
        ),
        metadata: { source: "eval_console", trace_family: activeTraceFamily },
      }),
    onSuccess: async (experiment) => {
      setCreatedExperiment(experiment);
      setSelectedExperimentId(experiment.experiment_id);
      message.success(t("eval.workbench.experimentCreated"));
      await queryClient.invalidateQueries({ queryKey: ["eval", "experiments"] });
    },
    onError: (error) => message.error(toError(error).message),
  });

  const versionMutation = useMutation({
    mutationFn: () => {
      if (!selectedAgentId || !selectedAgent?.draft_revision || selectedAgent.caller_role !== "owner") {
        throw new Error(t("eval.workbench.agentVersionUnavailable", "Only the Agent owner can snapshot a saved draft."));
      }
      return createAgentVersion(selectedAgentId, selectedAgent.draft_revision);
    },
    onSuccess: async (version) => {
      setSelectedAgentVersionId(version.agent_version_id);
      await queryClient.invalidateQueries({ queryKey: ["eval", "agent-versions", version.agent_id] });
      message.success(t("eval.workbench.agentVersionCreated", { version: version.version_number, defaultValue: "Immutable Agent version {{version}} created." }));
    },
    onError: (error) => message.error(toError(error).message),
  });

  const evaluatorRunMutation = useMutation({
    mutationFn: async () => {
      if (!activeEvaluator) throw new Error(t("eval.workbench.createEvaluatorFirst"));
      return runEvalEvaluatorAsync(activeEvaluator.evaluator_id, {
        experiment_id: activeExperiment?.experiment_id || null,
        dataset_id: activeDataset?.dataset_id || null,
        trace_id: selectedTraceId || null,
        target_snapshot: {
          trace_family: activeTraceFamily,
          trace_id: selectedTraceId || null,
          dataset_id: activeDataset?.dataset_id || null,
          experiment_id: activeExperiment?.experiment_id || null,
          evaluator_id: activeEvaluator.evaluator_id,
          target_config: activeExperiment?.target_config || {},
        },
        metadata: { source: "eval_console", trace_family: activeTraceFamily },
      });
    },
    onSuccess: (job) => {
      if (job.run_id) {
        setQueuedRunId(job.run_id);
        setCandidateRunId(job.run_id);
      }
      message.success(t("eval.workbench.evaluatorQueued"));
    },
    onError: (error) => message.error(toError(error).message),
  });

  const ragasEvaluatorRunMutation = useMutation({
    mutationFn: async () => {
      if (!activeRagasEvaluator) throw new Error(t("eval.workbench.createEvaluatorFirst"));
      return runEvalEvaluatorAsync(activeRagasEvaluator.evaluator_id, {
        experiment_id: null,
        dataset_id: null,
        trace_id: selectedTraceId || null,
        target_snapshot: {
          trace_family: "rag",
          trace_id: selectedTraceId || null,
          evaluator_id: activeRagasEvaluator.evaluator_id,
        },
        metadata: { source: "eval_console", trace_family: "rag", evaluator_type: "ragas" },
      });
    },
    onSuccess: (job) => {
      if (job.run_id) {
        setQueuedRunId(job.run_id);
        setCandidateRunId(job.run_id);
      }
      message.success(t("eval.workbench.evaluatorQueued"));
    },
    onError: (error) => message.error(toError(error).message),
  });

  const experimentBatchMutation = useMutation({
    mutationFn: async () => {
      if (!activeExperiment) throw new Error(t("eval.workbench.createExperimentFirst", "Create an experiment first"));
      if (!activeEvaluator) throw new Error(t("eval.workbench.createEvaluatorFirst"));
      const agentCandidate = runMode === "live_candidate" && candidateSource === "agent_version";
      if (agentCandidate && (!selectedAgentId || !selectedAgentVersionId)) {
        throw new Error(t("eval.workbench.selectAgentVersion", "Select an immutable Agent version before running."));
      }
      const datasetId = activeDataset?.dataset_id || activeExperiment.dataset_id || null;
      return runEvalExperiment(activeExperiment.experiment_id, {
        dataset_id: datasetId,
        evaluator_ids: [activeEvaluator.evaluator_id],
        run_mode: runMode,
        repetitions: runMode === "live_candidate" ? repetitions : 1,
        baseline_run_id: baselineRunId || activeExperiment.baseline_run_id || null,
        candidate_config: agentCandidate
          ? { agent_id: selectedAgentId!, agent_version_id: selectedAgentVersionId! }
          : runMode === "live_candidate" && systemPromptOverride.trim()
            ? { system_prompt_override: systemPromptOverride.trim() } : undefined,
        candidate_label: agentCandidate ? `${selectedAgent?.name || "Agent"} v${selectedAgentVersion?.version_number || "?"}` : "candidate",
        baseline_label: "baseline",
        ...(!agentCandidate ? { target_snapshot: {
          trace_family: activeTraceFamily,
          dataset_id: datasetId,
          trace_id: datasetId ? null : selectedTraceId || null,
          run_mode: runMode,
        } } : {}),
        metadata: { source: "eval_console", trace_family: activeTraceFamily },
      });
    },
    onSuccess: (batch) => {
      const firstRunId = batch.jobs[0]?.run_id;
      if (firstRunId) {
        setQueuedRunId(firstRunId);
        setCandidateRunId(firstRunId);
        setRunComparison(null);
        setGateResult(null);
        const nextParams = new URLSearchParams(searchParams);
        nextParams.set("run_id", firstRunId);
        setSearchParams(nextParams, { replace: true });
      }
      message.success(t("eval.workbench.evaluatorQueued"));
    },
    onError: (error) => message.error(toError(error).message),
  });

  const runActions = useEvalRunActions({
    runId: latestRun?.run_id === queuedRunId ? queuedRunId : undefined,
    experimentId: selectedExperimentId,
    visit: location,
    allowed: canRunEvaluations,
    onRetryQueued: (runId) => {
      setQueuedRunId(runId);
      setCandidateRunId(runId);
      setRunComparison(null);
      setGateResult(null);
      const nextParams = new URLSearchParams(searchParams);
      nextParams.set("run_id", runId);
      setSearchParams(nextParams, { replace: true });
    },
  });

  const examplesExportMutation = useMutation({
    mutationFn: async () => {
      if (!activeDataset) throw new Error(t("eval.workbench.createDatasetFirst"));
      return exportEvalExamples(activeDataset.dataset_id);
    },
    onSuccess: (result) => {
      setExamplesExport(result);
      message.success(t("eval.workbench.exportReady"));
    },
    onError: (error) => message.error(toError(error).message),
  });

  const examplesImportMutation = useMutation({
    mutationFn: async () => {
      if (!activeDataset) throw new Error(t("eval.workbench.createDatasetFirst"));
      const messageText = String(detailQuery.data?.trace.input_preview || "").trim();
      if (!selectedTraceId || !messageText) throw new Error(t("eval.workbench.selectTrace"));
      return importEvalExamples(activeDataset.dataset_id, [
        {
          case_id: `console.${Date.now()}`,
          split: exampleDraft.split.trim() || "regression",
          input: { message: messageText, source: "eval_console_seed", trace_id: selectedTraceId },
          expected_output: parseJsonObjectDraft(exampleDraft.expectedOutputText, {}),
          expected_trajectory: { required_span_kinds: ["lifecycle", "model_invocation"] },
          assertions: [{ type: "no_sensitive_output" }],
          metadata: {
            review_status: "pending",
            behavior_confirmed: false,
            tags: ["console-import"],
          },
        },
      ]);
    },
    onSuccess: async () => {
      message.success(t("eval.workbench.exampleCreated"));
      await queryClient.invalidateQueries({ queryKey: ["eval", "examples"] });
    },
    onError: (error) => message.error(toError(error).message),
  });

  const reviewMutation = useMutation({
    mutationFn: async (payload: { example: EvalExample; status: "approved" | "rejected" | "needs_fix" }) => {
      if (!activeDataset) throw new Error(t("eval.workbench.createDatasetFirst"));
      return updateEvalExample(activeDataset.dataset_id, payload.example.example_id, {
        review_status: payload.status,
        ...(payload.example.metadata?.source_kind === "kb_failure"
          ? { split: "review" }
          : {}),
        metadata: { reviewed_from: "eval_console" },
      });
    },
    onSuccess: async (_result, payload) => {
      message.success(
        payload.example.metadata?.source_kind === "kb_failure" && payload.status === "approved"
          ? t("knowledge.detail.failureApprovedNotRunnable")
          : t("eval.score.submitted")
      );
      await queryClient.invalidateQueries({ queryKey: ["eval", "examples"] });
      await queryClient.invalidateQueries({ queryKey: ["eval", "review-examples"] });
    },
    onError: (error) => message.error(toError(error).message),
    onSettled: (_data, _error, payload) => {
      if (!payload) return;
      reviewInFlightRef.current.delete(payload.example.example_id);
      setReviewBusyIds((current) => {
        const next = new Set(current);
        next.delete(payload.example.example_id);
        return next;
      });
    },
  });

  function submitReview(example: EvalExample, status: "approved" | "rejected" | "needs_fix") {
    if (!canRunEvaluations || reviewInFlightRef.current.has(example.example_id)) return;
    if (status === "approved" && !canApproveReviewExample(example)) return;
    reviewInFlightRef.current.add(example.example_id);
    setReviewBusyIds((current) => new Set(current).add(example.example_id));
    reviewMutation.mutate({ example, status });
  }

  const baselineRun = comparableRuns.find((run) => run.run_id === baselineRunId) || null;
  const candidateRun = comparableRuns.find((run) => run.run_id === candidateRunId) || null;
  const canCompareRuns = Boolean(
    baselineRunId
    && candidateRunId
    && baselineRunId !== candidateRunId
    && baselineRun?.status === "succeeded"
    && candidateRun?.status === "succeeded"
  );
  const latestScoreSummary = candidateRun?.score_summary || latestRun?.score_summary || {};
  const canDryRunGate = Boolean(
    canCompareRuns
    && candidateRun?.status === "succeeded"
    && latestScoreSummary.schema_version === "eval-gate-metrics/v2"
    && typeof latestScoreSummary.case_count === "number"
    && typeof latestScoreSummary.score_sum === "number"
    && typeof latestScoreSummary.overall_score === "number"
    && typeof latestScoreSummary.failed_case_count === "number"
    && typeof latestScoreSummary.trajectory_case_count === "number"
    && typeof latestScoreSummary.trajectory_failed_count === "number"
    && typeof latestScoreSummary.trajectory_pass_rate === "number"
    && typeof latestScoreSummary.critical_case_count === "number"
    && latestScoreSummary.critical_case_count > 0
    && typeof latestScoreSummary.critical_failed_count === "number"
    && typeof latestScoreSummary.critical_pass_rate === "number"
    && typeof latestScoreSummary.stateful_case_count === "number"
    && latestScoreSummary.stateful_case_count > 0
    && typeof latestScoreSummary.stateful_failed_count === "number"
    && typeof latestScoreSummary.stateful_pass_rate === "number"
  );
  const effectiveRunDatasetId = activeDataset?.dataset_id || activeExperiment?.dataset_id || null;
  const needsAgentVersion = runMode === "live_candidate" && candidateSource === "agent_version";
  const canRunExperiment = Boolean(
    canRunEvaluations
    && activeExperiment
    && activeEvaluator
    && (!needsAgentVersion || (selectedAgent && selectedAgentVersion))
    && (runMode === "live_candidate"
      ? effectiveRunDatasetId
      : effectiveRunDatasetId || selectedTraceId)
  );
  const missingRunInputs = [
    !activeExperiment ? "experiment" : null,
    !activeEvaluator ? "evaluator" : null,
    needsAgentVersion && (!selectedAgent || !selectedAgentVersion) ? "immutable Agent version" : null,
    runMode === "live_candidate"
      ? !effectiveRunDatasetId ? "test set" : null
      : !effectiveRunDatasetId && !selectedTraceId ? "test set or trace" : null,
  ].filter(Boolean);

  const selectRun = (runId: string) => {
    setQueuedRunId(runId);
    const nextParams = new URLSearchParams(searchParams);
    nextParams.set("run_id", runId);
    setSearchParams(nextParams, { replace: true });
  };

  // A run pair identifies data, not the request or navigation visit that read it.
  useLayoutEffect(() => {
    comparisonRequest.current = null;
    setComparisonLoading(false);
    setRunComparison(null);
    setComparisonError(null);
    return () => { comparisonRequest.current = null; };
  }, [baselineRunId, candidateRunId, selectedExperimentId, location, token, authorityScope]);

  useLayoutEffect(() => useAuthStore.subscribe((state, previous) => {
    if (state.token !== previous.token || state.isAuthenticated !== previous.isAuthenticated
      || sdkAuthorityScope(state.user) !== sdkAuthorityScope(previous.user)) {
      comparisonRequest.current = null;
      setRunComparison(null);
      setComparisonLoading(false);
      setComparisonError(null);
    }
  }), []);

  const compareMutation = useMutation({
    mutationFn: async (request: { baseline: string; candidate: string; ownsAuthority: () => boolean }) => {
      if (comparisonRequest.current !== request || !request.ownsAuthority()) throw new Error("Comparison authorization changed");
      return compareEvalExperimentRuns(request.baseline, request.candidate);
    },
    onSuccess: (result, request) => {
      if (comparisonRequest.current !== request) return;
      setComparisonLoading(false);
      setRunComparison(result);
    },
    onError: (error, request) => {
      if (comparisonRequest.current !== request) return;
      setComparisonLoading(false);
      setComparisonError(toError(error));
      message.error(toError(error).message);
    },
  });

  const compareSelectedRuns = () => {
    if (!baselineRunId || !candidateRunId) return;
    const request = { baseline: baselineRunId, candidate: candidateRunId, ownsAuthority: () => {
      const state = useAuthStore.getState();
      return Boolean(token && state.isAuthenticated && state.user && state.token === token
        && sdkAuthorityScope(state.user) === authorityScope);
    } };
    comparisonRequest.current = request;
    setComparisonLoading(true);
    setRunComparison(null);
    setComparisonError(null);
    compareMutation.mutate(request);
  };

  const promoteBaselineMutation = useMutation({
    mutationFn: async () => {
      if (!activeExperiment || !candidateRunId) {
        throw new Error(t("eval.workbench.selectCandidateFirst", "Select a candidate run first"));
      }
      return promoteEvalExperimentBaseline(activeExperiment.experiment_id, candidateRunId);
    },
    onSuccess: async (promotion) => {
      setBaselineRunId(promotion.baseline_run_id);
      setRunComparison(null);
      message.success(t("eval.workbench.baselinePromoted", "Baseline promoted"));
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["eval", "experiments"] }),
        queryClient.invalidateQueries({ queryKey: ["eval", "experiment", selectedExperimentId] }),
      ]);
    },
    onError: (error) => message.error(toError(error).message),
  });

  const gateMutation = useMutation({
    mutationFn: () =>
      dryRunEvalGate({
        baseline_run_id: baselineRunId,
        candidate_run_id: candidateRunId,
        result_payload: {
          metrics: latestScoreSummary,
        },
      }),
    onSuccess: (result) => setGateResult(result),
    onError: (error) => message.error(toError(error).message),
  });

  const runResultsQuery = useQuery({
    queryKey: ["eval", "run-results", queuedRunId, traceAccess.key],
    queryFn: () => getEvalExperimentRunResults(queuedRunId || ""),
    enabled: Boolean(queuedRunId && latestRun?.run_id === queuedRunId),
    refetchInterval: latestRun?.status === "queued" || latestRun?.status === "running" ? 2_000 : false,
    staleTime: 10_000,
  });

  const traceListCopy = useMemo(() => {
    if (activeTraceFamily === "rag") {
      return {
        title: t("eval.list.ragTitle"),
        empty: t("eval.list.ragEmpty"),
        aria: t("eval.list.ragAriaLabel"),
      };
    }
    if (activeTraceFamily === "langgraph_proxy") {
      return {
        title: t("eval.list.langgraphTitle"),
        empty: t("eval.list.langgraphEmpty"),
        aria: t("eval.list.langgraphAriaLabel"),
      };
    }
    return {
      title: t("eval.list.title"),
      empty: t("eval.list.empty"),
      aria: t("eval.list.ariaLabel"),
    };
  }, [activeTraceFamily, t]);
  const hasCapturedFamilyTraces = (tracesQuery.data?.total || 0) > 0;
  const familyCoverageMessage = useMemo(() => {
    if (activeTraceFamily === "assistant") return null;
    if (hasCapturedFamilyTraces) {
      return t("eval.workbench.familyCovered", "Trace family wired with captured backend data");
    }
    return t("eval.workbench.familyPartial", "Trace family wired but not fully covered");
  }, [activeTraceFamily, hasCapturedFamilyTraces, t]);

  const handleTraceFamilyChange = (key: string) => {
    const nextFamily = key as TraceFamily;
    setSearchParams(traceNavigationParams(searchParams, nextFamily), { replace: true });
    setFilters({ status: "all", score_status: "all" });
    setScoreError(null);
    setObservedExport(null);
  };

  const handleWorkbenchTabChange = (key: string) => {
    const nextParams = new URLSearchParams(searchParams);
    nextParams.set("tab", key);
    setSearchParams(nextParams, { replace: true });
    setActiveWorkbenchTab(key);
  };

  const openRunResultTrace = (item: EvalExperimentCaseResult) => {
    const family = item.trace.trace_family || activeTraceFamily;
    setSearchParams(traceNavigationParams(searchParams, family, item.candidate_trace_id, "traces"));
    if (family !== activeTraceFamily) setFilters({ status: "all", score_status: "all" });
    setActiveWorkbenchTab("traces");
    setTraceRunFocusRevision((revision) => revision + 1);
  };

  const comparedTraceOwner = comparisonRequest.current;
  const openComparedTrace = (traceId: string) => {
    const state = useAuthStore.getState();
    if (!comparedTraceOwner || comparisonRequest.current !== comparedTraceOwner
      || !token || !state.isAuthenticated || state.token !== token || sdkAuthorityScope(state.user) !== authorityScope) return;
    setSearchParams(traceNavigationParams(searchParams, activeTraceFamily, traceId, "traces"));
    setActiveWorkbenchTab("traces");
    setTraceRunFocusRevision((revision) => revision + 1);
  };

  const dashboardMetrics = (dashboardQuery.data?.metrics || {}) as Record<string, number | undefined>;
  const runtimeHealth = (dashboardQuery.data?.runtime_health || {}) as Record<string, number | undefined>;
  const overviewTab = (
    <>
      {summaryQuery.error || dashboardQuery.error ? (
        <Alert
          type="error"
          showIcon
          title={t("eval.workbench.overviewLoadFailed", "Could not load evaluation overview")}
          description={toError(summaryQuery.error || dashboardQuery.error).message}
          action={<Button onClick={() => void Promise.all([summaryQuery.refetch(), dashboardQuery.refetch()])}>{t("common.retry", "Retry")}</Button>}
        />
      ) : null}
      <div className="eval-platform-summary-block">
        <div className="eval-metric-group">
          <span className="eval-metric-group-title">{t("eval.workbench.traceVolume", "Traces & Golden Sets")}</span>
          <div className="eval-metric-items">
            <div className="eval-metric-item">
              <span className="eval-summary-label">{t("eval.summary.ragTraces")}</span>
              <strong className="eval-summary-value tabular-nums">{summaryQuery.data?.rag_traces ?? "—"}</strong>
            </div>
            <div className="eval-metric-item">
              <span className="eval-summary-label">{t("eval.summary.totalTraces")}</span>
              <strong className="eval-summary-value tabular-nums">{summaryQuery.data?.total_traces ?? "—"}</strong>
            </div>
            <div className="eval-metric-item">
              <span className="eval-summary-label">{t("eval.summary.scoredTraces")}</span>
              <strong className="eval-summary-value tabular-nums">{summaryQuery.data?.scored_traces ?? "—"}</strong>
            </div>
            <div className="eval-metric-item">
              <span className="eval-summary-label">{t("eval.workbench.goldenCases", "Golden cases")}</span>
              <strong className="eval-summary-value tabular-nums">{dashboardMetrics.example_count ?? "—"}</strong>
            </div>
          </div>
        </div>

        <div className="eval-metric-group">
          <span className="eval-metric-group-title">{t("eval.workbench.qualityGates", "Quality & Gates")}</span>
          <div className="eval-metric-items">
            <div className="eval-metric-item">
              <span className="eval-summary-label">{t("eval.workbench.passRate", "Pass rate")}</span>
              <strong className="eval-summary-value tabular-nums" style={{ color: (dashboardMetrics.pass_rate ?? 0) >= 0.8 ? "hsl(var(--success))" : "inherit" }}>
                {pct(dashboardMetrics.pass_rate)}
              </strong>
            </div>
            <div className="eval-metric-item">
              <span className="eval-summary-label">{t("eval.workbench.trajectoryPass", "Trajectory pass")}</span>
              <strong className="eval-summary-value tabular-nums">
                {pct(dashboardMetrics.trajectory_pass_rate)}
              </strong>
            </div>
            <div className="eval-metric-item">
              <span className="eval-summary-label">{t("eval.workbench.criticalFailures", "Critical failures")}</span>
              <strong className="eval-summary-value tabular-nums" style={{ color: (dashboardMetrics.critical_failures ?? 0) > 0 ? "hsl(var(--destructive))" : "inherit" }}>
                {dashboardMetrics.critical_failures ?? 0}
              </strong>
            </div>
            <div className="eval-metric-item">
              <span className="eval-summary-label">{t("eval.workbench.toolSafetyFailures", "Tool-safety")}</span>
              <strong className="eval-summary-value tabular-nums">
                {runtimeHealth.tool_safety_failures ?? 0}
              </strong>
            </div>
            <div className="eval-metric-item">
              <span className="eval-summary-label">{t("eval.workbench.outboxFailures", "Outbox")}</span>
              <strong className="eval-summary-value tabular-nums">
                {Number(dashboardQuery.data?.queue_health?.failed_jobs ?? 0)}
              </strong>
            </div>
            <div className="eval-metric-item">
              <span className="eval-summary-label">{t("eval.workbench.judgePending", "Judge pending")}</span>
              <strong className="eval-summary-value tabular-nums">
                {dashboardMetrics.judge_pending_count ?? 0}
              </strong>
            </div>
          </div>
        </div>
      </div>
      <div className="eval-platform-grid">
      <section className="eval-panel eval-workbench-panel eval-platform-wide">
        <div className="eval-panel-heading">
          <div>
            <h2>{t("eval.workbench.platformHealth", "Platform health")}</h2>
            <p>{t("eval.workbench.platformHealthDescription", "Latest run, queue, and offline gate readiness")}</p>
          </div>
          <ShieldCheck size={22} />
        </div>
        <div className="eval-overview-bars">
          <div>
            <span>{t("eval.workbench.passRate", "Pass rate")}</span>
            <Progress percent={Math.round(asNumber(dashboardMetrics.pass_rate) * 100)} size="small" />
          </div>
          <div>
            <span>{t("eval.workbench.trajectoryPass", "Trajectory pass")}</span>
            <Progress percent={Math.round(asNumber(dashboardMetrics.trajectory_pass_rate) * 100)} size="small" />
          </div>
        </div>
        <Descriptions
          className="eval-workbench-descriptions"
          size="small"
          bordered
          column={1}
          items={[
            { key: "assistant", label: t("eval.workbench.assistantRuntime", "Assistant runtime"), children: String(runtimeHealth.assistant_status || "unknown") },
            { key: "ragRuntime", label: t("eval.workbench.ragRuntime", "RAG runtime"), children: String(runtimeHealth.rag_status || "partial") },
            { key: "langgraphRuntime", label: t("eval.workbench.langgraphRuntime", "LangGraph runtime"), children: String(runtimeHealth.langgraph_status || "partial") },
            { key: "trajectoryTraces", label: t("eval.workbench.runtimeTrajectoryTraces", "Runtime trajectories"), children: String(runtimeHealth.runtime_trajectory_traces ?? 0) },
            { key: "traceWriterIssues", label: t("eval.workbench.traceWriterIssues", "Trace writer issues"), children: String(runtimeHealth.trace_writer_issue_traces ?? 0) },
            { key: "baseline", label: t("eval.workbench.latestBaseline", "Latest baseline"), children: String(dashboardMetrics.latest_baseline || "-") },
            { key: "candidate", label: t("eval.workbench.latestCandidate", "Latest candidate"), children: String(dashboardMetrics.latest_candidate || "-") },
            { key: "gate", label: t("eval.workbench.latestGate", "Latest gate"), children: String(dashboardQuery.data?.latest_gate_status?.status || "not_run") },
          ]}
        />
      </section>
      </div>
    </>
  );

  const traceExplorerTab = (
    <TraceExplorerShell
      activeTraceFamily={activeTraceFamily}
      traceAccessKey={traceAccess.key}
      onTraceFamilyChange={handleTraceFamilyChange}
      familyCoverageMessage={familyCoverageMessage}
      hasCapturedFamilyTraces={hasCapturedFamilyTraces}
      traces={visibleTraces}
      traceTotal={tracesQuery.data?.total || 0}
      traceOffset={traceOffset}
      tracePageSize={50}
      onTracePageChange={(page) => setTraceOffset((page - 1) * 50)}
      filters={filters}
      setFilters={setFilters}
      traceListCopy={traceListCopy}
      selectedTraceId={selectedTraceId}
      runFocusRevision={traceRunFocusRevision}
      onSelectTrace={(traceId) => {
        setSearchParams(traceNavigationParams(searchParams, activeTraceFamily, traceId), { replace: true });
      }}
      onRefresh={() => {
        void tracesQuery.refetch();
        if (selectedTraceId) void detailQuery.refetch();
      }}
      tracesLoading={tracesQuery.isLoading || tracesQuery.isFetching}
      tracesError={tracesQuery.error ? toError(tracesQuery.error) : null}
      detail={detailQuery.data}
      detailLoading={detailQuery.isLoading || detailQuery.isFetching}
      detailError={detailQuery.error ? toError(detailQuery.error) : null}
      scoreError={scoreError}
      scoreSubmitting={scoreMutation.isPending}
      onScoreSubmit={async (payload) => {
        await scoreMutation.mutateAsync(payload);
      }}
      exportPreview={exportPreview}
      exportLoading={exportMutation.isPending}
      onExport={() => exportMutation.mutate(exportBinding)}
      activeDatasetName={activeDataset?.name || null}
      datasetActionLoading={exampleMutation.isPending}
      onPromoteToGolden={promoteSelectedTraceToGolden}
      onAddToReview={addSelectedTraceToReview}
      onCreateFailureCase={createSelectedFailureCase}
      dashboard={dashboardQuery.data}
      readOnly={!canRunEvaluations}
    />
  );

  const goldenSetsTab = (
    <WorkbenchPanel
      title={t("eval.workbench.goldenSets", "Golden Sets")}
      description={t("eval.workbench.goldenSetsDescription", "Versioned regression cases backed by repo JSONL and DB review state.")}
      icon={<Database size={22} />}
    >
      {datasetsQuery.error || examplesQuery.error ? (
        <Alert
          type="error"
          showIcon
          title={t("eval.workbench.assetsLoadFailed", "Could not load evaluation assets")}
          description={toError(datasetsQuery.error || examplesQuery.error).message}
          action={<Button onClick={() => void Promise.all([datasetsQuery.refetch(), examplesQuery.refetch()])}>{t("common.retry", "Retry")}</Button>}
        />
      ) : null}
      <div className="eval-workbench-form-grid">
        <Select
          className="eval-workbench-wide"
          allowClear
          aria-label={t("eval.workbench.selectDataset", "Select dataset")}
          placeholder={t("eval.workbench.selectDataset", "Select dataset")}
          value={selectedDatasetId}
          options={datasets.map((dataset) => ({
            label: `${dataset.name} (${dataset.version})`,
            value: dataset.dataset_id,
          }))}
          onChange={(value) => setSelectedDatasetId(value)}
        />
        <Input
          aria-label={t("eval.workbench.datasetName", "Dataset name")}
          value={datasetDraft.name}
          onChange={(event) => setDatasetDraft((draft) => ({ ...draft, name: event.target.value }))}
        />
        <Input
          aria-label={t("eval.workbench.version")}
          value={datasetDraft.version}
          onChange={(event) => setDatasetDraft((draft) => ({ ...draft, version: event.target.value }))}
        />
        <Input
          className="eval-workbench-wide"
          aria-label={t("eval.workbench.datasetDescription", "Dataset description")}
          value={datasetDraft.description}
          onChange={(event) => setDatasetDraft((draft) => ({ ...draft, description: event.target.value }))}
        />
        <Input
          aria-label={t("eval.workbench.exampleSplit", "Example split")}
          value={exampleDraft.split}
          onChange={(event) => setExampleDraft((draft) => ({ ...draft, split: event.target.value }))}
        />
        <Input.TextArea
          className="eval-workbench-textarea eval-workbench-wide"
          aria-label={t("eval.workbench.datasetSchema", "Dataset schema JSON")}
          value={datasetDraft.schemaText}
          autoSize={{ minRows: 3, maxRows: 6 }}
          onChange={(event) => setDatasetDraft((draft) => ({ ...draft, schemaText: event.target.value }))}
        />
        <Input.TextArea
          className="eval-workbench-textarea eval-workbench-wide"
          aria-label={t("eval.workbench.expectedOutput", "Expected output JSON")}
          value={exampleDraft.expectedOutputText}
          autoSize={{ minRows: 2, maxRows: 5 }}
          onChange={(event) => setExampleDraft((draft) => ({ ...draft, expectedOutputText: event.target.value }))}
        />
      </div>
      <Space size={10} wrap>
        <Button
          type="primary"
          icon={<Database size={15} />}
          onClick={() => datasetMutation.mutate()}
          loading={datasetMutation.isPending}
          disabled={!canRunEvaluations}
        >
          {t("eval.workbench.createDataset")}
        </Button>
        <Button
          icon={<SearchCheck size={15} />}
          onClick={promoteSelectedTraceToGolden}
          loading={exampleMutation.isPending}
          disabled={!canRunEvaluations || !activeDataset || !selectedTraceId}
        >
          {t("eval.workbench.addTraceToDataset")}
        </Button>
        <Button
          icon={<Download size={15} />}
          onClick={() => examplesExportMutation.mutate()}
          loading={examplesExportMutation.isPending}
          disabled={!activeDataset}
        >
          {t("eval.workbench.exportJsonl", "Export JSONL")}
        </Button>
        <Button
          icon={<Database size={15} />}
          onClick={() => examplesImportMutation.mutate()}
          loading={examplesImportMutation.isPending}
          disabled={!canRunEvaluations || !activeDataset || !selectedTraceId || !detailQuery.data?.trace.input_preview}
        >
          {t("eval.workbench.importSeed", "Import seed case")}
        </Button>
      </Space>
      <Descriptions
        className="eval-workbench-descriptions"
        size="small"
        bordered
        column={1}
        items={[
          { key: "dataset", label: t("eval.workbench.currentDataset"), children: activeDataset?.name || "-" },
          { key: "version", label: t("eval.workbench.version"), children: activeDataset?.version || "-" },
          { key: "source", label: t("eval.workbench.selectedTrace"), children: selectedTraceId || "-" },
          { key: "listed", label: t("eval.workbench.listedDatasets", "Listed datasets"), children: String(datasetsQuery.data?.total ?? 0) },
          { key: "cases", label: t("eval.workbench.goldenCases", "Golden cases"), children: String(examplesQuery.data?.total ?? 0) },
          { key: "exported", label: t("eval.workbench.exportedCases", "Exported cases"), children: String(examplesExport?.examples.length ?? 0) },
        ]}
      />
      <BehaviorContractEditor
        datasetId={activeDataset?.dataset_id ?? null}
        examples={examplesQuery.data?.examples || []}
        selectedTrace={detailQuery.data}
        prefillRevision={contractPrefillRevision}
        readOnly={!canRunEvaluations}
        onSaved={async () => {
          await queryClient.invalidateQueries({ queryKey: ["eval", "examples"] });
          await queryClient.invalidateQueries({ queryKey: ["eval", "dashboard"] });
        }}
      />
      <GoldenJsonlImport
        datasetId={activeDataset?.dataset_id ?? null}
        readOnly={!canRunEvaluations}
        onImported={async () => {
          await queryClient.invalidateQueries({ queryKey: ["eval", "examples"] });
          await queryClient.invalidateQueries({ queryKey: ["eval", "dashboard"] });
        }}
      />
    </WorkbenchPanel>
  );

  const experimentsTab = (
    <WorkbenchPanel
      title={t("eval.workbench.runAndResults", "Run & Results")}
      description={t("eval.workbench.runAndResultsDescription", "Select a test set and evaluator, run it, then inspect scores and failed traces in one place.")}
      icon={<Beaker size={22} />}
    >
      {!canRunEvaluations ? (
        <Alert type="info" showIcon title={t("eval.workbench.readOnly", "Read-only mode: you can inspect results but cannot start or edit evaluations.")} />
      ) : null}
      {experimentsQuery.error || experimentDetailQuery.error || datasetsQuery.error || evaluatorsQuery.error ? (
        <Alert
          type="error"
          showIcon
          title={t("eval.workbench.runConfigLoadFailed", "Could not load run configuration")}
          description={toError(experimentsQuery.error || experimentDetailQuery.error || datasetsQuery.error || evaluatorsQuery.error).message}
          action={<Button onClick={() => void Promise.all([experimentsQuery.refetch(), experimentDetailQuery.refetch(), datasetsQuery.refetch(), evaluatorsQuery.refetch()])}>{t("common.retry", "Retry")}</Button>}
        />
      ) : null}
      <div className="eval-workbench-form-grid">
        <label className="eval-field">
          <span>{t("eval.workbench.selectExperiment", "Select experiment")}</span>
          <Select
            allowClear
            value={selectedExperimentId}
            options={experiments.map((experiment) => ({ label: experiment.name, value: experiment.experiment_id }))}
            onChange={(value) => {
              setSelectedExperimentId(value);
              setSelectedDatasetId(
                experiments.find((experiment) => experiment.experiment_id === value)?.dataset_id
                || undefined,
              );
              const nextParams = new URLSearchParams(searchParams);
              nextParams.delete("run_id");
              setSearchParams(nextParams, { replace: true });
            }}
          />
        </label>
        <label className="eval-field">
          <span>{t("eval.workbench.selectDataset", "Select test set")}</span>
          <Select allowClear value={selectedDatasetId} options={datasets.map((dataset) => ({ label: `${dataset.name} (${dataset.version})`, value: dataset.dataset_id }))} onChange={setSelectedDatasetId} />
        </label>
        <label className="eval-field">
          <span>{t("eval.workbench.selectEvaluator", "Select evaluator")}</span>
          <Select allowClear value={selectedEvaluatorId} options={evaluators.map((evaluator) => ({ label: `${evaluator.name} · ${evaluator.evaluator_type}`, value: evaluator.evaluator_id }))} onChange={setSelectedEvaluatorId} />
        </label>
        <label className="eval-field">
          <span>{t("eval.workbench.runMode", "Run mode")}</span>
          <Select<EvalExperimentRunMode>
            value={runMode}
            options={[
              { label: t("eval.workbench.liveCandidate", "Run current Agent"), value: "live_candidate" },
              { label: t("eval.workbench.rescoreTrace", "Re-score stored traces"), value: "rescore_trace" },
            ]}
            onChange={(value) => setRunMode(value)}
          />
        </label>
        {runMode === "live_candidate" ? <label className="eval-field">
          <span>{t("eval.workbench.candidateSource", "Candidate source")}</span>
          <Select
            aria-label={t("eval.workbench.candidateSource", "Candidate source")}
            value={candidateSource}
            options={[
              { label: t("eval.workbench.builtinCandidate", "Built-in Assistant"), value: "builtin" },
              { label: t("eval.workbench.agentVersionCandidate", "Immutable Agent version"), value: "agent_version" },
            ]}
            onChange={setCandidateSource}
          />
        </label> : null}
        {needsAgentVersion ? <>
          <label className="eval-field">
            <span>{t("eval.workbench.agent", "Agent")}</span>
            <Select
              aria-label={t("eval.workbench.agent", "Agent")}
              showSearch
              optionFilterProp="label"
              loading={agentsQuery.isLoading}
              value={selectedAgentId}
              options={(agentsQuery.data?.items || []).map((agent) => ({ label: agent.name, value: agent.agent_id }))}
              onChange={(value) => { setSelectedAgentId(value); setSelectedAgentVersionId(undefined); }}
            />
          </label>
          <label className="eval-field">
            <span>{t("eval.workbench.agentVersion", "Immutable version")}</span>
            <Select
              aria-label={t("eval.workbench.agentVersion", "Immutable version")}
              showSearch
              optionFilterProp="label"
              loading={agentVersionsQuery.isLoading}
              disabled={!selectedAgentId}
              value={selectedAgentVersionId}
              options={(agentVersionsQuery.data || []).map((version) => ({
                label: `v${version.version_number} · ${version.spec_hash.slice(0, 12)}`,
                value: version.agent_version_id,
              }))}
              onChange={setSelectedAgentVersionId}
            />
          </label>
        </> : null}
        <label className="eval-field">
          <span>{t("eval.workbench.repetitions", "Repetitions")}</span>
          <InputNumber min={1} max={10} value={runMode === "live_candidate" ? repetitions : 1} disabled={runMode !== "live_candidate"} onChange={(value) => setRepetitions(value ?? 3)} />
        </label>
        <label className="eval-field eval-field-wide">
          <span>{t("eval.workbench.promptOverride", "System prompt override (optional)")}</span>
          <Input.TextArea
            value={systemPromptOverride}
            disabled={runMode !== "live_candidate" || needsAgentVersion}
            placeholder={t("eval.workbench.promptOverrideHint", "Leave empty to evaluate the deployed prompt")}
            autoSize={{ minRows: 2, maxRows: 6 }}
            onChange={(event) => setSystemPromptOverride(event.target.value)}
          />
        </label>
      </div>
      {needsAgentVersion ? <>
        {agentsQuery.error || agentVersionsQuery.error ? <Alert type="error" showIcon title={t("eval.workbench.agentCatalogUnavailable", "Agent versions could not be loaded")}
          description={toError(agentsQuery.error || agentVersionsQuery.error).message}
          action={<Button onClick={() => void Promise.all([agentsQuery.refetch(), agentVersionsQuery.refetch()])}>{t("common.retry", "Retry")}</Button>} /> : null}
        <Space size={8} wrap>
          <Button
            disabled={!selectedAgent || selectedAgent.caller_role !== "owner" || !selectedAgent.draft_revision || selectedAgent.status === "archived"}
            loading={versionMutation.isPending}
            onClick={() => versionMutation.mutate()}
          >{t("eval.workbench.snapshotDraft", "Snapshot saved draft as immutable version")}</Button>
          {selectedAgentId ? <Link to={`/agents/${selectedAgentId}`}>{t("eval.workbench.editAgentDraft", "Edit and save Agent draft")}</Link> : null}
        </Space>
        {selectedAgentVersion ? <Descriptions size="small" bordered column={{ xs: 1, sm: 2 }} items={[
          { key: "version", label: t("eval.workbench.agentVersion", "Immutable version"), children: `v${selectedAgentVersion.version_number} · ${selectedAgentVersion.agent_version_id}` },
          { key: "model", label: t("eval.workbench.configuredModel", "Configured model"), children: selectedAgentVersion.spec.model.model_id || t("agents.common.serverDefault", "Server default") },
          { key: "knowledge", label: t("eval.workbench.configuredKnowledge", "Configured knowledge"), children: selectedAgentVersion.spec.knowledge.map((item) => item.dataset_id).join(", ") || "—" },
          { key: "scope", label: t("eval.workbench.scopeNote", "Runtime scope"), children: t("eval.workbench.scopeRechecked", "Model and knowledge access are resolved again when the run starts.") },
        ]} /> : null}
      </> : null}
      {missingRunInputs.length ? (
        <Alert
          type="warning"
          showIcon
          title={t("eval.workbench.missingRunInputs", "Select {{items}} before running.", { items: missingRunInputs.join(", ") })}
        />
      ) : null}
      <Space size={10} wrap>
        <Button
          type="primary"
          icon={<Play size={15} />}
          onClick={() => experimentBatchMutation.mutate()}
          loading={experimentBatchMutation.isPending}
          disabled={!canRunExperiment}
        >
          {needsAgentVersion ? t("eval.workbench.runAgentVersion", "Run immutable Agent version")
            : runMode === "live_candidate" ? t("eval.workbench.runCurrentAgent", "Run current Agent") : t("eval.workbench.rescoreStored", "Re-score stored traces")}
        </Button>
      </Space>
      <Descriptions
        className="eval-workbench-descriptions"
        size="small"
        bordered
        column={{ xs: 1, sm: 2, lg: 4 }}
        items={[
          { key: "mode", label: t("eval.workbench.runMode", "Run mode"), children: runMode },
          { key: "calls", label: t("eval.workbench.estimatedCalls", "Estimated calls"), children: effectiveRunDatasetId ? `Up to ${(examplesQuery.data?.total || 0) * (runMode === "live_candidate" ? repetitions : 1)} listed cases; only approved cases run` : "1 stored trace" },
          { key: "cost", label: "Cost", children: runMode === "live_candidate" ? "Actual cost unknown until usage is reported" : "Re-score uses the selected judge; cost depends on usage" },
          { key: "datasetVersion", label: "Dataset version", children: activeDataset?.version || "—" },
          { key: "baseline", label: t("eval.workbench.currentBaseline", "Current baseline"), children: activeExperiment?.baseline_run_id || t("eval.workbench.noBaseline", "Not set") },
          { key: "fingerprint", label: t("eval.workbench.candidateFingerprint", "Candidate fingerprint"), children: String(asRecord(latestRun?.metrics?.actual_fingerprint).system_prompt_hash || latestRun?.candidate_fingerprint?.prompt_override_hash || latestRun?.runtime_fingerprint?.prompt_hash || activeExperiment?.target_config?.prompt_hash || "—") },
        ]}
      />
      <div className="eval-comparison-controls">
        <label className="eval-field">
          <span>{t("eval.workbench.baselineRun", "Baseline run")}</span>
          <Select
            allowClear
            value={baselineRunId}
            placeholder={t("eval.workbench.selectBaseline", "Select baseline")}
            options={comparableRuns.map((run) => ({ label: `${run.status} · ${(run.created_at || run.run_id).slice(0, 19)}`, value: run.run_id, disabled: run.status !== "succeeded" }))}
            onChange={(value) => { setBaselineRunId(value); setRunComparison(null); setGateResult(null); }}
          />
        </label>
        <label className="eval-field">
          <span>{t("eval.workbench.candidateRun", "Candidate run")}</span>
          <Select
            allowClear
            value={candidateRunId}
            placeholder={t("eval.workbench.selectCandidate", "Select candidate")}
            options={comparableRuns.map((run) => ({ label: `${run.status} · ${(run.created_at || run.run_id).slice(0, 19)}`, value: run.run_id, disabled: run.status !== "succeeded" }))}
            onChange={(value) => {
              setCandidateRunId(value);
              setRunComparison(null);
              setGateResult(null);
              if (value) selectRun(value);
            }}
          />
        </label>
        <Button
          icon={<GitCompare size={15} />}
          onClick={compareSelectedRuns}
          loading={comparisonLoading}
          disabled={!canCompareRuns}
        >
          {t("eval.workbench.compareRuns", "Compare runs")}
        </Button>
        <Button
          onClick={() => modal.confirm({
            title: t("eval.workbench.promoteBaseline", "Set candidate as baseline?"),
            content: t("eval.workbench.promoteBaselineHint", "Only a successful compatible live run that passes critical gates can be promoted."),
            okText: t("eval.workbench.confirmPromote", "Promote baseline"),
            onOk: () => promoteBaselineMutation.mutateAsync(),
          })}
          loading={promoteBaselineMutation.isPending}
          disabled={
            !canRunEvaluations
            || !candidateRun
            || candidateRun.status !== "succeeded"
            || (candidateRun.run_mode || candidateRun.target_snapshot?.run_mode) !== "live_candidate"
            || candidateRunId === activeExperiment?.baseline_run_id
          }
        >
          {t("eval.workbench.setBaseline", "Set as baseline")}
        </Button>
      </div>
      {comparisonError ? (
        <Alert
          type="error"
          showIcon
          title={t("eval.workbench.compareFailed", "Could not compare these runs")}
          description={comparisonError.message}
        />
      ) : null}
      <ExperimentRunComparison
        comparison={runComparison?.baseline_run_id === baselineRunId
          && runComparison?.candidate_run_id === candidateRunId ? runComparison : null}
        runs={comparableRuns}
        baselineRunId={activeExperiment?.baseline_run_id || baselineRunId}
        onOpenTrace={openComparedTrace}
      />
      {latestRunAgentTarget ? <Descriptions
        className="eval-workbench-descriptions"
        size="small"
        bordered
        column={{ xs: 1, sm: 2 }}
        items={[
          { key: "agent", label: t("eval.workbench.actualAgent", "Executed Agent"), children: <Link to={`/agents/${latestRunAgentTarget.agent_id}/versions`}>{latestRunAgentTarget.agent_id}</Link> },
          { key: "version", label: t("eval.workbench.actualVersion", "Executed immutable version"), children: latestRunAgentTarget.agent_version_id },
          { key: "model", label: t("eval.workbench.actualModel", "Resolved model"), children: latestRunAgentTarget.model_id || "—" },
          { key: "knowledge", label: t("eval.workbench.actualKnowledge", "Resolved knowledge IDs"), children: latestRunAgentTarget.knowledge_dataset_ids?.join(", ") || "—" },
          { key: "spec", label: t("eval.workbench.actualSpecHash", "Executed spec hash"), children: latestRunAgentTarget.agent_spec_hash || "—" },
          { key: "runtime", label: t("eval.workbench.actualRuntimeHash", "Runtime snapshot hash"), children: latestRunAgentTarget.agent_runtime_snapshot_hash || "—" },
          { key: "publication", label: t("eval.workbench.publicationReadback", "Publication readback"), children: agentPublicationsQuery.isLoading
            ? t("eval.workbench.checkingPublication", "Checking exact version…")
            : agentPublicationsQuery.isError ? t("eval.workbench.publicationUnavailable", "Publication state unavailable")
              : exactVersionPublications.length ? exactVersionPublications.map((publication) => (
                <span key={publication.publication_id}>{publication.channel}: {publication.publication_id} </span>
              )) : t("eval.workbench.notPublishedVersion", "No active publication points to this exact version") },
        ]}
      /> : null}
      {runActions.notice ? <Alert showIcon type={runActions.notice.warning ? "warning" : "info"}
        title={runActions.notice.text} /> : null}
      <ExperimentRunResults
        run={latestRun && latestRun.run_id === queuedRunId ? latestRun : null}
        results={latestRun?.run_id === queuedRunId ? runResultsQuery.data || null : null}
        loading={selectedRunQuery.isFetching || runResultsQuery.isFetching}
        error={selectedRunQuery.error ? toError(selectedRunQuery.error) : runResultsQuery.error ? toError(runResultsQuery.error) : null}
        onRetry={() => {
          void selectedRunQuery.refetch();
          void runResultsQuery.refetch();
        }}
        onOpenTrace={openRunResultTrace}
        actionLoading={runActions.pending}
        onCancel={canRunEvaluations ? runActions.cancel : undefined}
        onRetryFailedCases={canRunEvaluations ? runActions.retry : undefined}
      />
      <details className="eval-run-advanced">
        <summary>{t("eval.workbench.createOrConfigure", "Create or configure an experiment")}</summary>
        <div className="eval-workbench-form-grid">
          <Input
            aria-label={t("eval.workbench.experimentName", "Experiment name")}
            value={experimentDraft.name}
            onChange={(event) => setExperimentDraft((draft) => ({ ...draft, name: event.target.value }))}
          />
          <Input
            aria-label={t("eval.workbench.experimentDescription", "Experiment description")}
            value={experimentDraft.description}
            onChange={(event) => setExperimentDraft((draft) => ({ ...draft, description: event.target.value }))}
          />
          <Input.TextArea
            className="eval-workbench-textarea eval-workbench-wide"
            aria-label={t("eval.workbench.targetConfig", "Target config JSON")}
            value={experimentDraft.targetConfigText}
            autoSize={{ minRows: 3, maxRows: 6 }}
            onChange={(event) => setExperimentDraft((draft) => ({ ...draft, targetConfigText: event.target.value }))}
          />
          <Button
            icon={<Beaker size={15} />}
            onClick={() => experimentMutation.mutate()}
            loading={experimentMutation.isPending}
            disabled={!canRunEvaluations}
          >
            {t("eval.workbench.createExperiment")}
          </Button>
        </div>
      </details>
    </WorkbenchPanel>
  );

  const evaluatorsTab = (
    <WorkbenchPanel
      title={t("eval.workbench.evaluators")}
      description={t("eval.workbench.evaluatorsDescription")}
      icon={<Sparkles size={22} />}
    >
      <div className="eval-evaluator-config-grid">
        <Select
          className="eval-workbench-wide"
          allowClear
          aria-label={t("eval.workbench.selectEvaluator", "Select evaluator")}
          placeholder={t("eval.workbench.selectEvaluator", "Select evaluator")}
          value={selectedEvaluatorId}
          options={evaluators.map((evaluator) => ({
            label: `${evaluator.name} (${evaluator.version})`,
            value: evaluator.evaluator_id,
          }))}
          onChange={(value) => setSelectedEvaluatorId(value)}
        />
        <Input
          value={evaluatorDraft.name}
          aria-label={t("eval.workbench.evaluatorName")}
          onChange={(event) => setEvaluatorDraft((draft) => ({ ...draft, name: event.target.value }))}
        />
        <Select
          value={evaluatorDraft.evaluator_type}
          aria-label={t("eval.workbench.evaluatorType")}
          options={evaluatorTypeOptions}
          onChange={(value: EvalEvaluator["evaluator_type"]) => {
            const preset = evaluatorPresetForType(value, activeTraceFamily, (key, fallback) =>
              fallback === undefined ? t(key) : t(key, fallback));
            setEvaluatorDraft((draft) => ({
              ...draft,
              evaluator_type: value,
              rubric: preset.rubric,
              filterConfigText: preset.filterConfigText,
              samplingConfigText: preset.samplingConfigText,
            }));
          }}
        />
        <Input
          value={evaluatorDraft.version}
          aria-label={t("eval.workbench.version")}
          onChange={(event) => setEvaluatorDraft((draft) => ({ ...draft, version: event.target.value }))}
        />
        <Input.TextArea
          className="eval-workbench-textarea eval-workbench-wide"
          aria-label={t("eval.workbench.rubric", "Rubric")}
          value={evaluatorDraft.rubric}
          autoSize={{ minRows: 3, maxRows: 6 }}
          onChange={(event) => setEvaluatorDraft((draft) => ({ ...draft, rubric: event.target.value }))}
        />
        <Input.TextArea
          className="eval-workbench-textarea"
          aria-label={t("eval.workbench.samplingConfig", "Sampling config JSON")}
          value={evaluatorDraft.samplingConfigText}
          autoSize={{ minRows: 2, maxRows: 5 }}
          onChange={(event) => setEvaluatorDraft((draft) => ({ ...draft, samplingConfigText: event.target.value }))}
        />
        <Input.TextArea
          className="eval-workbench-textarea"
          aria-label={t("eval.workbench.filterConfig", "Filter config JSON")}
          value={evaluatorDraft.filterConfigText}
          autoSize={{ minRows: 2, maxRows: 5 }}
          onChange={(event) => setEvaluatorDraft((draft) => ({ ...draft, filterConfigText: event.target.value }))}
        />
      </div>
      <Space size={10} wrap>
        <Button
          type="primary"
          icon={<Sparkles size={15} />}
          onClick={() => evaluatorMutation.mutate()}
          loading={evaluatorMutation.isPending}
          disabled={!canRunEvaluations}
        >
          {t("eval.workbench.createEvaluator")}
        </Button>
        <Button
          icon={<Play size={15} />}
          onClick={() => evaluatorRunMutation.mutate()}
          loading={evaluatorRunMutation.isPending}
          disabled={!canRunEvaluations || !activeEvaluator}
        >
          {t("eval.workbench.queueEvaluator")}
        </Button>
      </Space>
      <Descriptions
        className="eval-workbench-descriptions"
        size="small"
        bordered
        column={1}
        items={[
          { key: "evaluator", label: t("eval.workbench.currentEvaluator"), children: activeEvaluator?.name || "-" },
          { key: "version", label: t("eval.workbench.version"), children: activeEvaluator?.version || "-" },
          { key: "selected", label: t("eval.workbench.selectedTrace"), children: selectedTraceId || "-" },
          { key: "listed", label: t("eval.workbench.listedEvaluators", "Listed evaluators"), children: String(evaluatorsQuery.data?.total ?? 0) },
        ]}
      />
    </WorkbenchPanel>
  );

  const pendingExamples = latestReviewCandidates(reviewExamplesQuery.data?.examples || []);
  const reviewPageCount = Math.max(1, Math.ceil(pendingExamples.length / 8));
  const currentReviewPage = Math.min(reviewPage, reviewPageCount);

  const reviewQueueTab = (
    <WorkbenchPanel
      title={t("eval.workbench.reviewQueue", "Review Queue")}
      description={t("eval.workbench.reviewQueueDescription", "Approve golden candidates and judge disagreements before they affect regression gates.")}
      icon={<CheckCircle2 size={22} />}
    >
      <Space size={10} wrap>
        <Button
          icon={<CheckCircle2 size={15} />}
          onClick={addSelectedTraceToReview}
          loading={exampleMutation.isPending}
          disabled={!canRunEvaluations || !activeDataset || !selectedTraceId}
        >
          {t("eval.workbench.addToReview", "Add to Review")}
        </Button>
      </Space>
      <div className="eval-review-list">
        {reviewExamplesQuery.isLoading ? (
          <Alert type="info" showIcon title={t("knowledge.detail.failureReviewLoading")} />
        ) : reviewExamplesQuery.isError ? (
          <Alert
            type="error"
            showIcon
            title={t("knowledge.detail.failureReviewLoadFailed")}
            action={<Button onClick={() => void reviewExamplesQuery.refetch()}>{t("common.retry")}</Button>}
          />
        ) : pendingExamples.length > 0 ? pendingExamples.slice((currentReviewPage - 1) * 8, currentReviewPage * 8).map((example) => {
          const sourceVersions = kbFailureSourceVersions(example);
          const kbTraceId = example.metadata?.kb_trace_id;
          const kbDatasetId = example.metadata?.kb_dataset_id;
          const isKbFailure = example.metadata?.source_kind === "kb_failure";
          const canApprove = canApproveReviewExample(example);
          const busy = reviewBusyIds.has(example.example_id) || reviewMutation.isPending;
          return (
            <article className="eval-review-row" key={example.example_id}>
              <div>
                <strong>{String(example.metadata?.case_id || example.example_id)}</strong>
                <span>{String(example.metadata?.review_status || "pending")} · {example.split}
                  {isKbFailure && ` · v${String(example.metadata?.case_revision || 1)}`}
                </span>
                {isKbFailure && (
                  <div className="mt-2 space-y-1 text-xs">
                    <p>{t("knowledge.detail.failureQuestion")}: {String(example.input?.query || "")}</p>
                    <p>{t("knowledge.detail.failureExpectedAnswer")}: {String(example.expected_output?.answer || "")}</p>
                    {typeof example.metadata?.kb_observed_answer === "string" && (
                      <p>{t("knowledge.detail.failureObservedAnswer")}: {example.metadata.kb_observed_answer}</p>
                    )}
                    {typeof example.metadata?.kb_failure_reason === "string" && (
                      <p>{t("knowledge.detail.failureReason")}: {example.metadata.kb_failure_reason}</p>
                    )}
                    {sourceVersions.length > 0 ? sourceVersions.map((source) => (
                      <p key={`${source.document_id}:${source.segment_id}:${source.source_version}`}>
                        {t(example.metadata?.kb_source_versions_verified === true
                          ? "knowledge.detail.failureVerifiedSource"
                          : "knowledge.detail.failureRecordedSource")}: {source.document_id} · v{source.source_version} · {source.source_hash.slice(0, 12)}…{" "}
                        <a
                          href={`/knowledge/${encodeURIComponent(source.kb_dataset_id)}?tab=documents&document_id=${encodeURIComponent(source.document_id)}`}
                        >
                          {t("knowledge.detail.failureOpenDocument")}
                        </a>
                      </p>
                    )) : <p>{t("knowledge.detail.failureNoStableSource")}</p>}
                    {example.source_trace_id && typeof kbDatasetId === "string" ? (
                      <a href={`/eval?dataset_id=${encodeURIComponent(kbDatasetId)}&tab=traces&family=rag&trace_id=${encodeURIComponent(example.source_trace_id)}`}>
                        {t("knowledge.detail.failureOpenTrace")}
                      </a>
                    ) : typeof kbTraceId === "string" && kbTraceId ? (
                      <p>{t("knowledge.detail.failureTraceUnconfirmed")}</p>
                    ) : null}
                    {!canApprove && <p role="note">{t("knowledge.detail.failureApprovalNeedsTrace")}</p>}
                    <p role="note">{t("knowledge.detail.failureApprovalDoesNotRun")}</p>
                  </div>
                )}
              </div>
              <Space size={8}>
                <Button size="small" data-testid={`review-approve-${example.example_id}`} disabled={!canRunEvaluations || busy || !canApprove} onClick={() => submitReview(example, "approved")}>
                  {t("common.approve", "Approve")}
                </Button>
                <Button size="small" data-testid={`review-needs-fix-${example.example_id}`} disabled={!canRunEvaluations || busy} onClick={() => submitReview(example, "needs_fix")}>
                  {t("common.review", "Needs fix")}
                </Button>
              </Space>
            </article>
          );
        }) : (
          <Alert type="info" showIcon title={t("eval.workbench.reviewEmpty", "No pending review cases for the active dataset")} />
        )}
      </div>
      {pendingExamples.length > 8 && (
        <Pagination
          className="mt-3"
          current={currentReviewPage}
          pageSize={8}
          total={pendingExamples.length}
          showSizeChanger={false}
          onChange={setReviewPage}
          aria-label={t("knowledge.detail.failureReviewPages")}
        />
      )}
    </WorkbenchPanel>
  );

  const kbRagasTab = (
    <KbRagasPanel
      traces={visibleTraces}
      traceTotal={tracesQuery.data?.total || 0}
      selectedTraceId={selectedTraceId}
      detail={detailQuery.data}
      detailLoading={detailQuery.isLoading || detailQuery.isFetching}
      ragasEvaluators={ragasEvaluators}
      selectedEvaluatorId={activeRagasEvaluator?.evaluator_id}
      onSelectEvaluator={setSelectedEvaluatorId}
      onQueueEvaluator={() => ragasEvaluatorRunMutation.mutate()}
      queueLoading={ragasEvaluatorRunMutation.isPending}
      onCreateRagasEvaluator={() => createRagasEvaluatorMutation.mutate()}
      createLoading={createRagasEvaluatorMutation.isPending}
      initialDatasetId={kbDatasetFilter}
      canRunEvaluations={canRunEvaluations}
    />
  );

  const gatesTab = (
    <WorkbenchPanel
      title={t("eval.workbench.gates", "Gates")}
      description={t("eval.workbench.gatesDescription", "Dry-run the offline regression thresholds before they enter CI.")}
      icon={<ShieldCheck size={22} />}
    >
      <Space size={10} wrap>
        <Button
          type="primary"
          icon={<ShieldCheck size={15} />}
          onClick={() => gateMutation.mutate()}
          loading={gateMutation.isPending}
          disabled={!canRunEvaluations || !canDryRunGate}
        >
          {t("eval.workbench.dryRunGate", "Dry-run gate")}
        </Button>
      </Space>
      {!canDryRunGate ? (
        <Alert
          type="info"
          showIcon
          title={t("eval.workbench.gateNeedsRun", "Select a successful run with complete gate metrics before running the gate.")}
        />
      ) : null}
      <Descriptions
        className="eval-workbench-descriptions"
        size="small"
        bordered
        column={1}
        items={[
          { key: "status", label: t("eval.workbench.gateStatus", "Gate status"), children: gateResult?.status || "not_run" },
          { key: "overall", label: t("eval.workbench.overallScore", "Overall score"), children: String(gateResult?.metrics?.overall_score ?? "-") },
          { key: "trajectory", label: t("eval.workbench.trajectoryPass", "Trajectory pass"), children: String(gateResult?.metrics?.trajectory_pass_rate ?? "-") },
          { key: "critical", label: t("eval.workbench.criticalPass", "Critical pass"), children: String(gateResult?.metrics?.critical_pass_rate ?? "-") },
          { key: "failures", label: t("eval.workbench.failures", "Failures"), children: gateResult?.failures.join(", ") || "-" },
        ]}
      />
    </WorkbenchPanel>
  );

  const runAndResultsTab = (
    <Tabs
      className="eval-subtabs"
      activeKey={activeRunTab}
      onChange={(key) => {
        setActiveRunTab(key);
        if (key === "rag_quality" && activeTraceFamily !== "rag") {
          handleTraceFamilyChange("rag");
        }
      }}
      items={[
        {
          key: "experiment_runs",
          label: t("eval.workbench.testRuns", "Test runs"),
          children: experimentsTab,
        },
        {
          key: "rag_quality",
          label: t("eval.ragas.tab", "RAG quality"),
          children: kbRagasTab,
        },
      ]}
    />
  );

  const assetsTab = (
    <Tabs
      className="eval-subtabs"
      activeKey={activeAssetsTab}
      onChange={setActiveAssetsTab}
      items={[
        { key: "golden_sets", label: t("eval.workbench.goldenSets", "Golden Sets"), children: goldenSetsTab },
        { key: "evaluators", label: t("eval.workbench.evaluators"), children: evaluatorsTab },
        { key: "review_queue", label: t("eval.workbench.reviewQueue", "Review Queue"), children: reviewQueueTab },
      ]}
    />
  );

  return (
    <main className="eval-console" data-testid="eval-console">
      <div className="eval-page-heading">
        <div>
          <div className="eval-eyebrow">
            <SearchCheck size={15} />
            {t("eval.eyebrow")}
          </div>
          <p>{t("eval.description")}</p>
        </div>
        <div className="eval-latency-note">
          <Activity size={16} />
          {t("eval.latencyNote")}
        </div>
      </div>

      <Tabs
        className="eval-tabs"
        activeKey={activeWorkbenchTab}
        onChange={handleWorkbenchTabChange}
        items={[
          {
            key: "overview",
            label: t("eval.workbench.overview", "Overview"),
            children: overviewTab,
          },
          {
            key: "runs",
            label: t("eval.workbench.runAndResults", "Run & Results"),
            children: runAndResultsTab,
          },
          {
            key: "traces",
            label: t("eval.workbench.traces", "Traces"),
            children: traceExplorerTab,
          },
          {
            key: "assets",
            label: t("eval.workbench.assets", "Assets"),
            children: assetsTab,
          },
          {
            key: "gates",
            label: t("eval.workbench.gates", "Gates"),
            children: gatesTab,
          },
        ]}
      />

      <style>{`
        .eval-console {
          display: flex;
          min-height: calc(100vh - 112px);
          flex-direction: column;
          min-width: 0;
          color: hsl(var(--foreground));
        }
        .eval-page-heading {
          display: flex;
          justify-content: space-between;
          align-items: flex-start;
          gap: 16px;
          margin-bottom: 14px;
        }
        .eval-page-heading h2 {
          margin: 4px 0 4px;
          font-size: 24px;
          line-height: 1.18;
          font-weight: 720;
          letter-spacing: 0;
        }
        .eval-page-heading p {
          margin: 0;
          color: hsl(var(--muted-foreground));
          font-size: 13px;
        }
        .eval-platform-summary-block {
          display: grid;
          grid-template-columns: 1fr 1.5fr;
          gap: 12px;
          margin-bottom: 12px;
        }
        @media (max-width: 900px) {
          .eval-platform-summary-block {
            grid-template-columns: 1fr;
          }
        }
        .eval-metric-group {
          border: 1px solid hsl(var(--border));
          border-radius: 10px;
          background: hsl(var(--card));
          padding: 14px 18px;
          display: flex;
          flex-direction: column;
          gap: 10px;
        }
        .eval-metric-group-title {
          font-size: 11px;
          font-weight: 600;
          text-transform: uppercase;
          letter-spacing: 0.04em;
          color: hsl(var(--muted-foreground));
        }
        .eval-metric-items {
          display: grid;
          grid-template-columns: repeat(auto-fit, minmax(70px, 1fr));
          gap: 10px;
        }
        .eval-metric-item {
          display: flex;
          flex-direction: column;
          gap: 2px;
          min-width: 0;
        }
        .eval-platform-grid {
          display: grid;
          grid-template-columns: 1fr;
          gap: 10px;
        }
        .eval-platform-wide {
          grid-column: 1 / -1;
        }
        .eval-overview-bars {
          display: grid;
          grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
          gap: 12px;
          margin-bottom: 12px;
        }
        .eval-overview-bars span {
          display: block;
          margin-bottom: 4px;
          color: hsl(var(--muted-foreground));
          font-size: 12px;
        }
        .eval-summary-card {
          display: flex;
          flex-direction: column;
          gap: 4px;
          padding: 12px 14px;
          border: 1px solid hsl(var(--border));
          border-radius: 12px;
          background: hsl(var(--card));
        }
        .eval-summary-label {
          font-size: 11px;
          color: hsl(var(--muted-foreground));
          letter-spacing: 0.02em;
        }
        .eval-summary-value {
          font-size: 18px;
          line-height: 1.2;
          font-weight: 700;
        }
        .eval-eyebrow,
        .eval-latency-note {
          display: inline-flex;
          align-items: center;
          gap: 7px;
          color: hsl(var(--primary));
          font-size: 12px;
          font-weight: 650;
          letter-spacing: 0;
        }
        .eval-latency-note {
          max-width: 340px;
          padding: 9px 12px;
          border-radius: 8px;
          background: hsl(var(--primary) / 0.08);
          color: hsl(var(--foreground));
          box-shadow: 0 8px 28px rgb(18 24 38 / 0.08);
        }
        .eval-tabs > .ant-tabs-nav {
          margin-bottom: 10px;
        }
        .eval-tabs,
        .eval-tabs > .ant-tabs-content-holder,
        .eval-tabs > .ant-tabs-content-holder > .ant-tabs-content,
        .eval-tabs > .ant-tabs-content-holder > .ant-tabs-content > .ant-tabs-tabpane {
          min-height: 0;
        }
        .eval-tabs {
          display: flex;
          flex: 1;
          flex-direction: column;
        }
        .eval-tabs > .ant-tabs-content-holder {
          flex: 1;
        }
        .eval-tabs > .ant-tabs-content-holder > .ant-tabs-content,
        .eval-tabs > .ant-tabs-content-holder > .ant-tabs-content > .ant-tabs-tabpane-active {
          height: 100%;
        }
        .eval-tabs .ant-tabs-tab {
          border-radius: 8px;
          padding-inline: 12px;
        }
        .eval-family-tabs {
          display: flex;
          min-height: 0;
          height: 100%;
          flex-direction: column;
        }
        .eval-family-tabs > .ant-tabs-nav {
          flex: 0 0 auto;
          margin-bottom: 10px;
        }
        .eval-family-tabs > .ant-tabs-content-holder {
          flex: 1;
          min-height: 0;
        }
        .eval-family-tabs > .ant-tabs-content-holder > .ant-tabs-content,
        .eval-family-tabs > .ant-tabs-content-holder > .ant-tabs-content > .ant-tabs-tabpane-active {
          height: 100%;
        }
        .eval-workbench-actions {
          display: flex;
          flex: 0 0 auto;
          justify-content: space-between;
          align-items: center;
          gap: 10px;
          margin-bottom: 10px;
          min-width: 0;
        }
        .eval-family-coverage-note {
          margin-bottom: 10px;
        }
        .eval-assistant-grid {
          display: grid;
          grid-template-columns: minmax(360px, 0.95fr) minmax(460px, 1.22fr) minmax(292px, 0.62fr);
          gap: 12px;
          align-items: stretch;
          height: min(760px, calc(100vh - 202px));
          min-height: 560px;
          min-width: 0;
        }
        .eval-workbench-panel {
          min-height: min(560px, calc(100vh - 220px));
        }
        .eval-workbench-icon {
          display: inline-flex;
          align-items: center;
          justify-content: center;
          width: 34px;
          height: 34px;
          border-radius: 8px;
          background: hsl(var(--primary) / 0.08);
          color: hsl(var(--primary));
        }
        .eval-workbench-body {
          display: flex;
          flex-direction: column;
          gap: 14px;
          padding: 14px;
          min-width: 0;
        }
        .eval-workbench-descriptions {
          max-width: 760px;
        }
        .eval-workbench-descriptions .ant-descriptions-item-content {
          word-break: break-all;
        }
        .eval-workbench-form-grid,
        .eval-evaluator-config-grid {
          display: grid;
          grid-template-columns: repeat(2, minmax(0, 280px));
          gap: 10px;
          max-width: 760px;
        }
        .eval-workbench-wide {
          grid-column: 1 / -1;
        }
        .eval-review-list {
          display: grid;
          gap: 8px;
          margin-top: 12px;
        }
        .eval-review-row {
          display: flex;
          align-items: center;
          justify-content: space-between;
          gap: 12px;
          padding: 10px 12px;
          border: 1px solid hsl(var(--border));
          border-radius: 8px;
          background: hsl(var(--card));
        }
        .eval-review-row div {
          display: grid;
          gap: 2px;
          min-width: 0;
        }
        .eval-review-row span {
          color: hsl(var(--muted-foreground));
          font-size: 12px;
        }
        .eval-workbench-textarea textarea {
          font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
          font-size: 12px;
          line-height: 1.45;
        }
        .eval-panel {
          display: flex;
          min-height: 0;
          flex-direction: column;
          min-width: 0;
          border: 1px solid hsl(var(--border) / 0.72);
          border-radius: 8px;
          background: hsl(var(--card));
          box-shadow: 0 1px 2px rgb(20 24 32 / 0.04), 0 8px 28px rgb(20 24 32 / 0.06);
          overflow: hidden;
        }
        .eval-panel-heading {
          display: flex;
          flex: 0 0 auto;
          align-items: flex-start;
          justify-content: space-between;
          gap: 10px;
          padding: 14px 14px 10px;
          border-bottom: 1px solid hsl(var(--border) / 0.72);
        }
        .eval-panel-heading h2 {
          margin: 0;
          font-size: 14px;
          line-height: 1.2;
          font-weight: 700;
          letter-spacing: 0;
        }
        .eval-panel-heading p {
          margin: 3px 0 0;
          color: hsl(var(--muted-foreground));
          font-size: 12px;
        }
        .eval-filter-grid {
          display: grid;
          flex: 0 0 auto;
          grid-template-columns: repeat(2, minmax(0, 1fr));
          gap: 8px;
          padding: 12px 14px;
          border-bottom: 1px solid hsl(var(--border) / 0.6);
        }
        .eval-date-range {
          grid-column: span 2;
        }
        .eval-transcript-filter {
          grid-column: span 2;
        }
        .eval-trace-table {
          display: flex;
          min-height: 0;
          flex: 1;
          flex-direction: column;
          padding: 0 10px 12px;
        }
        .eval-trace-table .ant-spin-nested-loading,
        .eval-trace-table .ant-spin-container,
        .eval-trace-table .ant-table,
        .eval-trace-table .ant-table-container {
          min-height: 0;
          height: 100%;
        }
        .eval-trace-table .ant-spin-container {
          display: flex;
          flex-direction: column;
        }
        .eval-trace-table .ant-table-wrapper,
        .eval-trace-table .ant-table {
          flex: 1;
        }
        .eval-trace-table .ant-table-body {
          max-height: none !important;
        }
        .eval-trace-table .ant-table {
          border-radius: 8px;
          overflow: hidden;
        }
        .eval-trace-table .ant-table-row {
          cursor: pointer;
        }
        .eval-trace-table .ant-table-row:focus-visible {
          outline: 2px solid hsl(var(--ring));
          outline-offset: -2px;
        }
        .eval-trace-row-selected > td {
          background: hsl(var(--primary) / 0.08) !important;
        }
        .eval-trace-id-cell {
          min-width: 0;
        }
        .eval-trace-id-line,
        .eval-trace-model {
          max-width: 220px;
          overflow: hidden;
          text-overflow: ellipsis;
          white-space: nowrap;
          font-size: 12px;
          font-weight: 650;
        }
        .eval-trace-subline {
          max-width: 220px;
          overflow: hidden;
          text-overflow: ellipsis;
          white-space: nowrap;
          color: hsl(var(--muted-foreground));
          font-size: 11px;
          margin-top: 2px;
        }
        .eval-trace-metric {
          font-size: 12px;
          font-weight: 700;
        }
        .eval-status-tag {
          margin: 0;
          border-radius: 5px;
        }
        .eval-state-panel,
        .eval-detail-empty {
          flex: 1;
          min-height: 240px;
          display: flex;
          align-items: center;
          justify-content: center;
          gap: 10px;
          padding: 24px;
          color: hsl(var(--muted-foreground));
        }
        .eval-trace-detail {
          overflow-y: auto;
          padding-bottom: 14px;
        }
        .eval-redaction-banner {
          margin: 12px 14px;
        }
        .eval-metric-grid {
          display: grid;
          grid-template-columns: repeat(4, minmax(0, 1fr));
          gap: 8px;
          padding: 0 14px 12px;
        }
        .eval-metric-card {
          min-width: 0;
          border-radius: 8px;
          background: hsl(var(--muted) / 0.55);
          padding: 10px;
        }
        .eval-metric-card span {
          display: block;
          color: hsl(var(--muted-foreground));
          font-size: 11px;
        }
        .eval-metric-card strong {
          display: block;
          margin-top: 2px;
          font-size: 15px;
        }
        .eval-descriptions {
          margin: 0 14px 12px;
        }
        .eval-descriptions .ant-descriptions-item-label {
          width: 92px;
          white-space: nowrap;
        }
        .eval-descriptions .ant-descriptions-item-content {
          min-width: 0;
          word-break: break-all;
        }
        .eval-locator-panel {
          margin: 0 0 12px;
        }
        .eval-locator-panel .eval-preview-grid {
          margin-bottom: 0;
        }
        .eval-locator-descriptions .ant-descriptions-item-label {
          width: 110px;
        }
        .eval-section-title-compact {
          margin-top: 4px;
        }
        .eval-preview-grid {
          display: grid;
          grid-template-columns: repeat(2, minmax(0, 1fr));
          gap: 8px;
          margin: 0 14px 12px;
        }
        .eval-preview-block {
          min-width: 0;
          border-radius: 8px;
          background: hsl(var(--muted) / 0.46);
          padding: 10px;
        }
        .eval-preview-label {
          margin-bottom: 5px;
          color: hsl(var(--muted-foreground));
          font-size: 11px;
          font-weight: 650;
        }
        .eval-preview-text {
          margin-bottom: 0 !important;
          font-size: 12px;
          overflow-wrap: anywhere;
        }
        .eval-section-title {
          display: flex;
          justify-content: space-between;
          align-items: center;
          gap: 8px;
          margin: 16px 14px 10px;
        }
        .eval-section-title h3 {
          margin: 0;
          font-size: 13px;
          font-weight: 700;
        }
        .eval-section-title span {
          color: hsl(var(--muted-foreground));
          font-size: 11px;
        }
        .eval-timeline {
          padding: 0 14px;
        }
        .eval-timeline-card,
        .eval-event-row {
          min-width: 0;
          border: 1px solid hsl(var(--border) / 0.48);
          border-radius: 8px;
          background: hsl(var(--muted) / 0.4);
          padding: 10px;
        }
        .eval-timeline-title,
        .eval-event-main {
          display: flex;
          justify-content: space-between;
          align-items: flex-start;
          gap: 8px;
          min-width: 0;
          font-size: 12px;
          font-weight: 700;
        }
        .eval-timeline-meta {
          color: hsl(var(--muted-foreground));
          font-size: 11px;
          font-weight: 400;
        }
        .eval-json-block {
          max-height: 180px;
          overflow: auto;
          margin: 8px 0 0;
          border-radius: 8px;
          background: hsl(var(--background));
          color: hsl(var(--foreground));
          padding: 9px;
          font-size: 11px;
          line-height: 1.45;
          white-space: pre-wrap;
          overflow-wrap: anywhere;
        }
        .eval-event-list {
          display: flex;
          flex-direction: column;
          gap: 8px;
          padding: 0 14px;
        }
        .eval-event-type {
          overflow-wrap: anywhere;
        }
        .eval-score-panel {
          overflow: hidden;
          padding-bottom: 14px;
        }
        .eval-score-list {
          flex: 0 1 30%;
          max-height: 240px;
          min-height: 96px;
          overflow: auto;
          padding: 0 14px;
        }
        .eval-score-record {
          border-bottom: 1px solid hsl(var(--border) / 0.62);
          padding: 10px 0;
        }
        .eval-score-record:first-child {
          padding-top: 0;
        }
        .eval-score-record:last-child {
          border-bottom: 0;
          padding-bottom: 0;
        }
        .eval-score-state {
          display: flex;
          min-height: 96px;
          align-items: center;
          justify-content: center;
        }
        .eval-score-description {
          display: flex;
          flex-direction: column;
          gap: 2px;
          font-size: 12px;
        }
        .eval-score-form {
          margin-top: auto;
          padding: 12px 14px 0;
          border-top: 1px solid hsl(var(--border) / 0.62);
        }
        .eval-score-form-grid {
          display: grid;
          grid-template-columns: repeat(2, minmax(0, 1fr));
          gap: 8px;
        }
        .eval-future-panel {
          min-height: 360px;
          display: flex;
          flex-direction: column;
          align-items: center;
          justify-content: center;
          gap: 16px;
          padding: 28px;
          text-align: center;
        }
        .eval-future-icon {
          width: 54px;
          height: 54px;
          border-radius: 8px;
          display: inline-flex;
          align-items: center;
          justify-content: center;
          color: hsl(var(--primary));
          background: hsl(var(--primary) / 0.1);
        }
        .eval-future-copy {
          display: flex;
          flex-direction: column;
          gap: 4px;
        }
        .eval-future-copy span {
          color: hsl(var(--muted-foreground));
        }
        @media (max-width: 1280px) {
          .eval-assistant-grid {
            grid-template-columns: minmax(360px, 0.9fr) minmax(420px, 1.1fr);
            height: auto;
            min-height: 0;
          }
          .eval-score-panel {
            grid-column: 1 / -1;
            min-height: 420px;
          }
          .eval-trace-list,
          .eval-trace-detail {
            min-height: 620px;
            max-height: 720px;
          }
        }
        @media (max-width: 860px) {
          .eval-page-heading,
          .eval-panel-heading {
            flex-direction: column;
            align-items: stretch;
          }
          .eval-latency-note {
            max-width: none;
          }
          .eval-assistant-grid {
            grid-template-columns: minmax(0, 1fr);
            height: auto;
            min-height: 0;
          }
          .eval-trace-list,
          .eval-trace-detail,
          .eval-score-panel {
            min-height: 0;
            max-height: none;
          }
          .eval-trace-detail {
            overflow: visible;
          }
          .eval-score-list {
            max-height: 220px;
          }
          .eval-filter-grid,
          .eval-metric-grid,
          .eval-preview-grid,
          .eval-workbench-form-grid,
          .eval-evaluator-config-grid,
          .eval-score-form-grid {
            grid-template-columns: minmax(0, 1fr);
          }
          .eval-date-range {
            grid-column: span 1;
          }
          .eval-transcript-filter {
            grid-column: span 1;
          }
        }
        .dark .eval-latency-note,
        .dark .eval-panel {
          box-shadow: none;
        }
        .dark .eval-panel {
          border-color: hsl(var(--border) / 0.86);
        }
        .dark .eval-redaction-banner {
          background: hsl(var(--primary) / 0.12);
          border-color: hsl(var(--primary) / 0.28);
        }
        .dark .eval-metric-card,
        .dark .eval-preview-block,
        .dark .eval-timeline-card,
        .dark .eval-event-row {
          background: hsl(var(--muted) / 0.58);
        }
        .dark .eval-json-block {
          border: 1px solid hsl(var(--border) / 0.58);
          background: hsl(var(--background) / 0.72);
        }
        @media (prefers-reduced-motion: reduce) {
          .eval-panel,
          .eval-tabs .ant-tabs-tab {
            transition-duration: 1ms !important;
          }
        }
      `}</style>
    </main>
  );
}
