/**
 * Settings tab of the dataset detail page (chunking / retrieval / embedding
 * config, statistics, chunk preview, debug info, API examples).
 *
 * Extracted verbatim from DatasetDetail.tsx (C2 split); no behavior change.
 * Config loads lazily on first activation via the `active` prop — the same
 * gate the page used (`mainTab === "settings"`). The component stays mounted
 * once created (shell toggles `hidden`) so edit state survives tab switches.
 * `onDatasetRefetch` replaces the old `dsQuery.refetch()` call after an
 * embedding-model change.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Sliders,
  Edit3,
  Loader2,
  Search,
  Sparkles,
  BarChart3,
  Eye,
  Play,
  HelpCircle,
  Code,
  Terminal,
  Copy,
  Check,
  ExternalLink,
} from "lucide-react";

import { getApiBaseUrl } from "@/lib/api";
import { copyToClipboard } from "@/lib/clipboard";
import {
  getDatasetConfig,
  debugDataset,
  updateDatasetConfig,
  previewChunking,
  updateDataset,
  type ChunkPreviewItem,
} from "@/api/knowledge";
import type { ChunkingMode, Dataset, DatasetConfig, DatasetDebugInfo } from "@/types/knowledge";
import {
  CHUNKING_CONFIG_API_FIELDS,
  DEFAULT_CHUNKING_CONFIG,
  DEFAULT_RETRIEVAL_CONFIG,
  isSafeHeadingPatterns,
} from "@/types/knowledge";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { toast } from "@/hooks/use-toast";
import { DATASET_EMBEDDING_MODELS as EMBEDDING_MODELS } from "@/pages/knowledge/detail/useDatasetUploadController";
import { EmbeddingMigrationPanel } from "@/pages/knowledge/detail/EmbeddingMigrationPanel";
import { MetadataRegistryCard } from "@/pages/knowledge/detail/MetadataRegistryCard";
import { useAuthStore } from "@/store/useAuthStore";
import { currentSettingsDraftKey, shouldLoadSettingsConfig } from "./settingsDraftScope";
import {
  reconcileSettingsSave, restoreSettingsSave, settingsEditorUnchanged, stageSettingsSave,
  type SettingsSaveAttempt, type SettingsSaveSection,
} from "./settingsConfigSave";

interface SettingsDraft {
  version: 1;
  saveAttempt?: SettingsSaveAttempt | null;
  configEditing: boolean;
  retrievalEditing: boolean;
  embeddingEditing: boolean;
  chunkingMode: ChunkingMode;
  chunkSize: number;
  chunkOverlap: number;
  retrievalMode: "vector" | "keyword" | "hybrid";
  topK: number;
  fusionStrategy: "weighted" | "rrf";
  denseWeight: number;
  rerankEnabled: boolean;
  rerankModel: string;
  mmrEnabled: boolean;
  mmrLambda: number;
  scoreThreshold: number;
  embeddingModel: string;
}

function readSettingsDraft(key: string | null): SettingsDraft | null {
  if (!key) return null;
  try {
    const raw = sessionStorage.getItem(key);
    const draft: unknown = raw ? JSON.parse(raw) : null;
    if (!draft || typeof draft !== "object" || Array.isArray(draft)) return null;
    const value = draft as Partial<SettingsDraft>;
    return value.version === 1 && typeof value.configEditing === "boolean"
      && typeof value.retrievalEditing === "boolean" && typeof value.embeddingEditing === "boolean"
      && typeof value.chunkSize === "number" && typeof value.chunkOverlap === "number"
      && typeof value.topK === "number" && typeof value.denseWeight === "number"
      && typeof value.rerankEnabled === "boolean" && typeof value.mmrEnabled === "boolean"
      && typeof value.mmrLambda === "number" && typeof value.scoreThreshold === "number"
      && typeof value.rerankModel === "string" && typeof value.embeddingModel === "string"
      && typeof value.chunkingMode === "string" && typeof value.retrievalMode === "string"
      && typeof value.fusionStrategy === "string" ? value as SettingsDraft : null;
  } catch { return null; }
}

function writeSettingsDraft(key: string | null, draft: SettingsDraft | null) {
  if (!key) return;
  try {
    if (draft) sessionStorage.setItem(key, JSON.stringify(draft));
    else sessionStorage.removeItem(key);
  } catch { /* Keep the in-memory editor usable if tab storage is unavailable. */ }
}

interface SettingsTabProps {
  datasetId?: string;
  dataset?: Dataset;
  active: boolean;
  permission?: string;
  onUnsavedChange: (unsaved: boolean) => void;
  onDatasetRefetch: () => void;
}

export function SettingsTab({
  datasetId,
  dataset,
  active,
  permission,
  onUnsavedChange,
  onDatasetRefetch,
}: SettingsTabProps) {
  const { t } = useTranslation();
  const userId = useAuthStore((state) => state.user?.user_id);
  const sessionToken = useAuthStore((state) => state.token);
  // Session material stays in memory; persisted recovery is scoped by user/dataset.
  const scope = JSON.stringify([userId, sessionToken, datasetId]);
  const scopeRef = useRef({ value: scope, generation: 0 });
  if (scopeRef.current.value !== scope) {
    scopeRef.current = { value: scope, generation: scopeRef.current.generation + 1 };
  }
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const settingsDraftKey = userId && datasetId ? `kb-settings:${userId}:${datasetId}` : null;

  // Config
  const [datasetConfig, setDatasetConfig] = useState<DatasetConfig | null>(null);
  const [configLoading, setConfigLoading] = useState(false);
  const [debugInfo, setDebugInfo] = useState<DatasetDebugInfo | null>(null);
  const [draftReady, setDraftReady] = useState(false);
  const [draftRestored, setDraftRestored] = useState(false);
  const [loadedDatasetId, setLoadedDatasetId] = useState<string | null>(null);
  const [loadedScope, setLoadedScope] = useState<string | null>(null);
  const [loadedGeneration, setLoadedGeneration] = useState<number | null>(null);
  const scopeLoaded = loadedScope === scope && loadedGeneration === scopeRef.current.generation;
  const [saveAttempt, setSaveAttempt] = useState<SettingsSaveAttempt | null>(null);
  const saveAttemptRef = useRef<SettingsSaveAttempt | null>(null);
  const [saveReading, setSaveReading] = useState(false);
  const saveReadEpoch = useRef(0);
  const [reviewedConfig, setReviewedConfig] = useState<DatasetConfig | null>(null);
  const lastLoadAttemptId = useRef<string | null>(null);
  const wasActive = useRef(false);

  // Config editing - Chunking
  const [configEditing, setConfigEditing] = useState(false);
  const [configSaving, setConfigSaving] = useState(false);
  const [editChunkingMode, setEditChunkingMode] = useState(DEFAULT_CHUNKING_CONFIG.mode);
  const [editChunkSize, setEditChunkSize] = useState(DEFAULT_CHUNKING_CONFIG.chunk_size);
  const [editChunkOverlap, setEditChunkOverlap] = useState(DEFAULT_CHUNKING_CONFIG.chunk_overlap);

  // Config editing - Embedding
  const [embeddingEditing, setEmbeddingEditing] = useState(false);
  const [embeddingSaving, setEmbeddingSaving] = useState(false);
  const [editEmbeddingModel, setEditEmbeddingModel] = useState("");

  // Config editing - Retrieval
  const [retrievalEditing, setRetrievalEditing] = useState(false);
  const [editRetrievalMode, setEditRetrievalMode] = useState<"vector" | "keyword" | "hybrid">(DEFAULT_RETRIEVAL_CONFIG.mode);
  const [editTopK, setEditTopK] = useState(DEFAULT_RETRIEVAL_CONFIG.top_k);
  const [editFusionStrategy, setEditFusionStrategy] = useState<"weighted" | "rrf">(DEFAULT_RETRIEVAL_CONFIG.fusion.strategy);
  const [editDenseWeight, setEditDenseWeight] = useState(DEFAULT_RETRIEVAL_CONFIG.fusion.alpha);
  const [editBm25Weight, setEditBm25Weight] = useState(1 - DEFAULT_RETRIEVAL_CONFIG.fusion.alpha);
  const [editRerankEnabled, setEditRerankEnabled] = useState(DEFAULT_RETRIEVAL_CONFIG.rerank.enabled);
  const [editRerankModel, setEditRerankModel] = useState(DEFAULT_RETRIEVAL_CONFIG.rerank.model);
  const [editMmrEnabled, setEditMmrEnabled] = useState(DEFAULT_RETRIEVAL_CONFIG.mmr.enabled);
  const [editMmrLambda, setEditMmrLambda] = useState(DEFAULT_RETRIEVAL_CONFIG.mmr.lambda);
  const [editScoreThreshold, setEditScoreThreshold] = useState(DEFAULT_RETRIEVAL_CONFIG.score_threshold);

  // Chunk preview
  const [previewText, setPreviewText] = useState("");
  const [previewChunksResult, setPreviewChunksResult] = useState<ChunkPreviewItem[]>([]);
  const [previewLoading, setPreviewLoading] = useState(false);

  // API copy feedback
  const [copiedKey, setCopiedKey] = useState<string | null>(null);
  const unsavedConfig = scopeLoaded && loadedDatasetId === datasetId
    && (configEditing || retrievalEditing || embeddingEditing || saveAttempt !== null);
  const currentDraft: SettingsDraft = {
    version: 1, configEditing, retrievalEditing, embeddingEditing,
    chunkingMode: editChunkingMode, chunkSize: editChunkSize, chunkOverlap: editChunkOverlap,
    retrievalMode: editRetrievalMode, topK: editTopK, fusionStrategy: editFusionStrategy,
    denseWeight: editDenseWeight, rerankEnabled: editRerankEnabled, rerankModel: editRerankModel,
    mmrEnabled: editMmrEnabled, mmrLambda: editMmrLambda, scoreThreshold: editScoreThreshold,
    embeddingModel: editEmbeddingModel, saveAttempt,
  };
  const latestDraft = useRef(currentDraft);
  latestDraft.current = currentDraft;
  function editorSnapshot(section: SettingsSaveSection, draft = latestDraft.current) {
    if (section === "chunking") return [draft.chunkingMode, draft.chunkSize, draft.chunkOverlap];
    if (section === "embedding") return draft.embeddingModel;
    return [draft.retrievalMode, draft.topK, draft.fusionStrategy, draft.denseWeight,
      draft.rerankEnabled, draft.rerankModel, draft.mmrEnabled, draft.mmrLambda, draft.scoreThreshold];
  }

  useEffect(() => {
    const writableDraftKey = currentSettingsDraftKey(userId, datasetId,
      scopeLoaded ? loadedDatasetId : null, draftReady);
    if (!writableDraftKey) return;
    if (!unsavedConfig) setDraftRestored(false);
    writeSettingsDraft(writableDraftKey, unsavedConfig ? latestDraft.current : null);
  }, [draftReady, userId, datasetId, loadedDatasetId, loadedScope, scopeLoaded, scope, unsavedConfig,
    configEditing, retrievalEditing, embeddingEditing, editChunkingMode, editChunkSize,
    editChunkOverlap, editRetrievalMode, editTopK, editFusionStrategy, editDenseWeight,
    editRerankEnabled, editRerankModel, editMmrEnabled, editMmrLambda, editScoreThreshold,
    editEmbeddingModel, saveAttempt]);

  useEffect(() => {
    onUnsavedChange(unsavedConfig);
    return () => onUnsavedChange(false);
  }, [onUnsavedChange, unsavedConfig]);

  useEffect(() => {
    if (!unsavedConfig) return;
    const beforeUnload = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", beforeUnload);
    return () => window.removeEventListener("beforeunload", beforeUnload);
  }, [unsavedConfig]);

  // Helper function to copy text with feedback
  const handleCopy = async (text: string, key: string) => {
    try {
      await copyToClipboard(text);
      setCopiedKey(key);
      setTimeout(() => setCopiedKey(null), 2000);
    } catch (err) {
      console.error("Failed to copy:", err);
      toast.error(t("knowledge.detail.copyFailed"), t("knowledge.detail.copyManually"));
    }
  };

  const loadConfig = useCallback(async () => {
    if (!datasetId) return;
    if (scopeLoaded && saveAttemptRef.current) return;
    const generation = scopeRef.current.generation;
    const observationEpoch = saveReadEpoch.current;
    lastLoadAttemptId.current = `${generation}:${scope}`;
    if (!scopeLoaded) setConfigLoading(true);
    try {
      const [config, debug] = await Promise.all([
        getDatasetConfig(datasetId),
        debugDataset(datasetId).catch(() => null),
      ]);
      if (!mounted.current || scopeRef.current.generation !== generation || saveReadEpoch.current !== observationEpoch) return;
      if (config.dataset_id !== datasetId) throw new Error("Dataset config identity mismatch");
      // A migration refresh updates saved values, never the current editor or
      // an unresolved write. Only its explicit reconciliation may settle it.
      if (scopeLoaded && draftReady) {
        setDatasetConfig(config);
        setDebugInfo(debug);
        return;
      }
      // The route may reuse this component for another dataset. Reset the
      // editor before marking the new dataset ready to write its own draft.
      setDraftReady(false);
      setConfigEditing(false);
      setRetrievalEditing(false);
      setEmbeddingEditing(false);
      setDraftRestored(false);
      setConfigSaving(false);
      setEmbeddingSaving(false);
      setSaveReading(false);
      setReviewedConfig(null);
      saveAttemptRef.current = null;
      setSaveAttempt(null);
      setDatasetConfig(config);
      setDebugInfo(debug);

      // Initialize edit values from config
      if (config?.chunking) {
        setEditChunkingMode(config.chunking.mode || "automatic");
        setEditChunkSize(config.chunking.chunk_size || 500);
        setEditChunkOverlap(config.chunking.chunk_overlap ?? 50);
      }
      // Initialize retrieval config
      if (config?.retrieval) {
        // Map mode names (dense -> vector, bm25 -> keyword)
        const modeMap: Record<string, "vector" | "keyword" | "hybrid"> = {
          dense: "vector", vector: "vector",
          bm25: "keyword", keyword: "keyword",
          hybrid: "hybrid"
        };
        setEditRetrievalMode(modeMap[config.retrieval.mode] || DEFAULT_RETRIEVAL_CONFIG.mode);
        setEditTopK(config.retrieval.top_k || DEFAULT_RETRIEVAL_CONFIG.top_k);
        // Fusion config
        const fusion = config.retrieval.fusion;
        setEditFusionStrategy(fusion?.strategy === "weighted" ? "weighted" : DEFAULT_RETRIEVAL_CONFIG.fusion.strategy);
        // Alpha -> weights: alpha is dense weight, (1-alpha) is bm25 weight
        const alpha = fusion?.alpha ?? DEFAULT_RETRIEVAL_CONFIG.fusion.alpha;
        setEditDenseWeight(alpha);
        setEditBm25Weight(1 - alpha);
        // Rerank & MMR
        setEditRerankEnabled(config.retrieval.rerank?.enabled ?? DEFAULT_RETRIEVAL_CONFIG.rerank.enabled);
        setEditRerankModel(config.retrieval.rerank?.model || DEFAULT_RETRIEVAL_CONFIG.rerank.model);
        setEditMmrEnabled(config.retrieval.mmr?.enabled ?? DEFAULT_RETRIEVAL_CONFIG.mmr.enabled);
        setEditMmrLambda(config.retrieval.mmr?.lambda ?? DEFAULT_RETRIEVAL_CONFIG.mmr.lambda);
        // Score threshold
        setEditScoreThreshold(config.retrieval.score_threshold ?? DEFAULT_RETRIEVAL_CONFIG.score_threshold);
      }
      const draft = readSettingsDraft(settingsDraftKey);
      if (draft) {
        const recovery = restoreSettingsSave(draft.saveAttempt);
        saveAttemptRef.current = recovery;
        setSaveAttempt(recovery);
        setConfigEditing(draft.configEditing);
        setRetrievalEditing(draft.retrievalEditing);
        setEmbeddingEditing(draft.embeddingEditing);
        setEditChunkingMode(draft.chunkingMode);
        setEditChunkSize(draft.chunkSize);
        setEditChunkOverlap(draft.chunkOverlap);
        setEditRetrievalMode(draft.retrievalMode);
        setEditTopK(draft.topK);
        setEditFusionStrategy(draft.fusionStrategy);
        setEditDenseWeight(draft.denseWeight);
        setEditBm25Weight(1 - draft.denseWeight);
        setEditRerankEnabled(draft.rerankEnabled);
        setEditRerankModel(draft.rerankModel);
        setEditMmrEnabled(draft.mmrEnabled);
        setEditMmrLambda(draft.mmrLambda);
        setEditScoreThreshold(draft.scoreThreshold);
        setEditEmbeddingModel(draft.embeddingModel);
        setDraftRestored(true);
      }
      setLoadedDatasetId(datasetId);
      setLoadedScope(scope);
      setLoadedGeneration(generation);
      setDraftReady(true);
    } catch (e) {
      if (mounted.current && scopeRef.current.generation === generation) console.error("Failed to load config:", e);
    } finally {
      if (mounted.current && scopeRef.current.generation === generation) setConfigLoading(false);
    }
  }, [datasetId, settingsDraftKey, scope, scopeLoaded, draftReady]);

  const handleMigrationChange = useCallback(() => {
    onDatasetRefetch();
    void loadConfig();
  }, [loadConfig, onDatasetRefetch]);

  useEffect(() => {
    if (shouldLoadSettingsConfig(datasetId, scopeLoaded ? loadedDatasetId : null,
      lastLoadAttemptId.current === `${scopeRef.current.generation}:${scope}` ? datasetId ?? null : null, active, wasActive.current)) {
      void loadConfig();
    }
    wasActive.current = active;
  }, [datasetId, loadedDatasetId, scopeLoaded, scope, loadConfig, active]);

  /**
   * Chunking is REPLACED wholesale server-side, so build a complete object:
   * round-trip the stored config through the backend schema's field set
   * (ChunkingConfigSchema is extra="forbid" — anything else 422s), then
   * overlay the fields being edited. regex_pattern never round-trips (regex
   * chunking is disabled server-side), and heading_patterns survives only as
   * the exact safe triple. D6 tracks the runtime-only fields the schema
   * cannot carry yet.
   */
  function buildChunkingSavePayload(): Record<string, unknown> {
    const stored = (datasetConfig?.chunking ?? {}) as Record<string, unknown>;
    const payload: Record<string, unknown> = {};
    for (const key of CHUNKING_CONFIG_API_FIELDS) {
      const value = stored[key];
      if (value === undefined) continue;
      if (key === "heading_patterns" && !isSafeHeadingPatterns(value)) continue;
      payload[key] = value;
    }
    payload.mode = editChunkingMode;
    payload.chunk_size = editChunkSize;
    payload.chunk_overlap = editChunkOverlap;
    return payload;
  }

  function recordAttempt(attempt: SettingsSaveAttempt | null) {
    saveAttemptRef.current = attempt;
    setSaveAttempt(attempt);
    // Persist before sending: refresh during the request must not enable replay.
    writeSettingsDraft(settingsDraftKey, { ...latestDraft.current, saveAttempt: attempt });
  }

  function finishEditing(section: SettingsSaveSection) {
    if (section === "chunking") setConfigEditing(false);
    else if (section === "retrieval") setRetrievalEditing(false);
    else setEmbeddingEditing(false);
  }

  async function readSavedConfig(attempt: SettingsSaveAttempt, generation: number) {
    if (!datasetId) return;
    const readEpoch = ++saveReadEpoch.current;
    setSaveReading(true);
    setReviewedConfig(null);
    try {
      const config = await getDatasetConfig(datasetId);
      if (!mounted.current || scopeRef.current.generation !== generation || saveReadEpoch.current !== readEpoch) return;
      const result = reconcileSettingsSave(attempt, config, datasetId);
      setDatasetConfig(config);
      onDatasetRefetch();
      if (result.phase === "reconciled") {
        if (attempt.section && settingsEditorUnchanged(attempt, editorSnapshot(attempt.section))) {
          finishEditing(attempt.section);
        }
        recordAttempt(null);
      } else {
        recordAttempt(result);
        setReviewedConfig(config);
      }
    } catch {
      if (mounted.current && scopeRef.current.generation === generation && saveReadEpoch.current === readEpoch) {
        recordAttempt({ ...attempt, phase: "readback_uncertain" });
      }
    } finally {
      if (mounted.current && scopeRef.current.generation === generation && saveReadEpoch.current === readEpoch) setSaveReading(false);
    }
  }

  async function saveSection(section: SettingsSaveSection, submitted: Record<string, unknown>, write: () => Promise<unknown>) {
    if (!datasetId || !scopeLoaded || !draftReady || saveAttemptRef.current) return;
    const generation = scopeRef.current.generation;
    saveReadEpoch.current++;
    let attempt = stageSettingsSave(section, submitted, editorSnapshot(section));
    recordAttempt(attempt);
    setReviewedConfig(null);
    if (section === "embedding") setEmbeddingSaving(true); else setConfigSaving(true);
    try {
      await write();
      attempt = { ...attempt, mutationAccepted: true, phase: "mutation_accepted" };
    } catch {
      // Even HTTP 400 can follow a committed write whose compensation failed.
      attempt = { ...attempt, phase: "readback_uncertain" };
    }
    if (!mounted.current || scopeRef.current.generation !== generation) return;
    recordAttempt(attempt);
    await readSavedConfig(attempt, generation);
    if (mounted.current && scopeRef.current.generation === generation) {
      setConfigSaving(false);
      setEmbeddingSaving(false);
    }
  }

  async function handleSaveConfig() {
    if (!datasetId) return;
    // Local validation rejects before staging or transmitting a write.
    if (editChunkingMode === "regex") {
      toast.error(t("knowledge.detail.chunkRegexDisabled"), t("knowledge.detail.chunkRegexDisabledHint"));
      return;
    }
    if (editChunkOverlap >= editChunkSize) {
      toast.error(t("knowledge.detail.chunkOverlapTooLarge"));
      return;
    }
    const payload = buildChunkingSavePayload();
    await saveSection("chunking", payload, () => updateDatasetConfig(datasetId, { chunking_config: payload }));
  }

  async function handleSaveRetrievalConfig() {
    if (!datasetId) return;
    // Preserve the existing recursive PATCH: unexposed nested keys are omitted.
    const payload = {
          mode: editRetrievalMode,
          top_k: editTopK,
          score_threshold: editScoreThreshold,
          fusion: {
            strategy: editFusionStrategy,
            alpha: editDenseWeight,
          },
          rerank: {
            enabled: editRerankEnabled,
            model: editRerankModel,
          },
          mmr: {
            enabled: editMmrEnabled,
            lambda: editMmrLambda,
          },
        };
    await saveSection("retrieval", payload, () => updateDatasetConfig(datasetId, { retrieval_config: payload }));
  }

  async function handleSaveEmbeddingConfig() {
    if (!datasetId || !editEmbeddingModel) return;
    if (datasetConfig?.statistics?.document_count !== 0) {
      toast.error(t("knowledge.detail.embeddingMigration.directEditBlocked"),
        t("knowledge.detail.embeddingMigration.directEditBlockedHint"));
      return;
    }
    const [provider, model] = editEmbeddingModel.split(":");
    const embModel = EMBEDDING_MODELS.find((m) => m.provider === provider && m.model === model);
    const dimension = embModel?.dimension || 1024;
    await saveSection("embedding", { provider, model, dimension }, () => updateDataset(datasetId, {
      embedding_provider: provider, embedding_model: model, embedding_dimension: dimension,
    }));
  }

  function useReviewedConfig() {
    if (!reviewedConfig || reviewedConfig.dataset_id !== datasetId || !saveAttempt || saveReading) return;
    // Explicitly discards only the affected draft (all sections if its persisted
    // recovery marker was malformed). Never replaces another section's edits.
    saveReadEpoch.current++;
    const section = saveAttempt.section;
    if (section === "chunking" || section === null) {
      setEditChunkingMode(reviewedConfig.chunking.mode || "automatic");
      setEditChunkSize(reviewedConfig.chunking.chunk_size || 500);
      setEditChunkOverlap(reviewedConfig.chunking.chunk_overlap ?? 50);
      setConfigEditing(false);
    }
    if (section === "retrieval" || section === null) {
      const r = reviewedConfig.retrieval;
      const modes: Record<string, "vector" | "keyword" | "hybrid"> = { dense: "vector", vector: "vector", bm25: "keyword", keyword: "keyword", hybrid: "hybrid" };
      setEditRetrievalMode(modes[r.mode] || DEFAULT_RETRIEVAL_CONFIG.mode);
      setEditTopK(r.top_k || DEFAULT_RETRIEVAL_CONFIG.top_k);
      setEditFusionStrategy(r.fusion?.strategy === "weighted" ? "weighted" : "rrf");
      const alpha = r.fusion?.alpha ?? DEFAULT_RETRIEVAL_CONFIG.fusion.alpha;
      setEditDenseWeight(alpha); setEditBm25Weight(1 - alpha);
      setEditRerankEnabled(r.rerank?.enabled ?? DEFAULT_RETRIEVAL_CONFIG.rerank.enabled);
      setEditRerankModel(r.rerank?.model || DEFAULT_RETRIEVAL_CONFIG.rerank.model);
      setEditMmrEnabled(r.mmr?.enabled ?? DEFAULT_RETRIEVAL_CONFIG.mmr.enabled);
      setEditMmrLambda(r.mmr?.lambda ?? DEFAULT_RETRIEVAL_CONFIG.mmr.lambda);
      setEditScoreThreshold(r.score_threshold ?? DEFAULT_RETRIEVAL_CONFIG.score_threshold);
      setRetrievalEditing(false);
    }
    if (section === "embedding" || section === null) {
      setEditEmbeddingModel(`${reviewedConfig.embedding.provider}:${reviewedConfig.embedding.model}`);
      setEmbeddingEditing(false);
    }
    recordAttempt(null);
    setReviewedConfig(null);
  }

  async function handlePreviewChunks() {
    if (!datasetId || !previewText.trim()) return;
    setPreviewLoading(true);
    try {
      const result = await previewChunking(datasetId, previewText, {
        mode: editChunkingMode as "automatic",
        chunk_size: editChunkSize,
        chunk_overlap: editChunkOverlap,
      });
      setPreviewChunksResult(result.chunks);
    } catch (e) {
      console.error("Failed to preview chunks:", e);
      toast.error(t("knowledge.detail.previewChunkFailed"), e instanceof Error ? e.message : String(e));
    } finally {
      setPreviewLoading(false);
    }
  }

  const canEditEmbeddingDirectly = datasetConfig?.statistics?.document_count === 0;

  if (!scopeLoaded) {
    return <div className="p-5 text-muted-foreground" role="status">
      {configLoading ? <Loader2 className="h-5 w-5 animate-spin" /> : t("knowledge.detail.cannotLoadConfig")}
    </div>;
  }

  return (
    <div className="grid grid-cols-2 gap-6">
      {unsavedConfig && (
        <p role="status" className="col-span-2 text-sm text-amber-700 dark:text-amber-400">
          {t(draftRestored ? "knowledge.detail.configDraftRestored" : "knowledge.detail.unsavedConfigNotice")}
        </p>
      )}
      {scopeLoaded && saveAttempt && (
        <div className="col-span-2 rounded-lg border border-amber-500/30 bg-amber-500/10 p-4 space-y-2" role="status">
          <p>{t(saveAttempt.phase === "conflict" ? "knowledge.detail.configSaveConflict"
            : saveAttempt.phase === "observed_match" ? "knowledge.detail.configSaveObserved"
            : saveAttempt.phase === "staged" ? "knowledge.detail.configSavePending"
            : saveAttempt.phase === "mutation_accepted" ? "knowledge.detail.configSaveAccepted"
            : saveAttempt.mutationAccepted ? "knowledge.detail.configSaveAcceptedUnconfirmed"
            : "knowledge.detail.configSaveUnconfirmed")}</p>
          <Button variant="outline" disabled={configSaving || embeddingSaving || saveReading}
            onClick={() => void readSavedConfig(saveAttempt, scopeRef.current.generation)}>
            {t("knowledge.detail.configSaveCheck")}
          </Button>
          {reviewedConfig && (saveAttempt.phase === "conflict" || saveAttempt.phase === "observed_match") && (
            <Button variant="outline" disabled={saveReading} onClick={useReviewedConfig}>
              {t("knowledge.detail.configSaveUseCurrent")}
            </Button>
          )}
        </div>
      )}
      <div className="col-span-2">
        <MetadataRegistryCard
          datasetId={datasetId}
          active={active}
          permission={permission}
        />
      </div>
      {/* 分块配置 */}
      <Card className="p-5">
        <p className="mb-2 text-xs text-muted-foreground">
          {t("knowledge.detail.configSource")}: {t(dataset?.index_config?.chunking
            ? "knowledge.detail.configDatasetOverride" : "knowledge.detail.configDefaultInherited")}
          {" · "}{t("knowledge.detail.chunkingSavedNotIndexed")}
        </p>
        <div className="flex items-center justify-between mb-4">
          <h3 className="font-semibold text-foreground flex items-center gap-2">
            <Sliders className="h-5 w-5 text-amber-600" />
            {t("knowledge.detail.configChunking")}
          </h3>
          {!configEditing && (
            <Button
              variant="outline"
              size="sm"
              disabled={saveAttempt !== null || !scopeLoaded}
              onClick={() => setConfigEditing(true)}
            >
              <Edit3 className="h-3 w-3 mr-1" />
              {t("knowledge.detail.edit")}
            </Button>
          )}
        </div>

        {configLoading ? (
          <div className="py-12 text-center">
            <Loader2 className="h-8 w-8 animate-spin mx-auto text-muted-foreground/70" />
          </div>
        ) : configEditing ? (
          <div className="space-y-4">
            <div>
              <Label className="text-sm">{t("knowledge.detail.chunkMode")}</Label>
              <Select value={editChunkingMode} onValueChange={(value) => setEditChunkingMode(value as ChunkingMode)}>
                <SelectTrigger className="mt-1">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="automatic">
                    <div className="flex flex-col">
                      <span>{t("knowledge.detail.chunkAutomatic")}</span>
                      <span className="text-xs text-muted-foreground">{t("knowledge.detail.chunkAutomaticHint")}</span>
                    </div>
                  </SelectItem>
                  <SelectItem value="fixed_size">
                    <div className="flex flex-col">
                      <span>{t("knowledge.detail.chunkFixedSize")}</span>
                      <span className="text-xs text-muted-foreground">{t("knowledge.detail.chunkFixedSizeHint")}</span>
                    </div>
                  </SelectItem>
                  <SelectItem value="paragraph">
                    <div className="flex flex-col">
                      <span>{t("knowledge.detail.chunkParagraph")}</span>
                      <span className="text-xs text-muted-foreground">{t("knowledge.detail.chunkParagraphHint")}</span>
                    </div>
                  </SelectItem>
                  <SelectItem value="heading">
                    <div className="flex flex-col">
                      <span>{t("knowledge.detail.chunkHeading")}</span>
                      <span className="text-xs text-muted-foreground">{t("knowledge.detail.chunkHeadingHint")}</span>
                    </div>
                  </SelectItem>
                  <SelectItem value="hierarchical">
                    <div className="flex flex-col">
                      <span>{t("knowledge.detail.chunkHierarchical")}</span>
                      <span className="text-xs text-muted-foreground">{t("knowledge.detail.chunkHierarchicalHint")}</span>
                    </div>
                  </SelectItem>
                  <SelectItem value="recursive">
                    <div className="flex flex-col">
                      <span>{t("knowledge.detail.chunkRecursive")}</span>
                      <span className="text-xs text-muted-foreground">{t("knowledge.detail.chunkRecursiveHint")}</span>
                    </div>
                  </SelectItem>
                  <SelectItem value="separator">
                    <div className="flex flex-col">
                      <span>{t("knowledge.detail.chunkSeparator")}</span>
                      <span className="text-xs text-muted-foreground">{t("knowledge.detail.chunkSeparatorHint")}</span>
                    </div>
                  </SelectItem>
                </SelectContent>
              </Select>
              {/* 模式说明 */}
              <p className="text-xs text-muted-foreground mt-1">
                {t(`knowledge.detail.chunkModeDesc.${editChunkingMode}`)}
              </p>
            </div>

            {/* 基础参数 - 非automatic模式显示 */}
            {editChunkingMode !== "automatic" && (
              <>
                <div>
                  <div className="flex justify-between items-center">
                    <Label className="text-sm">
                      {editChunkingMode === "hierarchical" ? t("knowledge.detail.childBlockSize") : t("knowledge.detail.blockSize")}
                    </Label>
                    <span className="text-sm text-muted-foreground">{t("knowledge.detail.characters", { count: editChunkSize })}</span>
                  </div>
                  <input
                    type="range"
                    min={100}
                    max={2000}
                    step={50}
                    value={editChunkSize}
                    onChange={(e) => setEditChunkSize(Number(e.target.value))}
                    className="w-full mt-2"
                  />
                </div>
                <div>
                  <div className="flex justify-between items-center">
                    <Label className="text-sm">{t("knowledge.detail.overlapSize")}</Label>
                    <span className="text-sm text-muted-foreground">{t("knowledge.detail.characters", { count: editChunkOverlap })}</span>
                  </div>
                  <input
                    type="range"
                    min={0}
                    max={200}
                    step={10}
                    value={editChunkOverlap}
                    onChange={(e) => setEditChunkOverlap(Number(e.target.value))}
                    className="w-full mt-2"
                  />
                </div>
              </>
            )}

            {/* hierarchical模式特有参数 */}
            {editChunkingMode === "hierarchical" && (
              <div className="p-3 bg-accent/10 rounded-lg space-y-3">
                <Label className="text-sm font-medium text-accent">{t("knowledge.detail.parentChildConfig")}</Label>
                <div>
                  <div className="flex justify-between items-center">
                    <Label className="text-xs text-muted-foreground">{t("knowledge.detail.parentBlockSize")}</Label>
                    <span className="text-xs text-muted-foreground">{t("knowledge.detail.characters", { count: 2000 })}</span>
                  </div>
                  <p className="text-xs text-muted-foreground/70 mt-1">{t("knowledge.detail.parentBlockHint")}</p>
                </div>
              </div>
            )}
            <div className="flex gap-2 pt-2">
              <Button
                onClick={handleSaveConfig}
                disabled={configSaving || embeddingSaving || saveReading || saveAttempt !== null || !scopeLoaded}
                className="bg-primary hover:bg-primary/90"
              >
                {configSaving ? t("knowledge.detail.saving") : t("knowledge.detail.saveConfig")}
              </Button>
              <Button
                variant="outline"
                disabled={saveAttempt !== null || !scopeLoaded}
                onClick={() => {
                  setConfigEditing(false);
                  if (datasetConfig?.chunking) {
                    setEditChunkingMode(datasetConfig.chunking.mode || "automatic");
                    setEditChunkSize(datasetConfig.chunking.chunk_size || 500);
                    setEditChunkOverlap(datasetConfig.chunking.chunk_overlap ?? 50);
                  }
                }}
              >
                {t("knowledge.detail.cancel")}
              </Button>
            </div>
            <div className="p-3 bg-amber-500/10 dark:bg-amber-500/15 border border-amber-500/20 rounded-lg text-sm text-amber-700 dark:text-amber-400">
              {t("knowledge.detail.chunkConfigWarning")}
            </div>
          </div>
        ) : datasetConfig ? (
          <div className="space-y-4">
            {/* 分块模式标签 */}
            <div className="flex items-center gap-3 p-3.5 rounded-xl border border-amber-500/15 bg-linear-to-r from-amber-500/5 to-transparent">
              <div className="w-9 h-9 rounded-lg bg-amber-500/10 dark:bg-amber-500/15 flex items-center justify-center shrink-0">
                <Sliders className="h-4 w-4 text-amber-500" />
              </div>
              <div className="flex-1 min-w-0">
                <p className="text-xs text-muted-foreground">{t("knowledge.detail.currentMode")}</p>
                <p className="font-semibold text-foreground">
                  {t(`knowledge.detail.chunkModeLabels.${datasetConfig.chunking?.mode || "automatic"}`) || datasetConfig.chunking?.mode}
                </p>
              </div>
              <Badge variant="outline" className="text-xs border-amber-500/30 text-amber-600 dark:text-amber-400 bg-amber-500/5">
                {t(`knowledge.detail.chunkModeShort.${datasetConfig.chunking?.mode || "automatic"}`)}
              </Badge>
            </div>

            {/* 参数网格 */}
            <div className="grid grid-cols-2 gap-3">
              <div className="relative p-3.5 rounded-xl border border-border/50 bg-muted/30 group hover:border-primary/20 transition-colors">
                <div className="flex items-baseline justify-between">
                  <p className="text-xs text-muted-foreground">{t("knowledge.detail.blockSize")}</p>
                  <span className="text-[10px] text-muted-foreground/60">chars</span>
                </div>
                <p className="text-xl font-bold text-foreground mt-1 tabular-nums tracking-tight">
                  {datasetConfig.chunking?.chunk_size || 2000}
                </p>
                <div className="mt-2 h-1 rounded-full bg-muted overflow-hidden">
                  <div className="h-full rounded-full bg-primary/40" style={{ width: `${Math.min(((datasetConfig.chunking?.chunk_size || 2000) / 4000) * 100, 100)}%` }} />
                </div>
              </div>
              <div className="relative p-3.5 rounded-xl border border-border/50 bg-muted/30 group hover:border-primary/20 transition-colors">
                <div className="flex items-baseline justify-between">
                  <p className="text-xs text-muted-foreground">{t("knowledge.detail.overlapSize")}</p>
                  <span className="text-[10px] text-muted-foreground/60">chars</span>
                </div>
                <p className="text-xl font-bold text-foreground mt-1 tabular-nums tracking-tight">
                  {datasetConfig.chunking?.chunk_overlap ?? 50}
                </p>
                <div className="mt-2 h-1 rounded-full bg-muted overflow-hidden">
                  <div className="h-full rounded-full bg-amber-500/40" style={{ width: `${Math.min(((datasetConfig.chunking?.chunk_overlap ?? 50) / (datasetConfig.chunking?.chunk_size || 2000)) * 100, 100)}%` }} />
                </div>
              </div>
            </div>
          </div>
        ) : (
          <p className="text-muted-foreground">{t("knowledge.detail.cannotLoadConfig")}</p>
        )}
      </Card>

      {/* 检索配置 */}
      <Card className="p-5">
        <p className="mb-2 text-xs text-muted-foreground">
          {t("knowledge.detail.configSource")}: {t(dataset?.index_config?.retrieval
            ? "knowledge.detail.configDatasetOverride" : "knowledge.detail.configDefaultInherited")}
          {" · "}{t("knowledge.detail.retrievalQueryOnly")}
        </p>
        <div className="flex items-center justify-between mb-4">
          <h3 className="font-semibold text-foreground flex items-center gap-2">
            <Search className="h-5 w-5 text-emerald-600" />
            {t("knowledge.detail.configRetrieval")}
          </h3>
          {!retrievalEditing && (
            <Button
              variant="outline"
              size="sm"
              disabled={saveAttempt !== null || !scopeLoaded}
              onClick={() => setRetrievalEditing(true)}
            >
              <Edit3 className="h-3.5 w-3.5 mr-1" />
              {t("knowledge.detail.edit")}
            </Button>
          )}
        </div>

        {configLoading ? (
          <div className="py-12 text-center">
            <Loader2 className="h-8 w-8 animate-spin mx-auto text-muted-foreground/70" />
          </div>
        ) : retrievalEditing ? (
          <div className="space-y-4">
            {/* 检索模式 */}
            <div>
              <Label className="text-sm">{t("knowledge.detail.retrievalMode")}</Label>
              <Select value={editRetrievalMode} onValueChange={(v) => setEditRetrievalMode(v as typeof editRetrievalMode)}>
                <SelectTrigger className="mt-1">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="hybrid">{t("knowledge.detail.hybridRecommended")}</SelectItem>
                  <SelectItem value="vector">{t("knowledge.detail.vectorRetrieval")}</SelectItem>
                  <SelectItem value="keyword">{t("knowledge.detail.keywordRetrieval")}</SelectItem>
                </SelectContent>
              </Select>
            </div>

            {/* Top K */}
            <div>
              <div className="flex justify-between items-center">
                <Label className="text-sm">{t("knowledge.detail.topK")}</Label>
                <span className="text-sm text-muted-foreground">{editTopK}</span>
              </div>
              <input
                type="range"
                min={1}
                max={20}
                value={editTopK}
                onChange={(e) => setEditTopK(Number(e.target.value))}
                className="w-full mt-2"
              />
            </div>

            {/* Score Threshold */}
            <div>
              <div className="flex justify-between items-center">
                <Label className="text-sm">{t("knowledge.detail.scoreThreshold")}</Label>
                <span className="text-sm text-muted-foreground">{(editScoreThreshold * 100).toFixed(0)}%</span>
              </div>
              <input
                type="range"
                min={0}
                max={100}
                value={editScoreThreshold * 100}
                onChange={(e) => setEditScoreThreshold(Number(e.target.value) / 100)}
                className="w-full mt-2"
              />
              <p className="text-xs text-muted-foreground mt-1">
                {t("knowledge.detail.scoreThresholdHint")}
              </p>
            </div>

            {/* 融合配置 - 仅hybrid模式 */}
            {editRetrievalMode === "hybrid" && (
              <div className="p-3 bg-primary/5 rounded-lg space-y-3">
                <Label className="text-sm font-medium text-primary">{t("knowledge.detail.fusionStrategy")}</Label>
                <Select value={editFusionStrategy} onValueChange={(v) => setEditFusionStrategy(v as typeof editFusionStrategy)}>
                  <SelectTrigger className="bg-card">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="rrf">RRF (Reciprocal Rank Fusion)</SelectItem>
                    <SelectItem value="weighted">{t("knowledge.detail.weightedFusion")}</SelectItem>
                  </SelectContent>
                </Select>

                {/* 权重滑块 */}
                <div className="space-y-2">
                  <div className="flex justify-between text-xs text-muted-foreground">
                    <span>{t("knowledge.detail.vectorWeight", { pct: (editDenseWeight * 100).toFixed(0) })}</span>
                    <span>{t("knowledge.detail.bm25WeightPct", { pct: (editBm25Weight * 100).toFixed(0) })}</span>
                  </div>
                  <input
                    type="range"
                    min={0}
                    max={100}
                    value={editDenseWeight * 100}
                    onChange={(e) => {
                      const v = Number(e.target.value) / 100;
                      setEditDenseWeight(v);
                      setEditBm25Weight(1 - v);
                    }}
                    className="w-full"
                  />
                  <div className="flex justify-between text-xs">
                    <span className="text-primary">{t("knowledge.detail.vectorSemantic")}</span>
                    <span className="text-amber-600">{t("knowledge.detail.bm25Keyword")}</span>
                  </div>
                </div>
              </div>
            )}

            {/* Rerank */}
            <div className="flex items-center justify-between p-3 bg-muted/40 rounded-lg">
              <div>
                <p className="font-medium text-foreground">{t("knowledge.detail.rerankReorder")}</p>
                <p className="text-xs text-muted-foreground">{t("knowledge.detail.rerankHint")}</p>
              </div>
              <Switch
                checked={editRerankEnabled}
                onCheckedChange={setEditRerankEnabled}
              />
            </div>
            {editRerankEnabled && (
              <div className="ml-3">
                <Label htmlFor="knowledge-rerank-model" className="text-xs text-muted-foreground">{t("knowledge.detail.rerankModelLabel")}</Label>
                <Input
                  id="knowledge-rerank-model"
                  className="mt-1 h-8"
                  value={editRerankModel}
                  placeholder={t("knowledge.detail.rerankModelIdPlaceholder")}
                  onChange={(event) => setEditRerankModel(event.target.value)}
                />
                <p className="mt-1 text-xs text-muted-foreground">
                  {t("knowledge.detail.rerankModelIdHint")}
                </p>
              </div>
            )}

            {/* MMR */}
            <div className="flex items-center justify-between p-3 bg-muted/40 rounded-lg">
              <div>
                <p className="font-medium text-foreground">{t("knowledge.detail.mmrDiversity")}</p>
                <p className="text-xs text-muted-foreground">{t("knowledge.detail.mmrHint")}</p>
              </div>
              <Switch
                checked={editMmrEnabled}
                onCheckedChange={setEditMmrEnabled}
              />
            </div>
            {editMmrEnabled && (
              <div className="ml-3">
                <div className="flex justify-between items-center">
                  <Label className="text-xs text-muted-foreground">{t("knowledge.detail.mmrLambda")}</Label>
                  <span className="text-xs text-muted-foreground">{editMmrLambda.toFixed(2)}</span>
                </div>
                <input
                  type="range"
                  min={0}
                  max={100}
                  value={editMmrLambda * 100}
                  onChange={(e) => setEditMmrLambda(Number(e.target.value) / 100)}
                  className="w-full mt-1"
                />
                <div className="flex justify-between text-xs mt-1">
                  <span className="text-accent">{t("knowledge.detail.diversityFirst")}</span>
                  <span className="text-green-600">{t("knowledge.detail.relevanceFirst")}</span>
                </div>
              </div>
            )}

            {/* 保存/取消按钮 */}
            <div className="flex gap-2 pt-2">
              <Button
                onClick={handleSaveRetrievalConfig}
                disabled={configSaving || embeddingSaving || saveReading || saveAttempt !== null || !scopeLoaded}
                className="bg-emerald-600 hover:bg-emerald-700"
              >
                {configSaving ? t("knowledge.detail.saving") : t("knowledge.detail.saveConfig")}
              </Button>
              <Button
                variant="outline"
                disabled={saveAttempt !== null || !scopeLoaded}
                onClick={() => {
                  setRetrievalEditing(false);
                  // Reset values
                  if (datasetConfig?.retrieval) {
                    const modeMap: Record<string, "vector" | "keyword" | "hybrid"> = {
                      dense: "vector", vector: "vector", bm25: "keyword", keyword: "keyword", hybrid: "hybrid"
                    };
                    setEditRetrievalMode(modeMap[datasetConfig.retrieval.mode] || "hybrid");
                    setEditTopK(datasetConfig.retrieval.top_k || 5);
                  }
                }}
              >
                {t("knowledge.detail.cancel")}
              </Button>
            </div>
          </div>
        ) : datasetConfig ? (
          <div className="space-y-4">
            {/* 检索模式标签 */}
            <div className="flex items-center gap-3 p-3.5 rounded-xl border border-emerald-500/15 bg-linear-to-r from-emerald-500/5 to-transparent">
              <div className="w-9 h-9 rounded-lg bg-emerald-500/10 dark:bg-emerald-500/15 flex items-center justify-center shrink-0">
                <Search className="h-4 w-4 text-emerald-500" />
              </div>
              <div className="flex-1 min-w-0">
                <p className="text-xs text-muted-foreground">{t("knowledge.detail.retrievalMode")}</p>
                <p className="font-semibold text-foreground">
                  {t(`knowledge.detail.retrievalModes.${datasetConfig.retrieval?.mode || "hybrid"}`)}
                </p>
              </div>
              <Badge variant="outline" className="text-xs border-emerald-500/30 text-emerald-600 dark:text-emerald-400 bg-emerald-500/5">
                {{vector: "Dense", dense: "Dense", keyword: "BM25", bm25: "BM25", hybrid: "Hybrid"}[datasetConfig.retrieval?.mode || DEFAULT_RETRIEVAL_CONFIG.mode]}
              </Badge>
            </div>

            {/* 参数网格 */}
            <div className="grid grid-cols-2 gap-3">
              <div className="p-3.5 rounded-xl border border-border/50 bg-muted/30">
                <p className="text-xs text-muted-foreground">Top K</p>
                <p className="text-xl font-bold text-foreground mt-1 tabular-nums">{datasetConfig.retrieval?.top_k || DEFAULT_RETRIEVAL_CONFIG.top_k}</p>
              </div>
              <div className="p-3.5 rounded-xl border border-border/50 bg-muted/30">
                <p className="text-xs text-muted-foreground">{t("knowledge.detail.scoreThreshold")}</p>
                <p className="text-xl font-bold text-foreground mt-1 tabular-nums">{((datasetConfig.retrieval?.score_threshold ?? DEFAULT_RETRIEVAL_CONFIG.score_threshold) * 100).toFixed(0)}%</p>
              </div>
            </div>

            {/* 融合权重 - hybrid模式 */}
            {(datasetConfig.retrieval?.mode === "hybrid") && (
              <div className="p-3.5 rounded-xl border border-primary/15 bg-linear-to-r from-primary/5 to-transparent">
                <div className="flex items-center justify-between mb-2.5">
                  <p className="text-xs font-medium text-primary">{t("knowledge.detail.fusionWeight")}</p>
                  <div className="flex items-center gap-2 text-[10px] text-muted-foreground">
                    <span>{t("knowledge.detail.vectorWeight", { pct: ((datasetConfig.retrieval.fusion?.alpha ?? DEFAULT_RETRIEVAL_CONFIG.fusion.alpha) * 100).toFixed(0) })}</span>
                    <span className="text-muted-foreground/40">|</span>
                    <span>BM25 {((1 - (datasetConfig.retrieval.fusion?.alpha ?? DEFAULT_RETRIEVAL_CONFIG.fusion.alpha)) * 100).toFixed(0)}%</span>
                  </div>
                </div>
                <div className="h-2 rounded-full bg-muted/60 overflow-hidden flex">
                  <div
                    className="h-full bg-primary/60 rounded-l-full transition-[width] duration-200"
                    style={{ width: `${(datasetConfig.retrieval.fusion?.alpha ?? DEFAULT_RETRIEVAL_CONFIG.fusion.alpha) * 100}%` }}
                  />
                  <div
                    className="h-full bg-amber-500/40 rounded-r-full transition-[width] duration-200"
                    style={{ width: `${(1 - (datasetConfig.retrieval.fusion?.alpha ?? DEFAULT_RETRIEVAL_CONFIG.fusion.alpha)) * 100}%` }}
                  />
                </div>
              </div>
            )}

            {/* Rerank & MMR */}
            <div className="grid grid-cols-2 gap-3">
              <div className={`p-3.5 rounded-xl border transition-colors ${
                datasetConfig.retrieval?.rerank?.enabled
                  ? "border-emerald-500/20 bg-emerald-500/5"
                  : "border-border/50 bg-muted/20"
              }`}>
                <div className="flex items-center justify-between">
                  <p className="text-sm font-medium text-foreground">Rerank</p>
                  <div className={`w-2 h-2 rounded-full ${datasetConfig.retrieval?.rerank?.enabled ? "bg-emerald-500" : "bg-muted-foreground/30"}`} />
                </div>
                <p className="text-xs text-muted-foreground mt-0.5">{t("knowledge.detail.rerankOptimize")}</p>
                {datasetConfig.retrieval?.rerank?.enabled && datasetConfig.retrieval?.rerank?.model && (
                  <p className="text-[10px] text-emerald-600 dark:text-emerald-400 mt-1.5 font-mono">{datasetConfig.retrieval.rerank.model}</p>
                )}
              </div>
              <div className={`p-3.5 rounded-xl border transition-colors ${
                datasetConfig.retrieval?.mmr?.enabled
                  ? "border-emerald-500/20 bg-emerald-500/5"
                  : "border-border/50 bg-muted/20"
              }`}>
                <div className="flex items-center justify-between">
                  <p className="text-sm font-medium text-foreground">MMR</p>
                  <div className={`w-2 h-2 rounded-full ${datasetConfig.retrieval?.mmr?.enabled ? "bg-emerald-500" : "bg-muted-foreground/30"}`} />
                </div>
                <p className="text-xs text-muted-foreground mt-0.5">{t("knowledge.detail.mmrDedup")}</p>
                {datasetConfig.retrieval?.mmr?.enabled && (
                  <p className="text-[10px] text-emerald-600 dark:text-emerald-400 mt-1.5 font-mono">λ = {(datasetConfig.retrieval.mmr.lambda ?? 0.5).toFixed(2)}</p>
                )}
              </div>
            </div>
          </div>
        ) : (
          <p className="text-muted-foreground">{t("knowledge.detail.cannotLoadConfig")}</p>
        )}
      </Card>

      {datasetConfig?.parsing && (
        <Card className="col-span-2 p-5 space-y-2">
          <h3 className="font-semibold text-foreground">{t("knowledge.detail.parsingAvailability")}</h3>
          <p className="text-sm text-muted-foreground">
            {t(datasetConfig.parsing.enabled
              ? "knowledge.detail.parsingConfigured" : "knowledge.detail.parsingDefault")}
          </p>
          {datasetConfig.parsing.backends.map((backend) => (
            <p key={backend.name} className="text-sm">
              {backend.name}: {t(backend.available
                ? "knowledge.detail.parserAvailable" : "knowledge.detail.parserUnavailable")}
            </p>
          ))}
          {datasetConfig.parsing.warnings.length > 0 && (
            <p role="alert" className="text-sm text-amber-700 dark:text-amber-400">
              {t("knowledge.detail.parserWarning")}
            </p>
          )}
        </Card>
      )}

      {/* Embedding 配置 */}
      <Card className="p-5">
        <p className="mb-2 text-xs text-muted-foreground">
          {t("knowledge.detail.embeddingEffect")}
        </p>
        <div className="flex items-center justify-between mb-4">
          <h3 className="font-semibold text-foreground flex items-center gap-2">
            <Sparkles className="h-5 w-5 text-primary" />
            {t("knowledge.detail.embeddingConfig")}
          </h3>
          {!embeddingEditing && canEditEmbeddingDirectly ? (
            <Button
              variant="ghost"
              size="sm"
              className="h-7 px-2 text-xs text-muted-foreground hover:text-foreground"
              disabled={saveAttempt !== null || !scopeLoaded}
              onClick={() => {
                const p = datasetConfig?.embedding?.provider || "dashscope";
                const m = datasetConfig?.embedding?.model || "text-embedding-v4";
                setEditEmbeddingModel(`${p}:${m}`);
                setEmbeddingEditing(true);
              }}
            >
              <Edit3 className="h-3.5 w-3.5 mr-1" />
              {t("knowledge.detail.modify")}
            </Button>
          ) : null}
          {!embeddingEditing && !canEditEmbeddingDirectly ? (
            <Badge variant="outline" className="border-primary/30 text-primary">
              {t("knowledge.detail.embeddingMigration.blueGreenOnly")}
            </Badge>
          ) : null}
          {embeddingEditing ? (
            <div className="flex items-center gap-1.5">
              <Button
                variant="ghost"
                size="sm"
                className="h-7 px-2 text-xs"
                onClick={() => setEmbeddingEditing(false)}
                disabled={configSaving || embeddingSaving || saveReading || saveAttempt !== null || !scopeLoaded}
              >
                {t("knowledge.detail.cancel")}
              </Button>
              <Button
                size="sm"
                className="h-7 px-3 text-xs"
                onClick={handleSaveEmbeddingConfig}
                disabled={configSaving || embeddingSaving || saveReading || saveAttempt !== null || !scopeLoaded}
              >
                {embeddingSaving ? <Loader2 className="h-3 w-3 animate-spin mr-1" /> : null}
                {t("knowledge.detail.save")}
              </Button>
            </div>
          ) : null}
        </div>

        {datasetConfig && !embeddingEditing && (
          <div className="space-y-3">
            {/* Model label row */}
            <div className="flex items-center gap-3 p-3.5 rounded-xl border border-primary/15 bg-linear-to-r from-primary/5 to-transparent">
              <div className="w-9 h-9 rounded-lg bg-primary/10 dark:bg-primary/15 flex items-center justify-center shrink-0">
                <span className="text-sm font-bold text-primary">
                  {datasetConfig.embedding?.provider === "gemini" ? "G" : "A"}
                </span>
              </div>
              <div className="flex-1 min-w-0">
                <p className="text-xs text-muted-foreground">{t("knowledge.detail.currentModel")}</p>
                <p className="font-semibold text-foreground truncate">
                  {datasetConfig.embedding?.model || t("knowledge.detail.notSet")}
                </p>
              </div>
              <Badge variant="outline" className="text-xs border-primary/30 text-primary shrink-0">
                {t("knowledge.detail.dimension", { dim: datasetConfig.embedding?.dimension || "?" })}
              </Badge>
            </div>

            {/* Provider + Collection */}
            <div className="grid grid-cols-2 gap-3">
              <div className="p-3.5 rounded-xl border border-border/50 bg-muted/30">
                <p className="text-xs text-muted-foreground">Provider</p>
                <p className="text-sm font-medium text-foreground mt-1">
                  {datasetConfig.embedding?.provider === "gemini" ? t("knowledge.detail.googleGemini") : datasetConfig.embedding?.provider === "dashscope" ? t("knowledge.detail.aliDashscope") : datasetConfig.embedding?.provider}
                </p>
              </div>
              <div className="p-3.5 rounded-xl border border-border/50 bg-muted/30">
                <p className="text-xs text-muted-foreground">Collection</p>
                <p className="text-xs font-mono text-foreground mt-1 truncate">
                  {datasetConfig.embedding?.collection_name || t("knowledge.detail.notCreated")}
                </p>
              </div>
            </div>
          </div>
        )}

        {datasetConfig && embeddingEditing && (
          <div className="space-y-4">
            {/* Warning for existing documents */}
            {(datasetConfig.statistics?.document_count ?? 0) > 0 && (
              <div className="p-3 rounded-lg bg-amber-500/10 dark:bg-amber-500/15 border border-amber-500/20 text-sm">
                <p className="font-medium text-amber-700 dark:text-amber-400">
                  {t("knowledge.detail.docCountWarning", { count: datasetConfig.statistics?.document_count })}
                </p>
                <p className="text-xs text-amber-600 dark:text-amber-400/70 mt-1">
                  {t("knowledge.detail.dimensionChangeWarning")}
                </p>
              </div>
            )}

            <div>
              <Label className="text-sm font-medium">{t("knowledge.detail.embeddingConfig")}</Label>
              <Select value={editEmbeddingModel} onValueChange={setEditEmbeddingModel}>
                <SelectTrigger className="mt-2">
                  <SelectValue placeholder={t("knowledge.detail.selectEmbedding")} />
                </SelectTrigger>
                <SelectContent>
                  {EMBEDDING_MODELS.map((m) => (
                    <SelectItem key={`${m.provider}:${m.model}`} value={`${m.provider}:${m.model}`}>
                      <div className="flex items-center gap-2">
                        <span className="w-5 h-5 rounded bg-primary/10 flex items-center justify-center text-[10px] font-bold text-primary shrink-0">
                          {m.provider === "gemini" ? "G" : m.provider === "siliconflow" ? "S" : "A"}
                        </span>
                        <span>{m.label}</span>
                        <span className="text-muted-foreground text-xs">{t("knowledge.detail.dimensionValue", { dimension: m.dimension })}</span>
                      </div>
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>

            {/* Preview selected model info */}
            {editEmbeddingModel && (() => {
              const [p, m] = editEmbeddingModel.split(":");
              const sel = EMBEDDING_MODELS.find((em) => em.provider === p && em.model === m);
              const currentDim = datasetConfig.embedding?.dimension;
              const newDim = sel?.dimension;
              const dimChanged = currentDim && newDim && currentDim !== newDim;
              return sel ? (
                <div className={`p-3 rounded-lg border text-sm ${dimChanged ? "border-red-500/20 bg-red-500/5" : "border-border/50 bg-muted/20"}`}>
                  <div className="flex items-center justify-between">
                    <span className="text-muted-foreground">Dimension</span>
                    <span className={`font-mono font-medium ${dimChanged ? "text-red-500" : "text-foreground"}`}>
                      {currentDim} → {newDim}
                      {dimChanged && ` ${t("knowledge.detail.needsReindex")}`}
                    </span>
                  </div>
                </div>
              ) : null;
            })()}
          </div>
        )}
      </Card>

      {datasetId ? (
        <EmbeddingMigrationPanel
          datasetId={datasetId}
          active={active}
          onMigrationChange={handleMigrationChange}
        />
      ) : null}

      {/* 统计信息 */}
      <Card className="p-5">
        <h3 className="font-semibold text-foreground mb-4 flex items-center gap-2">
          <BarChart3 className="h-5 w-5 text-accent" />
          {t("knowledge.detail.statistics")}
        </h3>

        {datasetConfig?.statistics ? (
          <div className="grid grid-cols-2 gap-4">
            <div className="p-4 bg-primary/10 dark:bg-primary/15 rounded-lg text-center border border-primary/10 dark:border-primary/20">
              <p className="text-2xl font-bold text-primary">
                {datasetConfig.statistics.document_count ?? 0}
              </p>
              <p className="text-xs text-primary/70 mt-1">{t("knowledge.detail.totalDocuments")}</p>
            </div>
            <div className="p-4 bg-emerald-500/10 dark:bg-emerald-500/15 rounded-lg text-center border border-emerald-500/10 dark:border-emerald-500/20">
              <p className="text-2xl font-bold text-emerald-600 dark:text-emerald-400">
                {datasetConfig.statistics.segment_count ?? 0}
              </p>
              <p className="text-xs text-emerald-600/70 dark:text-emerald-400/70 mt-1">{t("knowledge.detail.totalSegments")}</p>
            </div>
            <div className="p-4 bg-amber-500/10 dark:bg-amber-500/15 rounded-lg text-center border border-amber-500/10 dark:border-amber-500/20">
              <p className="text-2xl font-bold text-amber-600 dark:text-amber-400">
                {datasetConfig.statistics.available_segment_count ?? 0}
              </p>
              <p className="text-xs text-amber-600/70 dark:text-amber-400/70 mt-1">{t("knowledge.detail.availableSegments")}</p>
            </div>
            <div className="p-4 bg-accent/10 dark:bg-accent/15 rounded-lg text-center border border-accent/10 dark:border-accent/20">
              <p className="text-2xl font-bold text-accent">
                {datasetConfig.statistics.hit_count ?? 0}
              </p>
              <p className="text-xs text-accent/70 mt-1">{t("knowledge.detail.hitCount")}</p>
            </div>
          </div>
        ) : (
          <p className="text-muted-foreground text-center py-4">{t("knowledge.detail.loadingStats")}</p>
        )}

        {datasetConfig?.statistics?.segment_count === 0 && (
          <div className="mt-4 p-3 bg-amber-500/10 dark:bg-amber-500/15 border border-amber-500/20 rounded-lg text-sm text-amber-700 dark:text-amber-400">
            {t("knowledge.detail.noSegmentsWarning")}
          </div>
        )}
      </Card>

      {/* 分块预览 */}
      <Card className="p-5 col-span-2">
        <h3 className="font-semibold text-foreground mb-4 flex items-center gap-2">
          <Eye className="h-5 w-5 text-accent" />
          {t("knowledge.detail.chunkPreviewLabel")}
        </h3>

        <div className="grid grid-cols-2 gap-4">
          <div className="space-y-3">
            <Textarea
              placeholder={t("knowledge.detail.chunkPreviewPlaceholder")}
              value={previewText}
              onChange={(e) => setPreviewText(e.target.value)}
              rows={8}
              className="font-mono text-sm"
            />
            <div className="flex items-center gap-2">
              <Button
                onClick={handlePreviewChunks}
                disabled={previewLoading || !previewText.trim()}
                className="bg-accent hover:bg-accent/90"
              >
                {previewLoading ? (
                  <><Loader2 className="h-4 w-4 mr-2 animate-spin" /> {t("knowledge.detail.processing")}</>
                ) : (
                  <><Play className="h-4 w-4 mr-2" /> {t("knowledge.detail.previewChunks")}</>
                )}
              </Button>
              <span className="text-xs text-muted-foreground">
                {t("knowledge.detail.previewConfigInfo", { mode: editChunkingMode, size: editChunkSize, overlap: editChunkOverlap })}
              </span>
            </div>
          </div>
          <div className="border rounded-lg p-3 bg-muted/40 max-h-[300px] overflow-auto">
            {previewChunksResult.length > 0 ? (
              <div className="space-y-2">
                <div className="text-xs text-muted-foreground mb-2">
                  {t("knowledge.detail.totalChunks", { count: previewChunksResult.length })}
                </div>
                {previewChunksResult.map((chunk, i) => (
                  <div key={i} className="p-2 bg-card rounded border text-xs">
                    <div className="flex items-center gap-2 mb-1">
                      <Badge variant="secondary">#{i + 1}</Badge>
                      <span className="text-muted-foreground/70">{t("knowledge.detail.characters", { count: chunk.char_count })}</span>
                      <span className="text-muted-foreground/70">~{chunk.token_count} tokens</span>
                    </div>
                    <div className="text-foreground/80 whitespace-pre-wrap line-clamp-3">
                      {chunk.content}
                    </div>
                  </div>
                ))}
              </div>
            ) : (
              <div className="text-center text-muted-foreground/70 py-8">
                {t("knowledge.detail.previewHint")}
              </div>
            )}
          </div>
        </div>
      </Card>

      {/* 调试信息 */}
      {debugInfo && (
        <Card className="p-5 col-span-2">
          <h3 className="font-semibold text-foreground mb-4 flex items-center gap-2">
            <HelpCircle className="h-5 w-5 text-muted-foreground" />
            {t("knowledge.detail.debugInfo")}
          </h3>

          <div className="space-y-4">
            <div className="grid grid-cols-2 gap-4">
              <div className={`p-3 rounded-lg ${debugInfo.has_segments ? "bg-emerald-500/10 dark:bg-emerald-500/15" : "bg-red-500/10 dark:bg-red-500/15"}`}>
                <p className="text-xs text-muted-foreground">{t("knowledge.detail.dbSegments")}</p>
                <p className={`font-medium ${debugInfo.has_segments ? "text-emerald-600 dark:text-emerald-400" : "text-red-600 dark:text-red-400"}`}>
                  {debugInfo.has_segments ? t("knowledge.detail.segmentsExist") : t("knowledge.detail.noSegmentsExist")}
                </p>
              </div>
              <div className={`p-3 rounded-lg ${debugInfo.has_collection ? "bg-emerald-500/10 dark:bg-emerald-500/15" : "bg-red-500/10 dark:bg-red-500/15"}`}>
                <p className="text-xs text-muted-foreground">{t("knowledge.detail.vectorCollection")}</p>
                <p className={`font-medium ${debugInfo.has_collection ? "text-emerald-600 dark:text-emerald-400" : "text-red-600 dark:text-red-400"}`}>
                  {debugInfo.has_collection ? t("knowledge.detail.collectionCreated") : t("knowledge.detail.collectionNotCreated")}
                </p>
              </div>
            </div>

            {Array.isArray(debugInfo.sample_segments) && debugInfo.sample_segments.length > 0 && (
              <div>
                <p className="text-xs text-muted-foreground mb-2">{t("knowledge.detail.sampleSegments")}</p>
                <div className="space-y-2">
                  {(debugInfo.sample_segments as Array<Record<string, unknown>>).slice(0, 2).map((seg, i) => (
                    <div key={i} className="p-2 bg-muted/40 rounded text-xs font-mono">
                      <div className="text-muted-foreground">ID: {String(seg.segment_id).slice(0, 16)}...</div>
                      <div className="text-foreground/80 mt-1">{String(seg.text_preview)}</div>
                    </div>
                  ))}
                </div>
              </div>
            )}

            {!debugInfo.has_segments && (
              <div className="p-3 bg-red-500/10 dark:bg-red-500/15 border border-red-500/20 rounded-lg text-sm text-red-700 dark:text-red-400">
                <p className="font-medium">{t("knowledge.detail.diagnostics")}</p>
                <ul className="list-disc list-inside mt-1 space-y-1">
                  <li>{t("knowledge.detail.diagnosticStatus")}</li>
                  <li>{t("knowledge.detail.diagnosticApiKey")}</li>
                  <li>{t("knowledge.detail.diagnosticReindex")}</li>
                </ul>
              </div>
            )}
          </div>
        </Card>
      )}

      {/* API 配置展示 */}
      <Card className="p-5 col-span-2">
        <h3 className="font-semibold text-foreground mb-4 flex items-center gap-2">
          <Code className="h-5 w-5 text-violet-500" />
          {t("knowledge.detail.apiCall")}
        </h3>

        <div className="space-y-4">
          {/* API 端点信息 */}
          <div className="grid grid-cols-2 gap-4">
            <div className="p-4 bg-muted/40 rounded-lg">
              <div className="flex items-center justify-between mb-2">
                <p className="text-xs text-muted-foreground font-medium">{t("knowledge.detail.retrieveEndpoint")}</p>
                <button
                  onClick={() => handleCopy(`${getApiBaseUrl()}/api/v1/knowledge/${datasetId}/retrieve`, "retrieve-url")}
                  className="text-muted-foreground hover:text-foreground transition-colors"
                  title={copiedKey === "retrieve-url" ? t("knowledge.detail.copied") : t("knowledge.detail.copy")}
                >
                  {copiedKey === "retrieve-url" ? (
                    <Check className="h-3.5 w-3.5 text-green-500" />
                  ) : (
                    <Copy className="h-3.5 w-3.5" />
                  )}
                </button>
              </div>
              <code className="text-xs font-mono text-foreground/80 break-all">
                POST /api/v1/knowledge/{datasetId}/retrieve
              </code>
            </div>
            <div className="p-4 bg-muted/40 rounded-lg">
              <div className="flex items-center justify-between mb-2">
                <p className="text-xs text-muted-foreground font-medium">{t("knowledge.detail.qaEndpoint")}</p>
                <button
                  onClick={() => handleCopy(`${getApiBaseUrl()}/api/v1/knowledge/${datasetId}/qa`, "qa-url")}
                  className="text-muted-foreground hover:text-foreground transition-colors"
                  title={copiedKey === "qa-url" ? t("knowledge.detail.copied") : t("knowledge.detail.copy")}
                >
                  {copiedKey === "qa-url" ? (
                    <Check className="h-3.5 w-3.5 text-green-500" />
                  ) : (
                    <Copy className="h-3.5 w-3.5" />
                  )}
                </button>
              </div>
              <code className="text-xs font-mono text-foreground/80 break-all">
                POST /api/v1/knowledge/{datasetId}/qa
              </code>
            </div>
          </div>

          {/* 代码示例 Tab */}
          <div className="border rounded-lg overflow-hidden">
            <div className="bg-slate-900 text-white">
              <div className="flex items-center gap-2 px-4 py-2 border-b border-slate-700">
                <Terminal className="h-4 w-4 text-slate-400" />
                <span className="text-sm font-medium">{t("knowledge.detail.requestExample")}</span>
                <div className="ml-auto flex items-center gap-1">
                  <button
                    onClick={() => {
                      const curlCmd = `curl -X POST '${getApiBaseUrl()}/api/v1/knowledge/${datasetId}/retrieve' \\
  -H 'Content-Type: application/json' \\
  -H 'Authorization: Bearer YOUR_API_KEY' \\
  -d '{
    "query": "your query",
    "top_k": 5,
    "mode": "hybrid"
  }'`;
                      handleCopy(curlCmd, "curl");
                    }}
                    className="px-2 py-1 text-xs bg-slate-700 hover:bg-slate-600 rounded transition-colors flex items-center gap-1"
                  >
                    {copiedKey === "curl" ? <Check className="h-3 w-3" /> : <Copy className="h-3 w-3" />}
                    {copiedKey === "curl" ? t("knowledge.detail.copied") : t("knowledge.detail.copy")}
                  </button>
                </div>
              </div>
              <pre className="p-4 text-xs overflow-x-auto font-mono leading-relaxed">
                <code className="text-green-400">{t("knowledge.detail.retrieveComment")}</code>
                {"\n"}curl -X POST <span className="text-yellow-300">'{getApiBaseUrl()}/api/v1/knowledge/{datasetId}/retrieve'</span> \
                {"\n"}  -H <span className="text-cyan-300">'Content-Type: application/json'</span> \
                {"\n"}  -H <span className="text-cyan-300">'Authorization: Bearer YOUR_API_KEY'</span> \
                {"\n"}  -d <span className="text-orange-300">{'\'{\n    "query": "your query",\n    "top_k": 5,\n    "mode": "hybrid"\n  }\''}</span>
              </pre>
            </div>
          </div>

          {/* Python 示例 */}
          <div className="border rounded-lg overflow-hidden">
            <div className="bg-slate-900 text-white">
              <div className="flex items-center gap-2 px-4 py-2 border-b border-slate-700">
                <Code className="h-4 w-4 text-slate-400" />
                <span className="text-sm font-medium">Python</span>
                <div className="ml-auto flex items-center gap-1">
                  <button
                    onClick={() => {
                      const pythonCode = `import requests

url = "${getApiBaseUrl()}/api/v1/knowledge/${datasetId}/retrieve"
headers = {
    "Content-Type": "application/json",
    "Authorization": "Bearer YOUR_API_KEY"
}
payload = {
    "query": "your query",
    "top_k": 5,
    "mode": "hybrid"  # vector, keyword, or hybrid
}

response = requests.post(url, json=payload, headers=headers)
results = response.json()

for chunk in results.get("chunks", []):
    print(f"Score: {chunk['score']:.3f}")
    print(f"Content: {chunk['content'][:200]}...")
    print("-" * 50)`;
                      handleCopy(pythonCode, "python");
                    }}
                    className="px-2 py-1 text-xs bg-slate-700 hover:bg-slate-600 rounded transition-colors flex items-center gap-1"
                  >
                    {copiedKey === "python" ? <Check className="h-3 w-3" /> : <Copy className="h-3 w-3" />}
                    {copiedKey === "python" ? t("knowledge.detail.copied") : t("knowledge.detail.copy")}
                  </button>
                </div>
              </div>
              <pre className="p-4 text-xs overflow-x-auto font-mono leading-relaxed">
                <code className="text-purple-400">import</code> <code className="text-white">requests</code>
                {"\n\n"}url = <span className="text-yellow-300">"{getApiBaseUrl()}/api/v1/knowledge/{datasetId}/retrieve"</span>
                {"\n"}headers = {"{"}
                {"\n"}    <span className="text-cyan-300">"Content-Type"</span>: <span className="text-yellow-300">"application/json"</span>,
                {"\n"}    <span className="text-cyan-300">"Authorization"</span>: <span className="text-yellow-300">"Bearer YOUR_API_KEY"</span>
                {"\n"}{"}"}
                {"\n"}payload = {"{"}
                {"\n"}    <span className="text-cyan-300">"query"</span>: <span className="text-yellow-300">"your query"</span>,
                {"\n"}    <span className="text-cyan-300">"top_k"</span>: <span className="text-orange-300">5</span>,
                {"\n"}    <span className="text-cyan-300">"mode"</span>: <span className="text-yellow-300">"hybrid"</span>
                {"\n"}{"}"}
                {"\n\n"}response = requests.post(url, json=payload, headers=headers)
                {"\n"}results = response.json()
              </pre>
            </div>
          </div>

          {/* 提示信息 */}
          <div className="p-3 bg-violet-50 border border-violet-200 rounded-lg text-sm text-violet-700 flex items-start gap-2">
            <ExternalLink className="h-4 w-4 mt-0.5 shrink-0" />
            <div>
              <p className="font-medium">{t("knowledge.detail.integrationGuide")}</p>
              <p className="text-xs mt-1 text-violet-600">
                {t("knowledge.detail.integrationHint")}
              </p>
            </div>
          </div>
        </div>
      </Card>
    </div>
  );
}
