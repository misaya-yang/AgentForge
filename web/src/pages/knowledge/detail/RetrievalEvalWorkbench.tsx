import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  CheckCircle2,
  FileUp,
  FlaskConical,
  ListChecks,
  Loader2,
  Plus,
  Save,
  Search,
  Trash2,
  XCircle,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Textarea } from "@/components/ui/textarea";
import { toast } from "@/hooks/use-toast";
import {
  deleteEvalExample,
  importEvalExamples,
  listEvalDatasets,
  listEvalExamples,
  updateEvalExample,
} from "@/api/eval";
import {
  listRetrievalPresets,
  retrieve,
  retrieveEvaluate,
  validateRetrievalPresetConfig,
  type RetrievalEvalCase,
  type RetrievalEvalResponse,
  type RetrievalMetricsAtK,
  type RetrievalPreset,
} from "@/api/knowledge";
import {
  importGoldenCasesInBatches,
  parseGoldenJsonl,
  validateGoldenCases,
} from "@/pages/eval/goldenImport";
import {
  diffEvalCases,
  exampleToEvalCase,
  findKbEvalDatasetPaged,
  KB_EVAL_DATASET_LIST_LIMIT,
  KB_EVAL_DATASET_SOURCE,
  KB_EVAL_EXAMPLE_LIST_LIMIT,
  type PersistedEvalCase,
} from "./evalCaseStore";
import { resolveKbEvalDataset } from "./kbEvalDataset";

interface TestCase {
  id: string;
  query: string;
  relevantSegmentIds: string[];
  relevantSegmentInput: string;
  /** Server identity once persisted; absent for never-saved cases. */
  exampleId?: string;
  candidates?: Array<{ segment_id: string; score: number; text: string }>;
  loadingCandidates?: boolean;
  candidateError?: string;
}

interface GateThresholds {
  ndcg: number;
  recall: number;
  mrr: number;
}

interface EvaluationResult {
  a: RetrievalEvalResponse;
  b: RetrievalEvalResponse;
  presetA: Pick<RetrievalPreset, "name" | "label">;
  presetB: Pick<RetrievalPreset, "name" | "label">;
  testSetSignature: string;
}

type PresetStatus = "loading" | "ready" | "error";

const DEFAULT_K_VALUES = [1, 3, 5, 10];
const PRESET_TIMEOUT_MS = 15_000;
const CANDIDATE_TIMEOUT_MS = 30_000;
const EVALUATION_TIMEOUT_MS = 120_000;
const MAX_EVAL_CASES = 20;
const JSONL_MAX_FILE_BYTES = 8 * 1024 * 1024;
let fallbackCaseSequence = 0;

function pct(value: number | undefined | null): string {
  if (value === undefined || value === null || !Number.isFinite(value)) return "—";
  return `${(value * 100).toFixed(1)}%`;
}

function deltaPct(a: number | undefined, b: number | undefined): string {
  if (a === undefined || b === undefined || !Number.isFinite(a) || !Number.isFinite(b)) {
    return "—";
  }
  const delta = (b - a) * 100;
  return `${delta > 0 ? "+" : ""}${delta.toFixed(1)}`;
}

function createCaseId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return `case_${crypto.randomUUID()}`;
  }
  fallbackCaseSequence += 1;
  return `case_${Date.now().toString(36)}_${fallbackCaseSequence.toString(36)}`;
}

function isCanceledError(error: unknown): boolean {
  if (!error || typeof error !== "object") return false;
  const candidate = error as { code?: string; name?: string };
  return (
    candidate.code === "ERR_CANCELED" ||
    candidate.name === "CanceledError" ||
    candidate.name === "AbortError"
  );
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function signatureForCases(cases: TestCase[]): string {
  return JSON.stringify(
    cases.map((testCase) => ({
      id: testCase.id,
      query: testCase.query,
      relevantSegmentIds: [...testCase.relevantSegmentIds].sort(),
    }))
  );
}

function executionEvidence(response: RetrievalEvalResponse): string {
  const metadata = response.case_metadata ?? [];
  const pipelineCounts = new Map<string, number>();
  const appliedProviders = new Set<string>();
  let rerankAppliedCases = 0;
  let rerankFailureCases = 0;
  let rerankObservedCases = 0;

  for (const item of metadata) {
    const retrieval = item.retrieval_metadata ?? {};
    const pipeline = typeof retrieval.pipeline === "string" ? retrieval.pipeline : "unknown";
    pipelineCounts.set(pipeline, (pipelineCounts.get(pipeline) ?? 0) + 1);
    let rerankObserved = false;
    if (typeof retrieval.rerank_applied_provider === "string") {
      appliedProviders.add(retrieval.rerank_applied_provider);
      rerankAppliedCases += 1;
      rerankObserved = true;
    }
    if (retrieval.rerank_error || retrieval.rerank_fallback) {
      rerankFailureCases += 1;
      rerankObserved = true;
    }
    if (rerankObserved) rerankObservedCases += 1;
  }

  const totalCases = Math.max(response.num_cases, metadata.length);
  const missingMetadataCases = Math.max(totalCases - metadata.length, 0);
  if (missingMetadataCases > 0) {
    pipelineCounts.set("unknown", (pipelineCounts.get("unknown") ?? 0) + missingMetadataCases);
  }
  const pipelineText = pipelineCounts.size > 0
    ? [...pipelineCounts.entries()].map(([name, count]) => `${name}(${count})`).join(", ")
    : "unknown";
  const rerankRequested = response.requested_config?.rerank === true;
  const providerText = appliedProviders.size > 0 ? [...appliedProviders].sort().join(",") : "none";
  const unconfirmedCases = Math.max(totalCases - rerankObservedCases, 0);
  const rerankText = [
    `rerank requested=${rerankRequested ? "yes" : "no"}`,
    `applied cases=${rerankAppliedCases}/${totalCases} providers=${providerText}`,
    `fallback/failure cases=${rerankFailureCases}/${totalCases}`,
    `unconfirmed cases=${unconfirmedCases}/${totalCases}`,
  ].join("; ");
  return `pipeline=${pipelineText}; ${rerankText}`;
}

/**
 * Labelled retrieval evaluation over the backend's canonical preset contract.
 * requested_config records normalized intent, while case_metadata is the
 * evidence boundary for the pipeline and provider stages that actually ran.
 */
export function RetrievalEvalWorkbench({ datasetId }: { datasetId: string }) {
  const { t } = useTranslation();

  const [presets, setPresets] = useState<RetrievalPreset[]>([]);
  const [presetStatus, setPresetStatus] = useState<PresetStatus>("loading");
  const [presetError, setPresetError] = useState<string | null>(null);
  const [presetReloadKey, setPresetReloadKey] = useState(0);
  const [presetA, setPresetA] = useState("");
  const [presetB, setPresetB] = useState("");

  const [cases, setCases] = useState<TestCase[]>([]);
  const [newQuery, setNewQuery] = useState("");
  const [casesStatus, setCasesStatus] = useState<"loading" | "ready" | "error">("loading");
  const [casesError, setCasesError] = useState<string | null>(null);
  const [casesReloadKey, setCasesReloadKey] = useState(0);
  const [savingCases, setSavingCases] = useState(false);
  const [removedExampleIds, setRemovedExampleIds] = useState<string[]>([]);
  const [jsonlImportOpen, setJsonlImportOpen] = useState(false);
  const [jsonlText, setJsonlText] = useState("");
  const [jsonlSkipDuplicates, setJsonlSkipDuplicates] = useState(true);
  const [jsonlImporting, setJsonlImporting] = useState(false);

  const [kForGate, setKForGate] = useState(10);
  const [thresholds, setThresholds] = useState<GateThresholds>({
    ndcg: 0.8,
    recall: 0.8,
    mrr: 0.5,
  });

  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<EvaluationResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  const evaluationControllerRef = useRef<AbortController | null>(null);
  const candidateControllersRef = useRef(new Map<string, AbortController>());

  useEffect(() => {
    const controller = new AbortController();
    setPresetStatus("loading");
    setPresetError(null);
    setPresets([]);
    setPresetA("");
    setPresetB("");

    listRetrievalPresets({ signal: controller.signal, timeoutMs: PRESET_TIMEOUT_MS })
      .then((response) => {
        if (response.presets.length === 0) {
          throw new Error(t("knowledge.eval.noPresets", "服务未返回可用检索预设"));
        }
        for (const preset of response.presets) {
          validateRetrievalPresetConfig(preset.config);
        }

        const names = new Set(response.presets.map((preset) => preset.name));
        const defaultA = names.has(response.recommended_default)
          ? response.recommended_default
          : response.presets[0].name;
        const defaultB = names.has("sota") && defaultA !== "sota"
          ? "sota"
          : (response.presets.find((preset) => preset.name !== defaultA)?.name ?? defaultA);

        setPresets(response.presets);
        setPresetA(defaultA);
        setPresetB(defaultB);
        setPresetStatus("ready");
      })
      .catch((loadError: unknown) => {
        if (isCanceledError(loadError)) return;
        setPresetStatus("error");
        setPresetError(errorMessage(loadError));
      });

    return () => controller.abort();
  }, [datasetId, presetReloadKey, t]);

  useEffect(() => {
    const candidateControllers = candidateControllersRef.current;
    return () => {
      evaluationControllerRef.current?.abort();
      for (const controller of candidateControllers.values()) {
        controller.abort();
      }
      candidateControllers.clear();
    };
  }, []);

  useEffect(() => {
    evaluationControllerRef.current?.abort();
    evaluationControllerRef.current = null;
    for (const controller of candidateControllersRef.current.values()) {
      controller.abort();
    }
    candidateControllersRef.current.clear();
    setCases([]);
    setRemovedExampleIds([]);
    setNewQuery("");
    setResult(null);
    setError(null);
    setRunning(false);
  }, [datasetId]);

  // Persisted cases (PRD §5-#22): load from the linked eval dataset. Reading
  // never creates it — the dataset comes into being lazily on first save or
  // import, so a fresh workbench simply starts empty.
  useEffect(() => {
    let cancelled = false;
    setCasesStatus("loading");
    setCasesError(null);
    (async () => {
      try {
        const evalDataset = await findKbEvalDatasetPaged(datasetId, (offset) =>
          listEvalDatasets({ limit: KB_EVAL_DATASET_LIST_LIMIT, offset })
        );
        if (cancelled) return;
        if (!evalDataset) {
          setCases([]);
          setRemovedExampleIds([]);
          setCasesStatus("ready");
          return;
        }
        const examples = await listEvalExamples(evalDataset.dataset_id, {
          limit: KB_EVAL_EXAMPLE_LIST_LIMIT,
        });
        if (cancelled) return;
        const loaded = examples.examples
          .map(exampleToEvalCase)
          .filter((persisted): persisted is PersistedEvalCase => persisted !== null)
          .map((persisted) => ({
            id: persisted.caseId,
            exampleId: persisted.exampleId,
            query: persisted.query,
            relevantSegmentIds: persisted.relevantSegmentIds,
            relevantSegmentInput: persisted.relevantSegmentIds.join(", "),
          }));
        setCases(loaded);
        setRemovedExampleIds([]);
        setCasesStatus("ready");
      } catch (loadError: unknown) {
        if (cancelled) return;
        setCasesStatus("error");
        setCasesError(errorMessage(loadError));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [datasetId, casesReloadKey]);

  const jsonlPreview = useMemo(() => {
    const text = jsonlText.trim();
    if (!text) return null;
    try {
      const parsed = parseGoldenJsonl(text);
      return {
        ok: true as const,
        cases: parsed.cases,
        validation: validateGoldenCases(parsed.cases),
      };
    } catch (parseError: unknown) {
      return { ok: false as const, error: errorMessage(parseError) };
    }
  }, [jsonlText]);

  const presetByName = useMemo(
    () => new Map(presets.map((preset) => [preset.name, preset])),
    [presets]
  );
  const testSetSignature = useMemo(() => signatureForCases(cases), [cases]);
  const allCasesLabelled =
    cases.length > 0 && cases.every((testCase) => testCase.relevantSegmentIds.length > 0);
  const presetsReady =
    presetStatus === "ready" &&
    presetByName.has(presetA) &&
    presetByName.has(presetB) &&
    presetA !== presetB;
  const canRun = presetsReady && allCasesLabelled && !running;

  function addCase() {
    const query = newQuery.trim();
    if (!query || cases.length >= MAX_EVAL_CASES) return;
    setCases((previous) => [
      ...previous,
      { id: createCaseId(), query, relevantSegmentIds: [], relevantSegmentInput: "" },
    ]);
    setNewQuery("");
  }

  function removeCase(id: string) {
    candidateControllersRef.current.get(id)?.abort();
    candidateControllersRef.current.delete(id);
    const persistedExampleId = cases.find((testCase) => testCase.id === id)?.exampleId;
    if (persistedExampleId) {
      setRemovedExampleIds((previous) =>
        previous.includes(persistedExampleId)
          ? previous
          : [...previous, persistedExampleId]
      );
    }
    setCases((previous) => previous.filter((testCase) => testCase.id !== id));
  }

  /**
   * Persist the workbench into the linked eval dataset. The store's
   * skip_duplicates import dedupes on metadata.case_id alone, so new cases go
   * through import while edited cases go through PATCH (`diffEvalCases`
   * computes the split). Persisted cases explicitly removed in this browser
   * are deleted last, after imports and updates succeed. Keeping deletion last
   * makes a retry deterministic if an earlier write fails.
   */
  async function saveCases() {
    if (
      savingCases ||
      casesStatus !== "ready" ||
      (cases.length === 0 && removedExampleIds.length === 0)
    ) {
      return;
    }
    setSavingCases(true);
    try {
      const evalDataset = await resolveKbEvalDataset(datasetId);
      const listed = await listEvalExamples(evalDataset.dataset_id, {
        limit: KB_EVAL_EXAMPLE_LIST_LIMIT,
      });
      const serverCases = listed.examples
        .map(exampleToEvalCase)
        .filter((persisted): persisted is PersistedEvalCase => persisted !== null);
      const diff = diffEvalCases({
        localCases: cases.map((testCase) => ({
          id: testCase.id,
          query: testCase.query,
          relevantSegmentIds: testCase.relevantSegmentIds,
        })),
        serverCases,
        kbDatasetId: datasetId,
        removedExampleIds,
      });

      let imported = 0;
      if (diff.toImport.length > 0) {
        const response = await importEvalExamples(evalDataset.dataset_id, diff.toImport, {
          mode: "skip_duplicates",
        });
        imported = response.imported;
        const exampleByCaseId = new Map(
          response.examples.map((example) => [
            typeof example.metadata?.case_id === "string" ? example.metadata.case_id : "",
            example,
          ])
        );
        setCases((previous) =>
          previous.map((testCase) => {
            const example = exampleByCaseId.get(testCase.id);
            return example ? { ...testCase, exampleId: example.example_id } : testCase;
          })
        );
      }

      let updated = 0;
      for (const entry of diff.toUpdate) {
        // PATCH carries the full validate_case shape; metadata keeps case_id
        // so skip_duplicates still recognises the example on later imports.
        await updateEvalExample(evalDataset.dataset_id, entry.exampleId, {
          input: { query: entry.query },
          expected_output: { relevant_segment_ids: entry.relevantSegmentIds },
          expected_trajectory: {},
          assertions: [],
          metadata: {
            case_id: entry.caseId,
            source: KB_EVAL_DATASET_SOURCE,
            kb_dataset_id: datasetId,
          },
        });
        updated += 1;
      }

      let deleted = 0;
      for (const entry of diff.toDelete) {
        await deleteEvalExample(evalDataset.dataset_id, entry.exampleId);
        deleted += 1;
      }

      toast.success(
        t("knowledge.eval.casesSavedTitle", "评测用例已保存"),
        t("knowledge.eval.casesSavedText", {
          defaultValue:
            "新增 {{imported}} 个，更新 {{updated}} 个，删除 {{deleted}} 个，未变 {{unchanged}} 个",
          imported,
          updated,
          deleted,
          unchanged: diff.unchangedCount,
        })
      );
      setRemovedExampleIds([]);
      setCasesReloadKey((key) => key + 1);
    } catch (saveError: unknown) {
      toast.error(
        t("knowledge.eval.casesSaveFailedTitle", "评测用例保存失败"),
        errorMessage(saveError)
      );
    } finally {
      setSavingCases(false);
    }
  }

  function handleJsonlFile(event: React.ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) return;
    if (file.size > JSONL_MAX_FILE_BYTES) {
      toast.error(t("knowledge.eval.jsonlTooLarge", "JSONL 文件过大（上限 8 MB）"));
      return;
    }
    const reader = new FileReader();
    reader.onload = () => {
      if (typeof reader.result === "string") setJsonlText(reader.result);
    };
    reader.readAsText(file);
  }

  async function importJsonlCases() {
    if (
      jsonlImporting ||
      !jsonlPreview?.ok ||
      !jsonlPreview.validation.valid ||
      jsonlPreview.cases.length === 0
    ) {
      return;
    }
    setJsonlImporting(true);
    try {
      const evalDataset = await resolveKbEvalDataset(datasetId);
      const result = await importGoldenCasesInBatches(
        evalDataset.dataset_id,
        jsonlPreview.cases,
        { mode: jsonlSkipDuplicates ? "skip_duplicates" : "append" }
      );
      toast.success(
        t("knowledge.eval.jsonlImportedTitle", "JSONL 导入完成"),
        t("knowledge.eval.jsonlImportedText", {
          defaultValue: "导入 {{imported}} 个，跳过 {{skipped}} 个",
          imported: result.imported,
          skipped: result.skipped,
        })
      );
      setJsonlImportOpen(false);
      setJsonlText("");
      setCasesReloadKey((key) => key + 1);
    } catch (importError: unknown) {
      toast.error(
        t("knowledge.eval.jsonlImportFailedTitle", "JSONL 导入失败"),
        errorMessage(importError)
      );
    } finally {
      setJsonlImporting(false);
    }
  }

  const canSaveCases =
    casesStatus === "ready" &&
    (cases.length > 0 || removedExampleIds.length > 0) &&
    !savingCases;
  const canImportJsonl =
    jsonlPreview?.ok === true &&
    jsonlPreview.validation.valid &&
    jsonlPreview.cases.length > 0 &&
    !jsonlImporting;

  async function loadCandidates(id: string) {
    const testCase = cases.find((candidate) => candidate.id === id);
    if (!testCase) return;

    candidateControllersRef.current.get(id)?.abort();
    const controller = new AbortController();
    candidateControllersRef.current.set(id, controller);
    setCases((previous) =>
      previous.map((candidate) =>
        candidate.id === id
          ? { ...candidate, loadingCandidates: true, candidateError: undefined }
          : candidate
      )
    );

    try {
      const response = await retrieve(
        datasetId,
        {
          query: testCase.query,
          top_k: 10,
          mode: "hybrid",
        },
        { signal: controller.signal, timeoutMs: CANDIDATE_TIMEOUT_MS }
      );
      const seenSegmentIds = new Set<string>();
      const candidates = (response.results ?? []).flatMap((item) => {
        if (seenSegmentIds.has(item.segment_id)) return [];
        seenSegmentIds.add(item.segment_id);
        return [{ segment_id: item.segment_id, score: item.score, text: item.text }];
      });
      setCases((previous) =>
        previous.map((candidate) =>
          candidate.id === id
            ? {
                ...candidate,
                candidates,
                loadingCandidates: false,
                candidateError:
                  candidates.length === 0
                    ? t("knowledge.eval.noCandidates", "没有找到可标注的分段")
                    : undefined,
              }
            : candidate
        )
      );
    } catch (candidateError: unknown) {
      if (isCanceledError(candidateError)) return;
      setCases((previous) =>
        previous.map((candidate) =>
          candidate.id === id
            ? {
                ...candidate,
                loadingCandidates: false,
                candidateError: errorMessage(candidateError),
              }
            : candidate
        )
      );
    } finally {
      if (candidateControllersRef.current.get(id) === controller) {
        candidateControllersRef.current.delete(id);
      }
    }
  }

  function toggleRelevant(caseId: string, segmentId: string) {
    setCases((previous) =>
      previous.map((testCase) => {
        if (testCase.id !== caseId) return testCase;
        const selected = testCase.relevantSegmentIds.includes(segmentId);
        const relevantSegmentIds = selected
          ? testCase.relevantSegmentIds.filter((id) => id !== segmentId)
          : [...testCase.relevantSegmentIds, segmentId];
        return {
          ...testCase,
          relevantSegmentIds,
          relevantSegmentInput: relevantSegmentIds.join(", "),
        };
      })
    );
  }

  function setRelevantSegmentInput(caseId: string, value: string) {
    const relevantSegmentIds = [...new Set(
      value
        .split(/[,，\n]/)
        .map((segmentId) => segmentId.trim())
        .filter(Boolean)
    )].slice(0, 500);
    setCases((previous) =>
      previous.map((testCase) =>
        testCase.id === caseId
          ? { ...testCase, relevantSegmentInput: value, relevantSegmentIds }
          : testCase
      )
    );
  }

  async function runEval() {
    const selectedA = presetByName.get(presetA);
    const selectedB = presetByName.get(presetB);
    if (!canRun || !selectedA || !selectedB) return;

    evaluationControllerRef.current?.abort();
    const controller = new AbortController();
    evaluationControllerRef.current = controller;
    setRunning(true);
    setError(null);
    setResult(null);

    const evalCases: RetrievalEvalCase[] = cases.map((testCase) => ({
      query: testCase.query,
      case_id: testCase.id,
      relevant_segment_ids: testCase.relevantSegmentIds,
    }));
    const base = {
      cases: evalCases,
      k_values: DEFAULT_K_VALUES,
      return_retrieved: true,
    };
    const requestOptions = {
      signal: controller.signal,
      timeoutMs: EVALUATION_TIMEOUT_MS,
    };

    try {
      const [a, b] = await Promise.all([
        retrieveEvaluate(
          datasetId,
          { ...selectedA.config, ...base },
          requestOptions
        ),
        retrieveEvaluate(
          datasetId,
          { ...selectedB.config, ...base },
          requestOptions
        ),
      ]);
      if (controller.signal.aborted) return;
      setResult({
        a,
        b,
        presetA: { name: selectedA.name, label: selectedA.label },
        presetB: { name: selectedB.name, label: selectedB.label },
        testSetSignature,
      });
    } catch (evaluationError: unknown) {
      if (evaluationControllerRef.current !== controller) return;
      controller.abort();
      setError(
        isCanceledError(evaluationError)
          ? t("knowledge.eval.cancelled", "评测已取消")
          : errorMessage(evaluationError)
      );
    } finally {
      if (evaluationControllerRef.current === controller) {
        evaluationControllerRef.current = null;
        setRunning(false);
      }
    }
  }

  function cancelEvaluation() {
    evaluationControllerRef.current?.abort();
  }

  function metricAtK(
    response: RetrievalEvalResponse | undefined,
    k: number
  ): RetrievalMetricsAtK | undefined {
    return response?.metrics?.[String(k)];
  }

  function gatePass(metrics: RetrievalMetricsAtK | undefined): boolean {
    if (!metrics) return false;
    return (
      metrics.ndcg_at_k >= thresholds.ndcg &&
      metrics.recall_at_k >= thresholds.recall &&
      metrics.mrr >= thresholds.mrr
    );
  }

  const aK = metricAtK(result?.a, kForGate);
  const bK = metricAtK(result?.b, kForGate);
  const availableK = result?.a.k_values ?? DEFAULT_K_VALUES;
  const resultIsStale =
    result !== null &&
    (result.testSetSignature !== testSetSignature ||
      result.presetA.name !== presetA ||
      result.presetB.name !== presetB);

  const metricRows: Array<{ key: keyof RetrievalMetricsAtK; label: string }> = [
    { key: "hit_rate", label: t("knowledge.eval.hitRate", "命中率 Hit Rate") },
    { key: "recall_at_k", label: t("knowledge.eval.recall", "召回率 Recall@K") },
    {
      key: "precision_at_k",
      label: t("knowledge.eval.precision", "精确率 Precision@K"),
    },
    { key: "mrr", label: t("knowledge.eval.mrr", "MRR") },
    { key: "ndcg_at_k", label: t("knowledge.eval.ndcg", "nDCG@K") },
    { key: "map", label: t("knowledge.eval.map", "MAP") },
  ];

  return (
    <div className="space-y-5 sm:space-y-6" data-testid="retrieval-eval-workbench">
      <div className="flex flex-col gap-1 sm:flex-row sm:flex-wrap sm:items-center sm:gap-2">
        <div className="flex items-center gap-2">
          <FlaskConical className="h-5 w-5 text-primary" aria-hidden="true" />
          <h3 className="font-semibold text-foreground">
            {t("knowledge.eval.title", "检索评测工作台")}
          </h3>
        </div>
        <p className="text-xs text-muted-foreground">
          {t(
            "knowledge.eval.subtitle",
            "标注正确答案 → 选择两个请求预设 A/B → 对比检索质量指标与门禁"
          )}
        </p>
      </div>

      <p
        className="rounded-md border border-border/60 bg-muted/30 px-3 py-2 text-xs text-muted-foreground"
        data-testid="eval-scope-note"
      >
        {t(
          "knowledge.eval.scopeNote",
          "范围：工作台会把 canonical preset 原样提交给评测 API；requested_config 表示后端归一化后的请求，case_metadata 才是 standard / hierarchical / multimodal 管线与重排、回退实际执行的证据。预设名称本身不代表这些阶段已执行。"
        )}
      </p>

      <Card className="bg-card p-4 sm:p-5">
        <div className="mb-3 flex flex-wrap items-center gap-2">
          <ListChecks className="h-4 w-4 text-muted-foreground" aria-hidden="true" />
          <h4 className="font-medium text-foreground">
            {t("knowledge.eval.testSet", "评测集（查询 + 正确分段标注）")}
          </h4>
          <Badge variant="secondary">{cases.length}</Badge>
          <div className="ml-auto flex flex-wrap items-center gap-2">
            <Button
              size="sm"
              variant="outline"
              onClick={() => setJsonlImportOpen(true)}
              disabled={casesStatus !== "ready"}
              data-testid="eval-jsonl-import-open"
            >
              <FileUp className="mr-1 h-3.5 w-3.5" aria-hidden="true" />
              {t("knowledge.eval.jsonlImport", "导入 JSONL")}
            </Button>
            <Button
              size="sm"
              onClick={saveCases}
              disabled={!canSaveCases}
              data-testid="save-eval-cases"
            >
              {savingCases ? (
                <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" aria-hidden="true" />
              ) : (
                <Save className="mr-1 h-3.5 w-3.5" aria-hidden="true" />
              )}
              {savingCases
                ? t("knowledge.eval.savingCases", "保存中…")
                : t("knowledge.eval.saveCases", "保存用例")}
            </Button>
          </div>
        </div>

        {casesStatus === "loading" && (
          <p className="mb-3 flex items-center gap-2 text-xs text-muted-foreground" role="status">
            <Loader2 className="h-3.5 w-3.5 animate-spin" aria-hidden="true" />
            {t("knowledge.eval.casesLoading", "正在载入已保存的评测用例…")}
          </p>
        )}
        {casesStatus === "error" && (
          <div
            className="mb-3 flex flex-col gap-2 rounded-md border border-destructive/30 bg-destructive/5 p-2 sm:flex-row sm:items-center sm:justify-between"
            role="alert"
          >
            <p className="text-xs text-destructive">
              {t("knowledge.eval.casesLoadFailed", "评测用例载入失败")}
              {casesError ? `：${casesError}` : ""}
            </p>
            <Button
              size="sm"
              variant="outline"
              onClick={() => setCasesReloadKey((key) => key + 1)}
            >
              {t("knowledge.eval.retry", "重试")}
            </Button>
          </div>
        )}

        <div className="mb-4 flex flex-col gap-2 sm:flex-row">
          <div className="flex-1">
            <Label htmlFor="retrieval-eval-query" className="sr-only">
              {t("knowledge.eval.queryLabel", "测试问题")}
            </Label>
            <Input
              id="retrieval-eval-query"
              value={newQuery}
              onChange={(event) => setNewQuery(event.target.value)}
              onKeyDown={(event) => event.key === "Enter" && addCase()}
              placeholder={t(
                "knowledge.eval.queryPlaceholder",
                "输入一个测试问题，例如：报销流程需要什么材料？"
              )}
            />
          </div>
          <Button
            onClick={addCase}
            disabled={!newQuery.trim() || cases.length >= MAX_EVAL_CASES}
            className="w-full sm:w-auto"
          >
            <Plus className="mr-1 h-4 w-4" aria-hidden="true" />
            {t("knowledge.eval.add", "添加")}
          </Button>
        </div>

        <div className="space-y-3">
          {cases.length === 0 && (
            <p className="text-sm text-muted-foreground">
              {t(
                "knowledge.eval.empty",
                "暂无测试用例。添加问题后，点击“标注正确分段”选择应被召回的分段。"
              )}
            </p>
          )}
          {cases.map((testCase) => (
            <div key={testCase.id} className="rounded-lg border border-border/60 p-3">
              <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
                <div className="break-words text-sm font-medium text-foreground">
                  {testCase.query}
                </div>
                <div className="flex flex-wrap items-center gap-2 sm:shrink-0">
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => loadCandidates(testCase.id)}
                    disabled={testCase.loadingCandidates}
                  >
                    {testCase.loadingCandidates ? (
                      <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" aria-hidden="true" />
                    ) : (
                      <Search className="mr-1 h-3.5 w-3.5" aria-hidden="true" />
                    )}
                    {t("knowledge.eval.annotate", "标注正确分段")}
                  </Button>
                  <Button
                    size="icon"
                    variant="ghost"
                    onClick={() => removeCase(testCase.id)}
                    aria-label={t("knowledge.eval.removeCase", "删除测试用例")}
                  >
                    <Trash2 className="h-3.5 w-3.5 text-destructive" aria-hidden="true" />
                  </Button>
                </div>
              </div>

              <div className="mt-3 space-y-1.5">
                <Label htmlFor={`retrieval-relevant-ids-${testCase.id}`} className="text-xs">
                  {t("knowledge.eval.relevantSegmentIds", "正确分段 ID（逗号分隔）")}
                </Label>
                <Input
                  id={`retrieval-relevant-ids-${testCase.id}`}
                  value={testCase.relevantSegmentInput}
                  onChange={(event) => setRelevantSegmentInput(testCase.id, event.target.value)}
                  placeholder={t(
                    "knowledge.eval.relevantSegmentIdsPlaceholder",
                    "可直接输入未出现在候选列表中的分段 ID"
                  )}
                />
              </div>

              {testCase.relevantSegmentIds.length > 0 && (
                <div className="mt-1 text-xs text-muted-foreground">
                  {t("knowledge.eval.marked", "已标注正确分段")}: {testCase.relevantSegmentIds.length}
                </div>
              )}

              {testCase.candidateError && (
                <p className="mt-2 text-xs text-destructive" role="alert">
                  {testCase.candidateError}
                </p>
              )}

              {testCase.candidates && testCase.candidates.length > 0 && (
                <div className="mt-2 max-h-56 space-y-1 overflow-y-auto">
                  {testCase.candidates.map((candidate) => {
                    const checked = testCase.relevantSegmentIds.includes(candidate.segment_id);
                    const checkboxId = `retrieval-relevant-${testCase.id}-${candidate.segment_id}`;
                    return (
                      <label
                        key={candidate.segment_id}
                        htmlFor={checkboxId}
                        className="flex cursor-pointer items-start gap-2 rounded p-1.5 hover:bg-accent/50"
                      >
                        <Checkbox
                          id={checkboxId}
                          checked={checked}
                          onCheckedChange={() =>
                            toggleRelevant(testCase.id, candidate.segment_id)
                          }
                          aria-label={t("knowledge.eval.markRelevant", {
                            defaultValue: "标记为正确分段：{{segmentId}}",
                            segmentId: candidate.segment_id,
                          })}
                          className="mt-0.5"
                        />
                        <span className="w-14 shrink-0 text-xs text-muted-foreground">
                          {Number.isFinite(candidate.score) ? candidate.score.toFixed(3) : "—"}
                        </span>
                        <span className="line-clamp-2 min-w-0 text-xs text-foreground/80">
                          {candidate.text}
                        </span>
                      </label>
                    );
                  })}
                </div>
              )}
            </div>
          ))}
        </div>
      </Card>

      <Card className="bg-card p-4 sm:p-5">
        {presetStatus === "loading" && (
          <p className="mb-4 flex items-center gap-2 text-sm text-muted-foreground" role="status">
            <Loader2 className="h-4 w-4 animate-spin" aria-hidden="true" />
            {t("knowledge.eval.loadingPresets", "正在加载检索预设…")}
          </p>
        )}
        {presetStatus === "error" && (
          <div
            className="mb-4 flex flex-col gap-3 rounded-md border border-destructive/30 bg-destructive/5 p-3 sm:flex-row sm:items-center sm:justify-between"
            role="alert"
          >
            <p className="text-sm text-destructive">
              {t("knowledge.eval.presetsFailed", "检索预设加载失败，评测已禁用")}
              {presetError ? `：${presetError}` : ""}
            </p>
            <Button
              size="sm"
              variant="outline"
              onClick={() => setPresetReloadKey((value) => value + 1)}
            >
              {t("knowledge.eval.retry", "重试")}
            </Button>
          </div>
        )}

        <div className="mb-4 grid grid-cols-1 gap-4 md:grid-cols-2">
          <div>
            <Label className="text-sm text-muted-foreground">
              {t("knowledge.eval.presetA", "请求预设 A（基线）")}
            </Label>
            <Select
              value={presetA}
              onValueChange={setPresetA}
              disabled={presetStatus !== "ready"}
            >
              <SelectTrigger
                className="mt-2 bg-card"
                aria-label={t("knowledge.eval.presetA", "请求预设 A（基线）")}
                data-testid="eval-preset-a"
              >
                <SelectValue placeholder={t("knowledge.eval.selectPreset", "选择预设")} />
              </SelectTrigger>
              <SelectContent>
                {presets.map((preset) => (
                  <SelectItem key={preset.name} value={preset.name}>
                    {preset.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <p className="mt-1 min-h-4 text-xs text-muted-foreground">
              {presetByName.get(presetA)?.summary}
            </p>
          </div>
          <div>
            <Label className="text-sm text-muted-foreground">
              {t("knowledge.eval.presetB", "请求预设 B（候选）")}
            </Label>
            <Select
              value={presetB}
              onValueChange={setPresetB}
              disabled={presetStatus !== "ready"}
            >
              <SelectTrigger
                className="mt-2 bg-card"
                aria-label={t("knowledge.eval.presetB", "请求预设 B（候选）")}
                data-testid="eval-preset-b"
              >
                <SelectValue placeholder={t("knowledge.eval.selectPreset", "选择预设")} />
              </SelectTrigger>
              <SelectContent>
                {presets.map((preset) => (
                  <SelectItem key={preset.name} value={preset.name}>
                    {preset.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <p className="mt-1 min-h-4 text-xs text-muted-foreground">
              {presetByName.get(presetB)?.summary}
            </p>
          </div>
        </div>

        <div className="flex flex-col gap-4 border-t border-border/60 pt-3 sm:flex-row sm:flex-wrap sm:items-end">
          <div>
            <Label className="text-xs text-muted-foreground">
              {t("knowledge.eval.gateK", "门禁 K 值")}
            </Label>
            <Select value={String(kForGate)} onValueChange={(value) => setKForGate(Number(value))}>
              <SelectTrigger className="mt-1 w-full bg-card sm:w-24" aria-label="K">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {availableK.map((k) => (
                  <SelectItem key={k} value={String(k)}>
                    @{k}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          {(["ndcg", "recall", "mrr"] as const).map((key) => (
            <div key={key}>
              <Label htmlFor={`retrieval-gate-${key}`} className="text-xs text-muted-foreground">
                {key === "ndcg" ? "nDCG ≥" : key === "recall" ? "Recall ≥" : "MRR ≥"}
              </Label>
              <Input
                id={`retrieval-gate-${key}`}
                type="number"
                step={0.05}
                min={0}
                max={1}
                value={thresholds[key]}
                onChange={(event) => {
                  const value = Number(event.target.value);
                  setThresholds((previous) => ({
                    ...previous,
                    [key]: Number.isFinite(value) ? Math.min(1, Math.max(0, value)) : 0,
                  }));
                }}
                className="mt-1 h-9 w-full sm:w-24"
              />
            </div>
          ))}
          <div className="w-full sm:ml-auto sm:w-auto">
            {running ? (
              <Button variant="outline" onClick={cancelEvaluation} className="w-full sm:w-auto">
                <XCircle className="mr-2 h-4 w-4" aria-hidden="true" />
                {t("knowledge.eval.cancel", "取消评测")}
              </Button>
            ) : (
              <Button
                onClick={runEval}
                disabled={!canRun}
                className="w-full sm:w-auto"
                data-testid="run-retrieval-eval"
                aria-describedby={!canRun ? "retrieval-eval-disabled-reason" : undefined}
              >
                <FlaskConical className="mr-2 h-4 w-4" aria-hidden="true" />
                {t("knowledge.eval.run", "运行 A/B 评测")}
              </Button>
            )}
          </div>
        </div>
        {!canRun && !running && (
          <p id="retrieval-eval-disabled-reason" className="mt-2 text-xs text-muted-foreground">
            {!presetsReady
              ? presetStatus !== "ready"
                ? t("knowledge.eval.waitForPresets", "预设成功加载后才能运行评测。")
                : t("knowledge.eval.chooseDistinctPresets", "请选择两个不同的请求预设。")
              : t("knowledge.eval.labelEveryCase", "每个测试问题至少标注一个正确分段后才能运行。")}
          </p>
        )}
        {error && (
          <p className="mt-2 text-sm text-destructive" role="alert">
            {error}
          </p>
        )}
      </Card>

      {result && (
        <Card className="bg-card p-4 sm:p-5">
          {resultIsStale && (
            <p className="mb-4 rounded-md border border-amber-500/30 bg-amber-500/10 p-2 text-xs text-amber-700" role="status">
              {t(
                "knowledge.eval.staleTestSet",
                "测试集或请求预设已修改；以下结果仍对应上次运行，请重新评测。"
              )}
            </p>
          )}
          <div className="mb-4 flex flex-col gap-3 lg:flex-row lg:items-center lg:justify-between">
            <h4 className="font-medium text-foreground">
              {t("knowledge.eval.resultAt", "指标对比")} @{kForGate}
            </h4>
            <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center sm:gap-3">
              <span className="flex items-center gap-1 text-sm">
                A · {result.presetA.label}:
                {gatePass(aK) ? (
                  <Badge className="border-0 bg-green-100 text-green-800 dark:bg-green-950/60 dark:text-green-200">
                    <CheckCircle2 className="mr-1 h-3 w-3" aria-hidden="true" />
                    {t("knowledge.eval.pass", "通过")}
                  </Badge>
                ) : (
                  <Badge className="border-0 bg-red-100 text-red-800 dark:bg-red-950/60 dark:text-red-200">
                    <XCircle className="mr-1 h-3 w-3" aria-hidden="true" />
                    {t("knowledge.eval.fail", "未通过")}
                  </Badge>
                )}
              </span>
              <span className="flex items-center gap-1 text-sm">
                B · {result.presetB.label}:
                {gatePass(bK) ? (
                  <Badge className="border-0 bg-green-100 text-green-800 dark:bg-green-950/60 dark:text-green-200">
                    <CheckCircle2 className="mr-1 h-3 w-3" aria-hidden="true" />
                    {t("knowledge.eval.pass", "通过")}
                  </Badge>
                ) : (
                  <Badge className="border-0 bg-red-100 text-red-800 dark:bg-red-950/60 dark:text-red-200">
                    <XCircle className="mr-1 h-3 w-3" aria-hidden="true" />
                    {t("knowledge.eval.fail", "未通过")}
                  </Badge>
                )}
              </span>
            </div>
          </div>

          <div
            className="mb-4 grid gap-2 rounded-md border border-border/60 bg-muted/30 p-3 text-xs text-muted-foreground md:grid-cols-2"
            data-testid="eval-execution-evidence"
          >
            <p>A · {executionEvidence(result.a)}</p>
            <p>B · {executionEvidence(result.b)}</p>
          </div>

          <div className="overflow-x-auto">
            <Table className="min-w-[620px]">
              <TableHeader>
                <TableRow>
                  <TableHead>{t("knowledge.eval.metric", "指标")}</TableHead>
                  <TableHead className="text-right">A · {result.presetA.name}</TableHead>
                  <TableHead className="text-right">B · {result.presetB.name}</TableHead>
                  <TableHead className="text-right">Δ (B−A)</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {metricRows.map((row) => {
                  const av = aK?.[row.key] as number | undefined;
                  const bv = bK?.[row.key] as number | undefined;
                  return (
                    <TableRow key={String(row.key)}>
                      <TableCell className="font-medium">{row.label}</TableCell>
                      <TableCell className="text-right tabular-nums">{pct(av)}</TableCell>
                      <TableCell className="text-right tabular-nums">{pct(bv)}</TableCell>
                      <TableCell className="text-right tabular-nums">
                        {deltaPct(av, bv)}
                      </TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
          </div>
        </Card>
      )}

      <Dialog
        open={jsonlImportOpen}
        onOpenChange={(open) => {
          if (jsonlImporting) return;
          setJsonlImportOpen(open);
          if (!open) setJsonlText("");
        }}
      >
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle>
              {t("knowledge.eval.jsonlImportTitle", "导入评测用例（JSONL）")}
            </DialogTitle>
          </DialogHeader>
          <p className="text-xs text-muted-foreground">
            {t(
              "knowledge.eval.jsonlImportHint",
              "粘贴或选择 .jsonl 文件，每行一个用例（含 case_id、input、expected_output 等字段）。只有 input.query 非空的用例会出现在工作台列表中。"
            )}
          </p>
          <Textarea
            value={jsonlText}
            onChange={(event) => setJsonlText(event.target.value)}
            placeholder={t(
              "knowledge.eval.jsonlPlaceholder",
              '{"case_id": "case_1", "input": {"query": "报销流程需要什么材料？"}, "expected_output": {"relevant_segment_ids": ["seg-1"]}}'
            )}
            rows={6}
            className="font-mono text-xs"
            data-testid="eval-jsonl-textarea"
            aria-label={t("knowledge.eval.jsonlImportTitle", "导入评测用例（JSONL）")}
          />
          <div className="flex flex-wrap items-center gap-4">
            <label className="inline-flex h-9 cursor-pointer items-center justify-center gap-2 rounded-md border border-input bg-background px-3 text-sm font-medium shadow-xs hover:bg-accent hover:text-accent-foreground">
              <FileUp className="h-4 w-4" aria-hidden="true" />
              {t("knowledge.eval.jsonlChooseFile", "选择文件")}
              <input
                type="file"
                accept=".jsonl,.json,.txt"
                className="sr-only"
                onChange={handleJsonlFile}
                data-testid="eval-jsonl-file"
              />
            </label>
            <label className="flex items-center gap-2">
              <Switch
                checked={jsonlSkipDuplicates}
                onCheckedChange={setJsonlSkipDuplicates}
                data-testid="eval-jsonl-skip-duplicates"
              />
              <span className="text-sm text-foreground/80">
                {t("knowledge.eval.jsonlSkipDuplicates", "按 case_id 跳过重复")}
              </span>
            </label>
          </div>

          {jsonlPreview && !jsonlPreview.ok && (
            <p className="text-xs text-destructive" role="alert">
              {t("knowledge.eval.jsonlParseFailed", "JSONL 解析失败")}：{jsonlPreview.error}
            </p>
          )}
          {jsonlPreview?.ok && !jsonlPreview.validation.valid && (
            <div
              className="max-h-32 space-y-0.5 overflow-y-auto rounded-md border border-destructive/30 bg-destructive/5 p-2 text-xs text-destructive"
              role="alert"
            >
              <p>{t("knowledge.eval.jsonlInvalid", "以下用例校验未通过：")}</p>
              {jsonlPreview.validation.errors.slice(0, 6).map((entry) => (
                <p key={`${entry.case_id}-${entry.errors.join("|")}`}>
                  {entry.case_id}: {entry.errors.join(", ")}
                </p>
              ))}
            </div>
          )}
          {jsonlPreview?.ok && jsonlPreview.validation.valid && jsonlPreview.cases.length > 0 && (
            <p className="text-xs text-emerald-700 dark:text-emerald-400" role="status">
              {t("knowledge.eval.jsonlValid", "{{count}} 个用例校验通过", {
                count: jsonlPreview.cases.length,
              })}
            </p>
          )}
          {jsonlPreview?.ok && jsonlPreview.cases.length === 0 && (
            <p className="text-xs text-muted-foreground">
              {t("knowledge.eval.jsonlEmpty", "没有可导入的用例")}
            </p>
          )}

          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => {
                setJsonlImportOpen(false);
                setJsonlText("");
              }}
              disabled={jsonlImporting}
            >
              {t("common.cancel", "取消")}
            </Button>
            <Button onClick={importJsonlCases} disabled={!canImportJsonl} data-testid="eval-jsonl-submit">
              {jsonlImporting && <Loader2 className="mr-1 h-4 w-4 animate-spin" aria-hidden="true" />}
              {t("knowledge.eval.jsonlSubmit", "导入")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
