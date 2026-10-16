/**
 * Shared retrieval hit-test console state.
 *
 * Owns the hit-test workbench state that BOTH the Retrieval tab and the QA
 * tab read and mutate (topK / mode / fusion weights / rerank / mmr), plus the
 * retrieval-only pieces (query, presets, results, RAGAS scoring). The page
 * shell calls this hook once and hands the bundle to both tabs so edits in
 * one tab stay visible in the other — exactly as when all of this lived in
 * the page component.
 *
 * Extracted verbatim from DatasetDetail.tsx (C2 split); no behavior change.
 */

import { useEffect, useMemo, useRef, useState } from "react";

import {
  getDatasetConfig,
  hitTest,
  listRetrievalPresets,
  retrievalPresetToFlatRequest,
  type RetrievalPreset,
} from "@/api/knowledge";
import { scoreKbRagasRetrieval, type KbRagasScoreRetrievalResult } from "@/api/eval";
import type { RetrieveHit } from "@/types/knowledge";
import { DEFAULT_RETRIEVAL_CONFIG } from "@/types/knowledge";
import { getDefaultRerankModel, getRerankModelOptions } from "./rerankModelOptions";

export function useHitTestConsole(datasetId?: string, identityScope?: { isCurrent: () => boolean }) {
  const hitTestControllerRef = useRef<AbortController | null>(null);
  const ragasGenerationRef = useRef(0);
  const activeDatasetIdRef = useRef(datasetId);
  activeDatasetIdRef.current = datasetId;
  const activeIdentityRef = useRef(identityScope);
  activeIdentityRef.current = identityScope;
  const [viewScope, setViewScope] = useState({ datasetId, identityScope });
  const currentView = viewScope.datasetId === datasetId && viewScope.identityScope === identityScope
    && (identityScope?.isCurrent() ?? true);

  // Retrieval testing
  // Defaults read from the single source in @/types/knowledge. Two deliberate
  // workbench exceptions: scoreThreshold starts at 0 (no filtering, so every
  // hit is inspectable) and fusionMethod starts "weighted" so the weight
  // sliders are meaningful on first use.
  const [query, setQuery] = useState("");
  const [topK, setTopK] = useState(DEFAULT_RETRIEVAL_CONFIG.top_k);
  const [mode, setMode] = useState<"dense" | "bm25" | "hybrid">(DEFAULT_RETRIEVAL_CONFIG.mode);
  const [denseWeight, setDenseWeight] = useState(DEFAULT_RETRIEVAL_CONFIG.fusion.alpha);  // 0-1 weight for dense scores
  const [bm25Weight, setBm25Weight] = useState(1 - DEFAULT_RETRIEVAL_CONFIG.fusion.alpha);    // 0-1 weight for BM25 scores
  const [fusionMethod, setFusionMethod] = useState<"weighted" | "rrf">("weighted");
  const [scoreThreshold, setScoreThreshold] = useState(0);  // 0 means no filtering
  const [rerank, setRerank] = useState(false);
  const [hitRerankModel, setHitRerankModel] = useState("");
  const [rerankModelConfig, setRerankModelConfig] = useState<{
    datasetId: string | undefined;
    identityScope: typeof identityScope;
    status: "loading" | "ready" | "error";
    model: string;
    provider: string;
  }>({ datasetId: undefined, identityScope, status: "loading", model: "", provider: "" });
  const [hitRerankTopN, setHitRerankTopN] = useState<number | undefined>(undefined);
  const [mmr, setMmr] = useState(false);
  const [hitMmrLambda, setHitMmrLambda] = useState(DEFAULT_RETRIEVAL_CONFIG.mmr.lambda);
  const [hitLoading, setHitLoading] = useState(false);

  // Presets are opt-in so the existing manual retrieval defaults remain stable.
  const [retrievalPresets, setRetrievalPresets] = useState<RetrievalPreset[]>([]);
  const [retrievalPresetStatus, setRetrievalPresetStatus] = useState<"loading" | "ready" | "error">("loading");
  const [retrievalPresetError, setRetrievalPresetError] = useState<string | null>(null);
  const [retrievalPresetReloadKey, setRetrievalPresetReloadKey] = useState(0);
  const [selectedPreset, setSelectedPreset] = useState("");
  const [presetRequestConfig, setPresetRequestConfig] = useState<
    ReturnType<typeof retrievalPresetToFlatRequest>
  >({});
  const rerankConfigMatchesView = rerankModelConfig.datasetId === datasetId
    && rerankModelConfig.identityScope === identityScope;
  const rerankModelConfigStatus = rerankConfigMatchesView
    ? rerankModelConfig.status
    : "loading";
  const configuredRerankModel = rerankConfigMatchesView && rerankModelConfig.status === "ready"
    ? rerankModelConfig.model
    : "";
  const configuredRerankProvider = rerankConfigMatchesView && rerankModelConfig.status === "ready"
    ? rerankModelConfig.provider
    : "";
  const rerankModelOptions = useMemo(
    () => currentView
      ? getRerankModelOptions(configuredRerankModel, configuredRerankProvider, retrievalPresets)
      : [],
    [currentView, configuredRerankModel, configuredRerankProvider, retrievalPresets]
  );

  useEffect(() => {
    let cancelled = false;
    setRerankModelConfig({ datasetId, identityScope, status: "loading", model: "", provider: "" });
    setHitRerankModel("");
    if (!datasetId) {
      setRerankModelConfig({ datasetId, identityScope, status: "ready", model: "", provider: "" });
      return () => { cancelled = true; };
    }

    getDatasetConfig(datasetId)
      .then((config) => {
        if (cancelled || activeDatasetIdRef.current !== datasetId
          || activeIdentityRef.current !== identityScope || identityScope?.isCurrent() === false) return;
        if (config.dataset_id !== datasetId) throw new Error("Dataset config identity mismatch");
        const model = typeof config.retrieval?.rerank?.model === "string"
          ? config.retrieval.rerank.model.trim()
          : "";
        const provider = typeof config.retrieval?.rerank?.provider === "string"
          ? config.retrieval.rerank.provider.trim()
          : "";
        setRerankModelConfig({ datasetId, identityScope, status: "ready", model, provider });
      })
      .catch(() => {
        if (cancelled || activeDatasetIdRef.current !== datasetId
          || activeIdentityRef.current !== identityScope || identityScope?.isCurrent() === false) return;
        setRerankModelConfig({ datasetId, identityScope, status: "error", model: "", provider: "" });
      });
    return () => { cancelled = true; };
  }, [datasetId, identityScope]);

  useEffect(() => {
    if (!currentView || rerankModelConfigStatus === "loading") return;
    if (hitRerankModel && rerankModelOptions.includes(hitRerankModel)) return;
    const model = getDefaultRerankModel(configuredRerankModel, configuredRerankProvider, retrievalPresets);
    setHitRerankModel(model);
    if (rerank && !model) setRerank(false);
  }, [currentView, configuredRerankModel, configuredRerankProvider, hitRerankModel, rerank, rerankModelConfigStatus, rerankModelOptions, retrievalPresets]);

  useEffect(() => {
    const controller = new AbortController();
    setRetrievalPresetStatus("loading");
    setRetrievalPresetError(null);
    setRetrievalPresets([]);
    setSelectedPreset("");
    setPresetRequestConfig({});
    listRetrievalPresets({ signal: controller.signal, timeoutMs: 15_000 })
      .then((resp) => {
        if (resp.presets.length === 0) throw new Error("No retrieval presets available");
        for (const preset of resp.presets) {
          retrievalPresetToFlatRequest(preset.config);
        }
        setRetrievalPresets(resp.presets);
        setRetrievalPresetStatus("ready");
      })
      .catch((error: unknown) => {
        const candidate = error as { code?: string; name?: string };
        if (
          candidate?.code === "ERR_CANCELED" ||
          candidate?.name === "CanceledError" ||
          candidate?.name === "AbortError"
        ) {
          return;
        }
        setRetrievalPresetStatus("error");
        setRetrievalPresetError(error instanceof Error ? error.message : String(error));
      });
    return () => controller.abort();
  }, [retrievalPresetReloadKey]);

  const [hitResults, setHitResults] = useState<RetrieveHit[]>([]);
  const [ragasLoading, setRagasLoading] = useState(false);
  const [ragasResults, setRagasResults] = useState<KbRagasScoreRetrievalResult[]>([]);
  const [ragasJudgeModel, setRagasJudgeModel] = useState<string | null>(null);
  const [hitMeta, setHitMeta] = useState<Record<string, unknown>>({});
  const [hitTraceId, setHitTraceId] = useState("");
  const [hitQueryFingerprint, setHitQueryFingerprint] = useState("");
  const [hitExecutedQuery, setHitExecutedQuery] = useState("");

  function invalidateHitTestResults() {
    hitTestControllerRef.current?.abort();
    hitTestControllerRef.current = null;
    ragasGenerationRef.current += 1;
    setHitLoading(false);
    setRagasLoading(false);
    setHitResults([]);
    setHitMeta({});
    setHitTraceId("");
    setHitQueryFingerprint("");
    setHitExecutedQuery("");
    setRagasResults([]);
    setRagasJudgeModel(null);
  }

  const applyPreset = (presetName: string) => {
    const preset = retrievalPresets.find((p) => p.name === presetName);
    if (!preset) return;
    invalidateHitTestResults();
    const config = retrievalPresetToFlatRequest(preset.config);
    setSelectedPreset(presetName);
    setPresetRequestConfig(config);
    const modeMap: Record<string, "dense" | "bm25" | "hybrid"> = {
      vector: "dense",
      keyword: "bm25",
      hybrid: "hybrid",
    };
    if (config.mode) setMode(modeMap[config.mode] ?? "hybrid");
    if (typeof config.top_k === "number") setTopK(config.top_k);
    setScoreThreshold(config.score_threshold ?? 0);
    if (config.fusion_method) setFusionMethod(config.fusion_method);
    if (typeof config.dense_weight === "number") {
      setDenseWeight(config.dense_weight);
      setBm25Weight(config.bm25_weight ?? 1 - config.dense_weight);
    }
    const presetModel = typeof config.rerank_model === "string" ? config.rerank_model : "";
    const presetProvider = typeof preset.config.rerank.provider === "string"
      ? preset.config.rerank.provider
      : "";
    const presetProviderMatches = !presetProvider
      || (configuredRerankProvider.length > 0 && presetProvider === configuredRerankProvider);
    const presetModelIsAvailable = presetModel.length > 0
      && rerankModelOptions.includes(presetModel)
      && presetProviderMatches;
    setRerank(config.rerank === true && presetModelIsAvailable);
    setHitRerankModel(presetModelIsAvailable ? presetModel : configuredRerankModel);
    setHitRerankTopN(config.rerank_top_n);
    setMmr(config.mmr ?? false);
    setHitMmrLambda(config.mmr_lambda ?? DEFAULT_RETRIEVAL_CONFIG.mmr.lambda);
    if (config.rerank === true && !presetModelIsAvailable) {
      setHitMeta({ error: "rerank_preset_unavailable" });
    }
  };

  const markRetrievalConfigCustom = () => {
    invalidateHitTestResults();
    setSelectedPreset("");
    setPresetRequestConfig({});
  };

  useEffect(() => {
    setViewScope({ datasetId, identityScope });
    if (identityScope) setQuery("");
    hitTestControllerRef.current?.abort();
    hitTestControllerRef.current = null;
    ragasGenerationRef.current += 1;
    setHitLoading(false);
    setRagasLoading(false);
    setHitResults([]);
    setHitMeta({});
    setHitTraceId("");
    setHitQueryFingerprint("");
    setHitExecutedQuery("");
    setRagasResults([]);
    setRagasJudgeModel(null);
    setRerank(false);
    setHitRerankModel("");
    setHitRerankTopN(undefined);
    return () => hitTestControllerRef.current?.abort();
  }, [datasetId, identityScope]);

  async function runRagasScore() {
    if (activeDatasetIdRef.current !== datasetId || activeIdentityRef.current !== identityScope) return;
    if (!currentView || !datasetId || !query.trim() || hitResults.length === 0 || identityScope?.isCurrent() === false) return;
    const requestDatasetId = datasetId;
    const requestIdentity = identityScope;
    const requestQuery = query.trim();
    const generation = ragasGenerationRef.current + 1;
    ragasGenerationRef.current = generation;
    setRagasLoading(true);
    setRagasResults([]);
    setRagasJudgeModel(null);
    setHitMeta((previous) => {
      const next = { ...previous };
      delete next.ragas_error;
      return next;
    });
    try {
      const contexts = hitResults
        .map((hit) => hit.text?.trim())
        .filter((text): text is string => Boolean(text));
      const response = await scoreKbRagasRetrieval({
        query: requestQuery,
        contexts,
        dataset_id: requestDatasetId,
        metrics: ["context_relevancy"],
      });
      if (
        ragasGenerationRef.current !== generation ||
        activeDatasetIdRef.current !== requestDatasetId
        || activeIdentityRef.current !== requestIdentity
        || requestIdentity?.isCurrent() === false
      ) {
        return;
      }
      setRagasResults(response.results || []);
      setRagasJudgeModel(response.judge_model || null);
    } catch (err: unknown) {
      if (
        ragasGenerationRef.current !== generation ||
        activeDatasetIdRef.current !== requestDatasetId
        || activeIdentityRef.current !== requestIdentity
        || requestIdentity?.isCurrent() === false
      ) {
        return;
      }
      const message = err instanceof Error ? err.message : String(err);
      setHitMeta((prev) => ({ ...prev, ragas_error: message }));
    } finally {
      if (ragasGenerationRef.current === generation) {
        setRagasLoading(false);
      }
    }
  }

  async function runHitTest() {
    if (activeDatasetIdRef.current !== datasetId || activeIdentityRef.current !== identityScope) return;
    if (!currentView || !datasetId || !query.trim() || identityScope?.isCurrent() === false) return;
    if (rerank && (!hitRerankModel || !rerankModelOptions.includes(hitRerankModel))) {
      setHitMeta({ error: "rerank_model_unconfigured" });
      return;
    }
    const requestDatasetId = datasetId;
    const requestIdentity = identityScope;
    const requestQuery = query.trim();
    hitTestControllerRef.current?.abort();
    const controller = new AbortController();
    hitTestControllerRef.current = controller;
    ragasGenerationRef.current += 1;
    setHitLoading(true);
    setRagasLoading(false);
    setHitResults([]);
    setHitMeta({});
    setHitExecutedQuery("");
    setRagasResults([]);
    setRagasJudgeModel(null);
    try {
      const res = await hitTest(requestDatasetId, {
        ...presetRequestConfig,
        query: requestQuery,
        top_k: topK,
        mode,
        dense_weight: mode === "hybrid" ? denseWeight : undefined,
        bm25_weight: mode === "hybrid" ? bm25Weight : undefined,
        fusion_method: mode === "hybrid" ? fusionMethod : undefined,
        score_threshold: scoreThreshold > 0 ? scoreThreshold : undefined,
        rerank,
        rerank_model: rerank && hitRerankModel ? hitRerankModel : undefined,
        rerank_top_n: rerank ? hitRerankTopN : undefined,
        mmr,
        mmr_lambda: hitMmrLambda,
      }, {
        signal: controller.signal,
        timeoutMs: 60_000,
      });
      if (activeDatasetIdRef.current !== requestDatasetId || activeIdentityRef.current !== requestIdentity || requestIdentity?.isCurrent() === false || controller.signal.aborted) return;
      setHitResults(res.results || []);
      setHitMeta(res.metadata || {});
      setHitTraceId(res.trace_id || "");
      setHitQueryFingerprint(res.query_fingerprint || "");
      setHitExecutedQuery(requestQuery);
    } catch (err: unknown) {
      if (activeDatasetIdRef.current !== requestDatasetId || activeIdentityRef.current !== requestIdentity || requestIdentity?.isCurrent() === false || controller.signal.aborted) return;
      const candidate = err as { code?: string; name?: string };
      if (
        candidate?.code === "ERR_CANCELED" ||
        candidate?.name === "CanceledError" ||
        candidate?.name === "AbortError"
      ) {
        return;
      }
      const message = err instanceof Error ? err.message : String(err);
      setHitMeta({ error: message });
    } finally {
      if (hitTestControllerRef.current === controller) {
        hitTestControllerRef.current = null;
        setHitLoading(false);
      }
    }
  }

  return {
    query: currentView ? query : "",
    setQuery,
    topK,
    setTopK,
    mode,
    setMode,
    denseWeight,
    setDenseWeight,
    bm25Weight,
    setBm25Weight,
    fusionMethod,
    setFusionMethod,
    scoreThreshold,
    setScoreThreshold,
    rerank,
    setRerank,
    hitRerankModel,
    setHitRerankModel,
    rerankModelOptions: currentView ? rerankModelOptions : [],
    rerankModelConfigStatus,
    mmr,
    setMmr,
    hitLoading: currentView ? hitLoading : false,
    retrievalPresets,
    retrievalPresetStatus,
    retrievalPresetError,
    setRetrievalPresetReloadKey,
    selectedPreset,
    applyPreset,
    markRetrievalConfigCustom,
    hitResults: currentView ? hitResults : [],
    ragasLoading: currentView ? ragasLoading : false,
    ragasResults: currentView ? ragasResults : [],
    ragasJudgeModel: currentView ? ragasJudgeModel : null,
    hitMeta: currentView ? hitMeta : {},
    hitTraceId: currentView ? hitTraceId : "",
    hitQueryFingerprint: currentView ? hitQueryFingerprint : "",
    hitExecutedQuery: currentView ? hitExecutedQuery : "",
    invalidateHitTestResults,
    runHitTest,
    runRagasScore,
  };
}

export type HitTestConsole = ReturnType<typeof useHitTestConsole>;
