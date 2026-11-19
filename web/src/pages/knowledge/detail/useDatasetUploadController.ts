import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";

import {
  batchUploadDocuments,
  uploadDocument,
  updateDatasetConfig,
  type ProcessingMode,
} from "@/api/knowledge";
import { toast } from "@/hooks/use-toast";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { beginUploadRecovery, readUploadRecovery, updateUploadRecovery, uploadRecoveryKey, writeUploadRecovery,
  type UploadRecoveryReceipt, type UploadRecoveryFile } from "@/lib/knowledgeUploadRecovery";
import { getSourceUploadError, isDefiniteUploadRejection } from "@/pages/knowledge/create/datasetCreateModel";
import {
  buildDatasetUploadConfigPatch,
  uploadDatasetFiles,
} from "@/pages/knowledge/detail/datasetUploadModel";
import { DEFAULT_CHUNKING_CONFIG, DEFAULT_RETRIEVAL_CONFIG, type Dataset } from "@/types/knowledge";

export const DATASET_EMBEDDING_MODELS = [
  {
    provider: "gemini",
    model: "gemini-embedding-2-preview",
    label: "Gemini Embedding 2 Preview",
    dimension: 1024,
  },
  {
    provider: "gemini",
    model: "gemini-embedding-001",
    label: "Gemini Embedding 001",
    dimension: 1024,
  },
  {
    provider: "dashscope",
    model: "text-embedding-v4",
    label: "DashScope text-embedding-v4",
    dimension: 1024,
    badge: "knowledge.upload.recommended",
  },
  {
    provider: "dashscope",
    model: "text-embedding-v3",
    label: "DashScope text-embedding-v3",
    dimension: 1024,
  },
  {
    provider: "siliconflow",
    model: "BAAI/bge-m3",
    label: "SiliconFlow BGE-M3",
    dimension: 1024,
  },
  {
    provider: "siliconflow",
    model: "BAAI/bge-large-zh-v1.5",
    label: "SiliconFlow BGE-Large-ZH",
    dimension: 1024,
  },
] as const;

interface DatasetUploadControllerOptions {
  datasetId?: string;
  dataset?: Dataset;
  pendingFiles: File[];
  onOpenChange: (open: boolean) => void;
  onPendingFilesChange: (files: File[]) => void;
  onUploadingChange: (uploading: boolean) => void;
}

export function useDatasetUploadController({
  datasetId,
  dataset,
  pendingFiles,
  onOpenChange,
  onPendingFilesChange,
  onUploadingChange,
}: DatasetUploadControllerOptions) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { id: authenticatedScopeId, isCurrent: isAuthenticatedScopeCurrent } = useAuthenticatedReadScope();
  const authority = useAuthStore((state) => sdkAuthorityScope(state.user));
  const token = useAuthStore((state) => state.token);
  const authenticated = useAuthStore((state) => state.isAuthenticated);
  const uploadScope = JSON.stringify([authenticatedScopeId, authority, authenticated, dataset?.tenant_id, datasetId, dataset?.my_permission]);
  const recoveryKey = token && authenticated && dataset?.dataset_id === datasetId
    && ["owner", "editor"].includes(dataset?.my_permission ?? "")
    ? uploadRecoveryKey(authority, dataset?.tenant_id, datasetId) : null;
  const [recoveryState, setRecoveryState] = useState<{ owner: string; key: string | null; receipts: UploadRecoveryReceipt[]; failed: boolean }>(
    { owner: authenticatedScopeId, key: null, receipts: [], failed: false }
  );
  const recoveryReceipts = isAuthenticatedScopeCurrent() && recoveryState.owner === authenticatedScopeId
    && recoveryState.key === recoveryKey ? recoveryState.receipts : [];
  const recoveryFailed = isAuthenticatedScopeCurrent() && recoveryState.owner === authenticatedScopeId
    && recoveryState.key === recoveryKey && recoveryState.failed;
  function reloadRecovery() {
    if (!recoveryKey || !isAuthenticatedScopeCurrent()) return;
    try { setRecoveryState({ owner: authenticatedScopeId, key: recoveryKey, receipts: readUploadRecovery(recoveryKey), failed: false }); }
    catch { setRecoveryState({ owner: authenticatedScopeId, key: recoveryKey, receipts: [], failed: true }); }
  }
  useEffect(() => {
    if (!recoveryKey || !isAuthenticatedScopeCurrent()) { setRecoveryState({ owner: authenticatedScopeId, key: null, receipts: [], failed: false }); return; }
    try { setRecoveryState({ owner: authenticatedScopeId, key: recoveryKey, receipts: readUploadRecovery(recoveryKey), failed: false }); }
    catch { setRecoveryState({ owner: authenticatedScopeId, key: recoveryKey, receipts: [], failed: true }); }
  }, [recoveryKey, authenticatedScopeId, isAuthenticatedScopeCurrent]);
  const inFlightUpload = useRef<symbol | null>(null);
  const uploadOwner = useRef({ scope: uploadScope, token, generation: 0, mounted: true, pendingFiles });
  if (uploadOwner.current.scope !== uploadScope || uploadOwner.current.token !== token) {
    uploadOwner.current.token = token;
    uploadOwner.current.scope = uploadScope;
    uploadOwner.current.generation += 1;
    inFlightUpload.current = null;
  }
  uploadOwner.current.pendingFiles = pendingFiles;
  const ownerGeneration = uploadOwner.current.generation;
  useEffect(() => {
    const owner = uploadOwner.current;
    owner.mounted = true;
    return () => { owner.mounted = false; inFlightUpload.current = null; };
  }, []);
  useEffect(() => {
    // A retired owner's finalizer must not clear a new owner's busy state.
    if (!inFlightUpload.current) onUploadingChange(false);
  }, [uploadScope, token, onUploadingChange]);
  const [uploadChunkMode, setUploadChunkMode] = useState(DEFAULT_CHUNKING_CONFIG.mode);
  const [uploadChunkSize, setUploadChunkSize] = useState(DEFAULT_CHUNKING_CONFIG.chunk_size);
  const [uploadChunkOverlap, setUploadChunkOverlap] = useState(DEFAULT_CHUNKING_CONFIG.chunk_overlap);
  const [uploadMinParagraphLength, setUploadMinParagraphLength] = useState(50);
  const [uploadMergeShortParagraphs, setUploadMergeShortParagraphs] = useState(true);
  const [uploadHeadingLevel, setUploadHeadingLevel] = useState<"h1" | "h2" | "h3">("h2");
  const [uploadParentChunkSize, setUploadParentChunkSize] = useState(1500);
  const [uploadChildChunkSize, setUploadChildChunkSize] = useState(300);
  const [uploadChildOverlap, setUploadChildOverlap] = useState(50);
  const [uploadSeparator, setUploadSeparator] = useState("\\n\\n");
  const [uploadKeepSeparator, setUploadKeepSeparator] = useState(false);
  const [uploadRegexPattern, setUploadRegexPattern] = useState("");
  const [uploadQuestionPrefix, setUploadQuestionPrefix] = useState("Q:");
  const [uploadAnswerPrefix, setUploadAnswerPrefix] = useState("A:");
  const [uploadMetadataEnabled, setUploadMetadataEnabled] = useState(false);
  const [uploadExtractTitle, setUploadExtractTitle] = useState(true);
  const [uploadExtractSummary, setUploadExtractSummary] = useState(false);
  const [uploadExtractKeywords, setUploadExtractKeywords] = useState(true);
  const [uploadExtractEntities, setUploadExtractEntities] = useState(false);
  const [uploadDetectLanguage, setUploadDetectLanguage] = useState(true);
  const [uploadTableEnabled, setUploadTableEnabled] = useState(false);
  const [uploadTableMode, setUploadTableMode] = useState<
    "markdown" | "row_based" | "structured"
  >("markdown");
  const [uploadTableIncludeHeaders, setUploadTableIncludeHeaders] = useState(true);
  const [uploadTableGenerateSummary, setUploadTableGenerateSummary] = useState(false);
  const [uploadEmbeddingModel, setUploadEmbeddingModel] = useState(
    "dashscope:text-embedding-v4"
  );
  // NOTE: rerank defaults ON here but OFF in the platform defaults
  // (DEFAULT_RETRIEVAL_CONFIG.rerank.enabled). The upload dialog has always
  // shipped this way; changing it is a product decision, tracked for C3.
  const [rerankEnabled, setRerankEnabled] = useState(true);
  const [rerankModel, setRerankModel] = useState(DEFAULT_RETRIEVAL_CONFIG.rerank.model);
  const [uploadFailures, setUploadFailures] = useState<
    Array<{ name: string; position: number; sizeBytes: number; error: string; documentId?: string; retrySafe: boolean }>
  >([]);
  const uploadProcessingMode: ProcessingMode = "text_only";

  // Adopt the dataset's current embedding as the dialog selection once it is
  // known — confirming an upload used to silently swap the dataset's model
  // for the hard-coded default below.
  const datasetEmbedding =
    dataset?.embedding_provider && dataset?.embedding_model
      ? `${dataset.embedding_provider}:${dataset.embedding_model}`
      : null;
  useEffect(() => {
    if (datasetEmbedding) setUploadEmbeddingModel(datasetEmbedding);
  }, [datasetEmbedding]);

  function buildChunkingConfig() {
    const baseConfig: Record<string, unknown> = { mode: uploadChunkMode };

    switch (uploadChunkMode) {
      case "fixed_size":
        baseConfig.chunk_size = uploadChunkSize;
        baseConfig.chunk_overlap = uploadChunkOverlap;
        break;
      case "paragraph":
        baseConfig.chunk_size = uploadChunkSize;
        baseConfig.min_paragraph_length = uploadMinParagraphLength;
        baseConfig.merge_short_paragraphs = uploadMergeShortParagraphs;
        break;
      case "heading":
        baseConfig.chunk_size = uploadChunkSize;
        baseConfig.heading_level = uploadHeadingLevel;
        break;
      case "hierarchical":
        baseConfig.parent_chunk_size = uploadParentChunkSize;
        baseConfig.child_chunk_size = uploadChildChunkSize;
        baseConfig.child_overlap = uploadChildOverlap;
        break;
      case "separator":
        baseConfig.chunk_size = uploadChunkSize;
        baseConfig.primary_separator = uploadSeparator;
        baseConfig.keep_separator = uploadKeepSeparator;
        break;
      case "regex":
        baseConfig.chunk_size = uploadChunkSize;
        baseConfig.regex_pattern = uploadRegexPattern;
        break;
      case "qa":
        baseConfig.question_prefix = uploadQuestionPrefix;
        baseConfig.answer_prefix = uploadAnswerPrefix;
        break;
      case "recursive":
        baseConfig.chunk_size = uploadChunkSize;
        baseConfig.chunk_overlap = uploadChunkOverlap;
        break;
      default:
        break;
    }

    if (uploadMetadataEnabled) {
      const metadataFields = [
        uploadExtractTitle && "title",
        uploadExtractSummary && "summary",
        uploadExtractKeywords && "keywords",
        uploadExtractEntities && "entities",
        uploadDetectLanguage && "language",
      ].filter((field): field is string => Boolean(field));
      baseConfig.extract_metadata = metadataFields.length > 0;
      baseConfig.metadata_fields = metadataFields;
    } else {
      baseConfig.extract_metadata = false;
      baseConfig.metadata_fields = [];
    }

    return baseConfig;
  }

  /**
   * Mirror the ChunkingConfigSchema validators so a doomed config update is
   * blocked before the upload starts. Modes that omit chunk_size/
   * chunk_overlap fall back to the schema defaults (2000 / 300) for the
   * check.
   */
  function chunkingValidationError(): string | null {
    if (uploadChunkMode === "regex") {
      return t("knowledge.detail.chunkRegexDisabled");
    }
    const sendsSize =
      uploadChunkMode === "fixed_size" ||
      uploadChunkMode === "paragraph" ||
      uploadChunkMode === "heading" ||
      uploadChunkMode === "separator" ||
      uploadChunkMode === "recursive";
    const sendsOverlap =
      uploadChunkMode === "fixed_size" || uploadChunkMode === "recursive";
    const effectiveSize = sendsSize ? uploadChunkSize : 2000;
    const effectiveOverlap = sendsOverlap ? uploadChunkOverlap : 300;
    if (effectiveOverlap >= effectiveSize) {
      return t("knowledge.detail.chunkOverlapTooLarge");
    }
    if (uploadChunkMode === "hierarchical" && uploadChildOverlap >= uploadChildChunkSize) {
      return t("knowledge.detail.childOverlapTooLarge");
    }
    return null;
  }

  async function handleConfirmUpload() {
    if (!datasetId || !recoveryKey || pendingFiles.length === 0 || inFlightUpload.current
      || !isAuthenticatedScopeCurrent() || !uploadOwner.current.mounted || uploadOwner.current.generation !== ownerGeneration) return;

    const validationError = chunkingValidationError();
    if (validationError) {
      toast.error(t("knowledge.detail.uploadConfigInvalid"), validationError);
      return;
    }

    const filesToUpload = [...pendingFiles];
    const submittedFiles = new Set(filesToUpload);
    const originalAuth = useAuthStore.getState();
    let authRetired = false;
    const unsubscribe = useAuthStore.subscribe((current, previous) => {
      if (current.token !== previous.token || current.isAuthenticated !== previous.isAuthenticated
        || sdkAuthorityScope(current.user) !== sdkAuthorityScope(previous.user)) authRetired = true;
    });
    const attempt = Symbol("dataset-upload");
    inFlightUpload.current = attempt;
    const ownsAttempt = () => inFlightUpload.current === attempt
      && uploadOwner.current.mounted && uploadOwner.current.generation === ownerGeneration
      && isAuthenticatedScopeCurrent() && !authRetired && Boolean(originalAuth.token && originalAuth.isAuthenticated)
      && useAuthStore.getState().token === originalAuth.token
      && sdkAuthorityScope(useAuthStore.getState().user) === sdkAuthorityScope(originalAuth.user);
    const settleFiles = (retryableFiles: File[]) => {
      const retryable = new Set(retryableFiles);
      const remaining = uploadOwner.current.pendingFiles.filter((file) =>
        !submittedFiles.has(file) || retryable.has(file));
      onPendingFilesChange(remaining);
      return remaining;
    };
    setUploadFailures([]);
    onUploadingChange(true);
    let uploadStarted = false;
    let recovery: UploadRecoveryReceipt | null = null;
    const recordFiles = (files: File[], state: UploadRecoveryFile["state"], documentId?: string) => {
      if (!ownsAttempt() || !recovery) return false;
      const changed = new Set(files);
      const next: UploadRecoveryReceipt = { ...recovery, files: recovery.files.map((item, index) =>
        changed.has(filesToUpload[index]) ? { ...item, state, ...(documentId ? { documentId } : {}) } : item) };
      try {
        if (!updateUploadRecovery(recoveryKey, next)) return false;
        recovery = next;
        reloadRecovery();
        return true;
      } catch {
        // A failed local save never changes an observed HTTP result or replays it.
        if (state !== "unknown") recovery = next;
        setRecoveryState((current) => ({ owner: authenticatedScopeId, key: recoveryKey, failed: true,
          receipts: current.key === recoveryKey
            ? current.receipts.map((item) => item.id === next.id ? (state === "unknown" ? recovery! : next) : item) : [],
        }));
        return false;
      }
    };
    const beforeDispatch = (files: File[]) => {
      if (!recordFiles(files, "unknown")) throw new Error("Upload owner or recovery receipt retired before dispatch");
    };
    const recordFailure = (files: File[], error: unknown) => {
      if (ownsAttempt()) recordFiles(files, isDefiniteUploadRejection(error) ? "rejected" : "unknown");
    };

    try {
      recovery = beginUploadRecovery(recoveryKey, filesToUpload);
      reloadRecovery();
      const chunkingConfig = buildChunkingConfig();
      const [embeddingProvider, embeddingModel] = uploadEmbeddingModel.split(":");
      const selectedModel = DATASET_EMBEDDING_MODELS.find(
        (model) => model.provider === embeddingProvider && model.model === embeddingModel
      );
      // Only touch the dataset's embedding when the selection actually
      // differs — the PUT would otherwise re-set (or swap) it on every
      // upload.
      const configPatch: Parameters<typeof updateDatasetConfig>[1] =
        buildDatasetUploadConfigPatch({
          documentCount: dataset?.statistics?.document_count,
          chunkingConfig,
          rerank: { enabled: rerankEnabled, model: rerankModel },
          embedding: {
            changed: !datasetEmbedding || datasetEmbedding !== uploadEmbeddingModel,
            provider: embeddingProvider,
            model: embeddingModel,
            dimension: selectedModel?.dimension || 1024,
          },
        });
      await updateDatasetConfig(datasetId, configPatch);
      if (!ownsAttempt()) return;

      const describeError = (error: unknown) =>
        isDefiniteUploadRejection(error)
          ? getSourceUploadError(error, {
              fallback: t("knowledge.detail.uploadFailed"),
              requestTooLarge: t("knowledge.create.uploadTooLarge"),
            })
          : t("knowledge.detail.uploadOutcomeUnknown");
      uploadStarted = true;
      const outcome = await uploadDatasetFiles(filesToUpload, {
        uploadBatch: async (files) => {
          beforeDispatch(files);
          try {
            const receipt = await batchUploadDocuments(datasetId, files);
            // Reuse the existing strict per-position reconciliation before recording facts.
            const checked = await uploadDatasetFiles(files, {
              uploadBatch: async () => receipt, uploadOne: async () => undefined,
              describeError, unknownOutcomeMessage: t("knowledge.detail.uploadOutcomeUnknown"),
            });
            if (ownsAttempt()) {
              const failed = new Map(checked.failures.map((item) => [item.file, item]));
              // Both existing upload owners return accepted documents in submitted order.
              const acceptedDocuments = Array.isArray(receipt.documents) && receipt.documents.length === checked.accepted
                ? receipt.documents : [];
              let acceptedIndex = 0;
              for (const file of files) {
                const failure = failed.get(file);
                const documentId = failure ? failure.documentId : acceptedDocuments[acceptedIndex++]?.document_id;
                recordFiles([file], !failure ? "accepted" : failure.documentId ? "created"
                  : failure.retrySafe === false ? "unknown" : "rejected", typeof documentId === "string" ? documentId : undefined);
              }
            }
            return receipt;
          } catch (error) { recordFailure(files.filter((file) => recovery?.files[filesToUpload.indexOf(file)].state === "unknown"), error); throw error; }
        },
        uploadOne: async (file) => {
          beforeDispatch([file]);
          try {
            const document = await uploadDocument(datasetId, file, uploadProcessingMode);
            recordFiles([file], "accepted", typeof document.document_id === "string" ? document.document_id : undefined);
            return document;
          } catch (error) { recordFailure([file], error); throw error; }
        },
        describeError,
        isRetrySafe: isDefiniteUploadRejection,
        unknownOutcomeMessage: t("knowledge.detail.uploadOutcomeUnknown"),
      });

      if (!ownsAttempt()) return;
      // Read refresh failure cannot turn already observed acceptance into uncertainty.
      await Promise.allSettled([
        queryClient.invalidateQueries({ queryKey: ["kb-documents", datasetId] }),
        queryClient.invalidateQueries({ queryKey: ["kb-dataset", datasetId] }),
        queryClient.invalidateQueries({ queryKey: ["kb-dataset-sources", datasetId] }),
      ]);
      if (!ownsAttempt()) return;

      if (outcome.failures.length > 0) {
        setUploadFailures(outcome.failures.map(({ file, error, documentId, retrySafe }) => ({
          name: file.name,
          position: filesToUpload.indexOf(file) + 1,
          sizeBytes: file.size,
          error,
          documentId,
          retrySafe: retrySafe !== false,
        })));
        // An enqueue rejection may already have created a document record.
        // Re-uploading that source would create a second document.
        settleFiles(outcome.failures.filter(({ retrySafe }) => retrySafe !== false).map(({ file }) => file));
        toast.warning(
          t("knowledge.detail.uploadDone", {
            success: outcome.accepted,
            failed: outcome.failures.length,
          }),
          outcome.failures.map(({ file, error, documentId, retrySafe }) =>
            `#${filesToUpload.indexOf(file) + 1} ${file.name}: ${error}${documentId ? ` — ${t("knowledge.detail.uploadRecordCreated")}` : retrySafe === false ? ` — ${t("knowledge.detail.uploadInspectBeforeRetry")}` : ""}`
          ).join("; ")
        );
        return;
      }

      const remaining = settleFiles([]);
      if (remaining.length === 0) onOpenChange(false);
      toast.success(
        t("knowledge.detail.filesUploaded", { count: outcome.accepted }),
        t("knowledge.detail.docProcessing")
      );
    } catch (error) {
      if (!ownsAttempt()) return;
      console.error("Upload failed:", error);
      if (!recovery) setRecoveryState((current) => ({ ...current, owner: authenticatedScopeId, key: recoveryKey, failed: true }));
      if (uploadStarted) {
        const retrySafe = isDefiniteUploadRejection(error);
        setUploadFailures(filesToUpload.map((file, index) => ({
          name: file.name,
          position: index + 1,
          sizeBytes: file.size,
          error: retrySafe
            ? getSourceUploadError(error, {
                fallback: t("knowledge.detail.uploadFailed"),
                requestTooLarge: t("knowledge.create.uploadTooLarge"),
              })
            : t("knowledge.detail.uploadOutcomeUnknown"),
          retrySafe,
        })));
        if (!retrySafe) settleFiles([]);
      }
      toast.error(
        t("knowledge.detail.uploadFailed"),
        uploadStarted && !isDefiniteUploadRejection(error)
          ? t("knowledge.detail.uploadOutcomeUnknown")
          : getSourceUploadError(error, {
              fallback: t("knowledge.detail.uploadFailed"),
              requestTooLarge: t("knowledge.create.uploadTooLarge"),
            })
      );
    } finally {
      unsubscribe();
      if (inFlightUpload.current === attempt) {
        inFlightUpload.current = null;
        if (uploadOwner.current.mounted) onUploadingChange(false);
      }
    }
  }

  async function refreshUploadRecovery() {
    if (!recoveryKey || !isAuthenticatedScopeCurrent() || !uploadOwner.current.mounted || uploadOwner.current.generation !== ownerGeneration
      || useAuthStore.getState().token !== token || sdkAuthorityScope(useAuthStore.getState().user) !== authority) return;
    const currentGeneration = uploadOwner.current.generation;
    reloadRecovery();
    const results = await Promise.allSettled([
      queryClient.invalidateQueries({ queryKey: ["kb-documents", datasetId] }),
      queryClient.invalidateQueries({ queryKey: ["kb-dataset", datasetId] }),
    ]);
    if (isAuthenticatedScopeCurrent() && uploadOwner.current.mounted && uploadOwner.current.generation === currentGeneration
      && results.some((result) => result.status === "rejected")) {
      toast.error(t("knowledge.detail.uploadRecoveryRefreshFailed"));
    }
  }
  function clearUploadRecovery() {
    if (!recoveryKey || !isAuthenticatedScopeCurrent() || inFlightUpload.current || !uploadOwner.current.mounted
      || uploadOwner.current.generation !== ownerGeneration || useAuthStore.getState().token !== token
      || sdkAuthorityScope(useAuthStore.getState().user) !== authority) return;
    try { writeUploadRecovery(recoveryKey, []); reloadRecovery(); }
    catch { setRecoveryState({ owner: authenticatedScopeId, key: recoveryKey, receipts: [], failed: true }); }
  }

  return {
    recoveryReceipts,
    recoveryFailed,
    refreshUploadRecovery,
    clearUploadRecovery,
    uploadFailures,
    clearUploadFailures: () => setUploadFailures([]),
    uploadChunkMode,
    setUploadChunkMode,
    uploadChunkSize,
    setUploadChunkSize,
    uploadChunkOverlap,
    setUploadChunkOverlap,
    uploadMinParagraphLength,
    setUploadMinParagraphLength,
    uploadMergeShortParagraphs,
    setUploadMergeShortParagraphs,
    uploadHeadingLevel,
    setUploadHeadingLevel,
    uploadParentChunkSize,
    setUploadParentChunkSize,
    uploadChildChunkSize,
    setUploadChildChunkSize,
    uploadChildOverlap,
    setUploadChildOverlap,
    uploadSeparator,
    setUploadSeparator,
    uploadKeepSeparator,
    setUploadKeepSeparator,
    uploadRegexPattern,
    setUploadRegexPattern,
    uploadQuestionPrefix,
    setUploadQuestionPrefix,
    uploadAnswerPrefix,
    setUploadAnswerPrefix,
    uploadMetadataEnabled,
    setUploadMetadataEnabled,
    uploadExtractTitle,
    setUploadExtractTitle,
    uploadExtractSummary,
    setUploadExtractSummary,
    uploadExtractKeywords,
    setUploadExtractKeywords,
    uploadExtractEntities,
    setUploadExtractEntities,
    uploadDetectLanguage,
    setUploadDetectLanguage,
    uploadTableEnabled,
    setUploadTableEnabled,
    uploadTableMode,
    setUploadTableMode,
    uploadTableIncludeHeaders,
    setUploadTableIncludeHeaders,
    uploadTableGenerateSummary,
    setUploadTableGenerateSummary,
    uploadEmbeddingModel,
    setUploadEmbeddingModel,
    rerankEnabled,
    setRerankEnabled,
    rerankModel,
    setRerankModel,
    handleConfirmUpload,
  };
}
