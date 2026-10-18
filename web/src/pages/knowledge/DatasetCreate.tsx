/**
 * Knowledge Base Creation Wizard
 *
 * 3-Step wizard following Alibaba Cloud design:
 * 1. Basic Info - Name, description, embedding model
 * 2. Select Data - Upload files / URL
 * 3. Index Settings - Chunking, retrieval config
 */
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";
import { message } from "antd";
import { ArrowLeft, ArrowRight, Check, Loader2 } from "lucide-react";

import { createDataset, createDocumentFromUrl, getDataset, uploadDocument } from "@/api/knowledge";
import { Button } from "@/components/ui/button";
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
import { DatasetCreateBasicStep } from "@/pages/knowledge/create/DatasetCreateBasicStep";
import { DatasetCreateIndexStep } from "@/pages/knowledge/create/DatasetCreateIndexStep";
import { DatasetCreateSourcesStep } from "@/pages/knowledge/create/DatasetCreateSourcesStep";
import { SourceUploadFailureAlert } from "@/pages/knowledge/create/SourceUploadFailureAlert";
import {
  EMBEDDING_MODELS,
  MAX_FILE_SIZE,
  MAX_FILE_SIZE_MB,
  MAX_NAME_LENGTH,
  SUPPORTED_FILE_EXTENSIONS,
  URL_PATTERN,
  getSourceUploadError,
  isDefiniteUploadRejection,
  type KBType,
  type PendingFile,
  type PendingUrl,
  type UseCase,
  type VisibilityType,
} from "@/pages/knowledge/create/datasetCreateModel";
import type { ChunkingMode, Dataset } from "@/types/knowledge";
import { DEFAULT_CHUNKING_CONFIG, DEFAULT_RETRIEVAL_CONFIG } from "@/types/knowledge";
import { useAuthStore } from "@/store/useAuthStore";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";

function createdDatasetDraftKey(userId: string | undefined): string | null {
  return userId ? `kb-create-dataset:${userId}` : null;
}

interface CreateFormDraft {
  version: 1;
  step: number;
  name: string;
  description: string;
  visibility: VisibilityType;
  kbType: KBType;
  useCase: UseCase;
  embeddingModel: string;
  pendingUrls: Array<Pick<PendingUrl, "id" | "url" | "title">>;
  urlInput: string;
  urlTitle: string;
  chunkingMode: ChunkingMode;
  maxChunkSize: number;
  metadataExtract: boolean;
  excelHeaderConcat: boolean;
  multiTurnRewrite: boolean;
  rerankModel: string;
  scoreThreshold: number;
  maxRecall: number;
}

function createFormDraftKey(userId: string | undefined): string | null {
  return userId ? `kb-create-form:${userId}` : null;
}

function readCreateFormDraft(key: string | null): CreateFormDraft | null {
  if (!key) return null;
  try {
    const raw = sessionStorage.getItem(key);
    if (!raw) return null;
    const draft: unknown = JSON.parse(raw);
    if (!draft || typeof draft !== "object" || Array.isArray(draft)) return null;
    const candidate = draft as Partial<CreateFormDraft>;
    return candidate.version === 1 && typeof candidate.name === "string"
      && typeof candidate.description === "string" && Array.isArray(candidate.pendingUrls)
      && typeof candidate.step === "number" && typeof candidate.maxChunkSize === "number"
      && typeof candidate.maxRecall === "number" && typeof candidate.scoreThreshold === "number"
      && typeof candidate.visibility === "string" && typeof candidate.kbType === "string"
      && typeof candidate.useCase === "string" && typeof candidate.embeddingModel === "string"
      && typeof candidate.urlInput === "string" && typeof candidate.urlTitle === "string"
      && typeof candidate.chunkingMode === "string" && typeof candidate.rerankModel === "string"
      && typeof candidate.metadataExtract === "boolean" && typeof candidate.excelHeaderConcat === "boolean"
      && typeof candidate.multiTurnRewrite === "boolean"
      ? candidate as CreateFormDraft : null;
  } catch {
    return null;
  }
}

function writeCreateFormDraft(key: string | null, draft: CreateFormDraft | null) {
  if (!key) return;
  try {
    if (draft) sessionStorage.setItem(key, JSON.stringify(draft));
    else sessionStorage.removeItem(key);
  } catch {
    // The create-request identity has its own required storage check.
  }
}

function readCreatedDatasetDraft(key: string | null): string | null {
  if (!key) return null;
  try {
    const value = sessionStorage.getItem(key);
    return value && value.length <= 255 ? value : null;
  } catch {
    return null;
  }
}

function writeCreatedDatasetDraft(key: string | null, value: string | null): boolean {
  if (!key) return false;
  try {
    if (value) sessionStorage.setItem(key, value);
    else sessionStorage.removeItem(key);
    return true;
  } catch {
    return false;
  }
}

function errorStatus(error: unknown): number | undefined {
  return error && typeof error === "object"
    ? (error as { response?: { status?: number } }).response?.status
    : undefined;
}

export default function DatasetCreatePage() {
  const { id, isCurrent } = useAuthenticatedReadScope();
  return <DatasetCreateWizard key={id} isScopeCurrent={isCurrent} />;
}

function DatasetCreateWizard({ isScopeCurrent }: { isScopeCurrent: () => boolean }) {
  const viewActive = useRef(true);
  useLayoutEffect(() => {
    viewActive.current = true;
    return () => { viewActive.current = false; };
  }, []);
  const isCurrent = useCallback(() => viewActive.current && isScopeCurrent(), [isScopeCurrent]);
  const navigate = useNavigate();
  const { t } = useTranslation();
  const navigateAway = (path: string) => {
    if (!isCurrent()) return;
    // Retire synchronously: navigation/unmount may be scheduled after an HTTP reply.
    // Accepted server work is not rolled back; retain its stable recovery identity.
    viewActive.current = false;
    navigate(path);
  };
  const leaveWizard = () => navigateAway("/knowledge");
  const userId = useAuthStore((state) => state.user?.user_id);
  const createdDraftKey = createdDatasetDraftKey(userId);
  const formDraftKey = createFormDraftKey(userId);
  const [restoredFormDraftKey, setRestoredFormDraftKey] = useState<string | null>(null);

  const [step, setStep] = useState(1);
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [outcomeUnknown, setOutcomeUnknown] = useState(false);
  const [requestDatasetId, setRequestDatasetId] = useState<string | null>(
    () => readCreatedDatasetDraft(createdDraftKey)
  );
  const [createdDatasetId, setCreatedDatasetId] = useState<string | null>(null);
  const [recoveredDataset, setRecoveredDataset] = useState<Dataset | null>(null);
  const [checkingDraft, setCheckingDraft] = useState(Boolean(requestDatasetId));
  const [draftLookupFailure, setDraftLookupFailure] = useState<"access" | "unavailable" | null>(null);
  const [discardDraftOpen, setDiscardDraftOpen] = useState(false);
  const submitInFlight = useRef(false);
  const [nameError, setNameError] = useState<string | null>(null);

  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [visibility, setVisibility] = useState<VisibilityType>("private");
  const [kbType, setKbType] = useState<KBType>("document");
  const [useCase, setUseCase] = useState<UseCase>("basic_qa");
  const [embeddingModel, setEmbeddingModel] = useState("dashscope:text-embedding-v4");

  const [pendingFiles, setPendingFiles] = useState<PendingFile[]>([]);
  const [pendingUrls, setPendingUrls] = useState<PendingUrl[]>([]);
  const [urlInput, setUrlInput] = useState("");
  const [urlTitle, setUrlTitle] = useState("");

  const [chunkingMode, setChunkingMode] = useState<ChunkingMode>(DEFAULT_CHUNKING_CONFIG.mode);
  const [maxChunkSize, setMaxChunkSize] = useState(DEFAULT_CHUNKING_CONFIG.chunk_size);
  const [metadataExtract, setMetadataExtract] = useState(false);
  const [excelHeaderConcat, setExcelHeaderConcat] = useState(false);
  const [multiTurnRewrite, setMultiTurnRewrite] = useState(true);
  const [rerankModel, setRerankModel] = useState("default");
  const [scoreThreshold, setScoreThreshold] = useState(DEFAULT_RETRIEVAL_CONFIG.score_threshold);
  const [maxRecall, setMaxRecall] = useState(DEFAULT_RETRIEVAL_CONFIG.top_k);

  useEffect(() => {
    if (!formDraftKey || !isCurrent()) return;
    const draft = readCreateFormDraft(formDraftKey);
    if (draft) {
      setStep(Math.min(3, Math.max(1, draft.step)));
      setName(draft.name);
      setDescription(draft.description);
      setVisibility(draft.visibility);
      setKbType(draft.kbType);
      setUseCase(draft.useCase);
      setEmbeddingModel(draft.embeddingModel);
      setPendingUrls(draft.pendingUrls.map((url) => ({ ...url, status: "pending" })));
      setUrlInput(draft.urlInput);
      setUrlTitle(draft.urlTitle);
      setChunkingMode(draft.chunkingMode);
      setMaxChunkSize(draft.maxChunkSize);
      setMetadataExtract(draft.metadataExtract);
      setExcelHeaderConcat(draft.excelHeaderConcat);
      setMultiTurnRewrite(draft.multiTurnRewrite);
      setRerankModel(draft.rerankModel);
      setScoreThreshold(draft.scoreThreshold);
      setMaxRecall(draft.maxRecall);
    }
    setRestoredFormDraftKey(formDraftKey);
  }, [formDraftKey, isCurrent]);

  useEffect(() => {
    if (restoredFormDraftKey !== formDraftKey || !formDraftKey || !isCurrent()) return;
    writeCreateFormDraft(formDraftKey, {
      version: 1,
      step, name, description, visibility, kbType, useCase, embeddingModel,
      pendingUrls: pendingUrls.map(({ id, url, title }) => ({ id, url, title })),
      urlInput, urlTitle, chunkingMode, maxChunkSize, metadataExtract,
      excelHeaderConcat, multiTurnRewrite, rerankModel, scoreThreshold, maxRecall,
    });
  }, [restoredFormDraftKey, formDraftKey, step, name, description, visibility, kbType, useCase,
    embeddingModel, pendingUrls, urlInput, urlTitle, chunkingMode, maxChunkSize,
    metadataExtract, excelHeaderConcat, multiTurnRewrite, rerankModel, scoreThreshold, maxRecall, isCurrent]);

  useEffect(() => {
    if (!isCurrent()) return;
    const id = readCreatedDatasetDraft(createdDraftKey);
    setRequestDatasetId(id);
    setCreatedDatasetId(null);
    setRecoveredDataset(null);
    setDraftLookupFailure(null);
    setCheckingDraft(Boolean(id));
    if (!id) return;
    let cancelled = false;
    getDataset(id).then((dataset) => {
      if (cancelled || !isCurrent()) return;
      if (!["owner", "editor"].includes(dataset.my_permission || "")) {
        setDraftLookupFailure("access");
        return;
      }
      setRecoveredDataset(dataset);
      setCreatedDatasetId(id);
    }).catch((lookupError: unknown) => {
      if (!cancelled && isCurrent() && errorStatus(lookupError) !== 404) {
        setDraftLookupFailure(errorStatus(lookupError) === 403 ? "access" : "unavailable");
      }
    }).finally(() => {
      if (!cancelled && isCurrent()) setCheckingDraft(false);
    });
    return () => { cancelled = true; };
  }, [createdDraftKey, isCurrent]);

  const handleChunkingModeSelect = useCallback((mode: ChunkingMode) => {
    setChunkingMode(mode);
  }, []);

  const handleFilesSelect = useCallback(
    (files: FileList | null) => {
      if (!files) return;
      const newFiles: PendingFile[] = [];
      const errors: string[] = [];

      Array.from(files).forEach((file) => {
        if (!SUPPORTED_FILE_EXTENSIONS.test(file.name)) {
          errors.push(t("knowledge.create.validation.unsupportedFile", { name: file.name }));
          return;
        }
        const maxSize = MAX_FILE_SIZE;
        const maxSizeLabel = `${MAX_FILE_SIZE_MB}MB`;

        if (file.size > maxSize) {
          errors.push(
            t("knowledge.create.validation.fileTooLarge", {
              name: file.name,
              limit: maxSizeLabel,
            })
          );
          return;
        }

        if (pendingFiles.some((pendingFile) => pendingFile.name === file.name && pendingFile.size === file.size)) {
          errors.push(t("knowledge.create.validation.duplicateFile", { name: file.name }));
          return;
        }

        newFiles.push({
          id: `file_${Date.now()}_${Math.random().toString(36).slice(2)}`,
          file,
          name: file.name,
          size: file.size,
          status: "pending",
        });
      });

      if (errors.length > 0) {
        errors.forEach((validationError) => message.warning(validationError));
      }

      if (newFiles.length > 0) {
        setPendingFiles((previous) => [...previous, ...newFiles]);
      }
    },
    [pendingFiles, t]
  );

  const handleRemoveFile = useCallback((id: string) => {
    setPendingFiles((previous) => previous.filter((file) => file.id !== id));
  }, []);

  const handleAddUrl = useCallback(() => {
    const trimmedUrl = urlInput.trim();
    if (!trimmedUrl) return;

    if (!URL_PATTERN.test(trimmedUrl)) {
      message.error(t("knowledge.create.validation.invalidUrl"));
      return;
    }

    if (pendingUrls.some((pendingUrl) => pendingUrl.url === trimmedUrl)) {
      message.warning(t("knowledge.create.validation.duplicateUrl"));
      return;
    }

    const newUrl: PendingUrl = {
      id: `url_${Date.now()}_${Math.random().toString(36).slice(2)}`,
      url: trimmedUrl,
      title: urlTitle.trim() || trimmedUrl,
      status: "pending",
    };
    setPendingUrls((previous) => [...previous, newUrl]);
    setUrlInput("");
    setUrlTitle("");
  }, [urlInput, urlTitle, pendingUrls, t]);

  const handleRemoveUrl = useCallback((id: string) => {
    setPendingUrls((previous) => previous.filter((url) => url.id !== id));
  }, []);

  const handleSubmit = async () => {
    if (!isCurrent() || submitInFlight.current || checkingDraft || draftLookupFailure || recoveredDataset) return;
    const [provider, model] = embeddingModel.split(":");
    const selectedEmbeddingModel = EMBEDDING_MODELS.find(
      (candidate) => candidate.provider === provider && candidate.model === model
    );
    if (!name.trim() || name.trim().length > MAX_NAME_LENGTH || !selectedEmbeddingModel) {
      setStep(1);
      setError(t("knowledge.create.invalidDraftConfig"));
      return;
    }
    if (!Number.isFinite(maxChunkSize) || maxChunkSize < 50 || maxChunkSize > 6000
      || !Number.isFinite(scoreThreshold) || scoreThreshold < 0 || scoreThreshold > 1
      || !Number.isInteger(maxRecall) || maxRecall < 1 || maxRecall > 20) {
      setStep(3);
      setError(t("knowledge.create.invalidDraftConfig"));
      return;
    }
    submitInFlight.current = true;
    setIsSubmitting(true);
    setError(null);
    setOutcomeUnknown(false);

    try {
      const rerankProvider = rerankModel.startsWith("bge-") ? "bge" : "dashscope";

      let datasetId = createdDatasetId;
      if (!datasetId) {
        let stableId = requestDatasetId;
        if (!stableId) {
          stableId = `kb_${crypto.randomUUID().replaceAll("-", "")}`;
          if (!writeCreatedDatasetDraft(createdDraftKey, stableId)) {
            setError(t("knowledge.create.draftStorageUnavailable"));
            return;
          }
          setRequestDatasetId(stableId);
        } else {
          try {
            const existing = await getDataset(stableId);
            if (!isCurrent()) return;
            if (
              !["owner", "editor"].includes(existing.my_permission || "") ||
              existing.name !== name.trim()
            ) {
              setDraftLookupFailure("access");
              setError(t("knowledge.create.draftIdentityChanged"));
              return;
            }
            setRecoveredDataset(existing);
            setCreatedDatasetId(stableId);
            setError(t("knowledge.create.existingDraftFound"));
            return;
          } catch (lookupError) {
            if (!isCurrent()) return;
            if (errorStatus(lookupError) !== 404) {
              const failure = errorStatus(lookupError) === 403 ? "access" : "unavailable";
              setDraftLookupFailure(failure);
              setError(t(failure === "access" ? "knowledge.create.draftAccessChanged" : "knowledge.create.draftLookupUnavailable"));
              return;
            }
          }
        }
        if (!isCurrent()) return;
        const dataset = await createDataset({
          dataset_id: stableId,
          name: name.trim(),
          description: description.trim(),
          visibility,
          kb_type: kbType,
          use_case: useCase,
          embedding_provider: provider,
          embedding_model: model,
          embedding_dimension: selectedEmbeddingModel?.dimension || 1024,
          index_config: {
            chunking: {
              mode: chunkingMode,
              chunk_size: maxChunkSize,
              chunk_overlap: Math.min(DEFAULT_CHUNKING_CONFIG.chunk_overlap, Math.floor(maxChunkSize * 0.1)),
              extract_metadata: metadataExtract,
              remove_extra_spaces: DEFAULT_CHUNKING_CONFIG.remove_extra_spaces,
            },
            retrieval: {
              mode: DEFAULT_RETRIEVAL_CONFIG.mode,
              top_k: maxRecall,
              score_threshold: scoreThreshold,
              // Creation stored retrieval wholesale; without fusion the
              // dataset would fall back to schema defaults for hybrid mode.
              fusion: { ...DEFAULT_RETRIEVAL_CONFIG.fusion },
              rerank: {
                enabled: rerankModel !== "default",
                provider: rerankProvider,
                model: rerankModel === "default" ? DEFAULT_RETRIEVAL_CONFIG.rerank.model : rerankModel,
              },
            },
          },
        });
        if (!isCurrent()) return;
        if (dataset.dataset_id !== stableId) {
          throw new Error("dataset create response did not match its request identity");
        }
        datasetId = dataset.dataset_id;
        setCreatedDatasetId(datasetId);
      }

      let uncertainUploads = pendingFiles.filter((file) => file.status === "error" && file.retrySafe === false).length
        + pendingUrls.filter((url) => url.status === "error" && url.retrySafe === false).length;
      let failedUploads = uncertainUploads;
      for (const pendingFile of pendingFiles.filter((file) => file.status !== "done" && file.retrySafe !== false)) {
        if (!isCurrent()) return;
        setPendingFiles((previous) =>
          previous.map((file) =>
            file.id === pendingFile.id ? { ...file, status: "uploading" } : file
          )
        );
        try {
          await uploadDocument(datasetId, pendingFile.file);
          if (!isCurrent()) return;
          setPendingFiles((previous) =>
            previous.map((file) =>
              file.id === pendingFile.id ? { ...file, status: "done" } : file
            )
          );
        } catch (uploadError) {
          if (!isCurrent()) return;
          failedUploads += 1;
          const retrySafe = isDefiniteUploadRejection(uploadError);
          if (!retrySafe) uncertainUploads += 1;
          setPendingFiles((previous) =>
            previous.map((file) =>
              file.id === pendingFile.id
                ? {
                    ...file,
                    status: "error",
                    retrySafe,
                    error: retrySafe
                      ? getSourceUploadError(uploadError, {
                          fallback: t("knowledge.create.uploadFailed"),
                          requestTooLarge: t("knowledge.create.uploadTooLarge"),
                        })
                      : t("knowledge.detail.uploadOutcomeUnknown"),
                  }
                : file
            )
          );
        }
      }

      for (const pendingUrl of pendingUrls.filter((url) => url.status !== "done" && url.retrySafe !== false)) {
        if (!isCurrent()) return;
        setPendingUrls((previous) =>
          previous.map((url) =>
            url.id === pendingUrl.id ? { ...url, status: "uploading" } : url
          )
        );
        try {
          await createDocumentFromUrl(datasetId, {
            url: pendingUrl.url,
            title: pendingUrl.title,
          });
          if (!isCurrent()) return;
          setPendingUrls((previous) =>
            previous.map((url) =>
              url.id === pendingUrl.id ? { ...url, status: "done" } : url
            )
          );
        } catch (fetchError) {
          if (!isCurrent()) return;
          failedUploads += 1;
          const retrySafe = isDefiniteUploadRejection(fetchError);
          if (!retrySafe) uncertainUploads += 1;
          setPendingUrls((previous) =>
            previous.map((url) =>
              url.id === pendingUrl.id
                ? {
                    ...url,
                    status: "error",
                    retrySafe,
                    error: retrySafe
                      ? getSourceUploadError(fetchError, {
                          fallback: t("knowledge.create.fetchFailed"),
                          requestTooLarge: t("knowledge.create.uploadTooLarge"),
                        })
                      : t("knowledge.detail.uploadOutcomeUnknown"),
                  }
                : url
            )
          );
        }
      }

      if (!isCurrent()) return;
      if (failedUploads > 0) {
        setStep(2);
        setOutcomeUnknown(uncertainUploads > 0);
        setError(
          uncertainUploads > 0
            ? t("knowledge.create.partialUploadUnknown", { count: uncertainUploads })
            : t("knowledge.create.partialUploadFailed", { count: failedUploads })
        );
        return;
      }

      viewActive.current = false;
      writeCreatedDatasetDraft(createdDraftKey, null);
      writeCreateFormDraft(formDraftKey, null);
      navigate(`/knowledge/${datasetId}`);
    } catch (submitError) {
      if (!isCurrent()) return;
      console.error("Failed to create dataset:", submitError);
      const unknown = Boolean(requestDatasetId || readCreatedDatasetDraft(createdDraftKey));
      setOutcomeUnknown(unknown);
      setError(unknown
        ? t("knowledge.create.createOutcomeUnknown")
        : t("knowledge.create.createError"));
    } finally {
      if (isCurrent()) {
        setIsSubmitting(false);
        submitInFlight.current = false;
      }
    }
  };

  const handleNextStep = () => {
    if (step === 1) {
      const trimmedName = name.trim();
      if (!trimmedName) {
        setNameError(t("knowledge.create.nameRequired"));
        message.error(t("knowledge.create.nameRequired"));
        return;
      }
      if (trimmedName.length > MAX_NAME_LENGTH) {
        const validationError = t("knowledge.create.nameTooLong", {
          max: MAX_NAME_LENGTH,
        });
        setNameError(validationError);
        message.error(validationError);
        return;
      }
      setNameError(null);
      setStep(2);
    } else if (step === 2) {
      setStep(3);
    }
  };

  const wizardSteps = [
    { num: 1, label: t("knowledge.create.step1") },
    { num: 2, label: t("knowledge.create.step2") },
    { num: 3, label: t("knowledge.create.step3") },
  ];

  return (
    <div className="min-h-full bg-background">
      <div className="bg-card border-b px-4 py-4 sm:px-6">
        <div className="max-w-4xl mx-auto flex items-center gap-4">
          <button
            type="button"
            onClick={leaveWizard}
            aria-label={t("common.back", "Back")}
            className="text-muted-foreground hover:text-foreground/80 transition"
          >
            <ArrowLeft className="h-5 w-5" />
          </button>
          <div className="text-muted-foreground/70">/</div>
          <h1 className="text-lg font-semibold text-foreground">
            {t("knowledge.create.title")}
          </h1>
        </div>
      </div>

      <div className="bg-card border-b">
        <div className="max-w-4xl mx-auto px-4 py-4 sm:px-6 sm:py-6">
          <div className="flex items-center justify-between gap-3 sm:hidden" aria-live="polite">
            <span className="text-xs font-medium uppercase tracking-[0.12em] text-muted-foreground">
              {t("knowledge.create.stepProgress", "Step {current} of {total}", {
                current: step,
                total: wizardSteps.length,
              })}
            </span>
            <span className="text-sm font-semibold text-foreground">
              {wizardSteps[step - 1]?.label}
            </span>
          </div>
          <div className="hidden items-center justify-center gap-4 sm:flex">
            {wizardSteps.map((wizardStep, index) => (
              <div key={wizardStep.num} className="flex items-center">
                <div className="flex items-center gap-2">
                  <div
                    className={`w-7 h-7 rounded-full flex items-center justify-center text-sm font-medium transition-[color,background-color,box-shadow] ${
                      step > wizardStep.num
                        ? "bg-primary text-white"
                        : step === wizardStep.num
                          ? "bg-primary text-white ring-4 ring-primary/10"
                          : "bg-border text-muted-foreground"
                    }`}
                  >
                    {step > wizardStep.num ? (
                      <Check className="h-4 w-4" />
                    ) : (
                      wizardStep.num
                    )}
                  </div>
                  <span
                    className={`text-sm font-medium ${
                      step >= wizardStep.num ? "text-foreground" : "text-muted-foreground/70"
                    }`}
                  >
                    {wizardStep.label}
                  </span>
                </div>
                {index < 2 && (
                  <div
                    className={`w-24 h-0.5 mx-4 transition-colors ${
                      step > wizardStep.num ? "bg-primary" : "bg-border"
                    }`}
                  />
                )}
              </div>
            ))}
          </div>
        </div>
      </div>

      <div className="max-w-4xl mx-auto px-4 py-6 sm:px-6 sm:py-8">
        {checkingDraft && (
          <p role="status" className="mb-4 text-sm text-muted-foreground">
            {t("knowledge.create.checkingExistingDraft")}
          </p>
        )}
        {restoredFormDraftKey === formDraftKey && (name || description || pendingUrls.length > 0) && (
          <p role="status" className="mb-4 text-xs text-muted-foreground">
            {t("knowledge.create.formDraftSaved")}
          </p>
        )}
        {restoredFormDraftKey === formDraftKey && step > 1 && pendingFiles.length === 0 && (
          <p className="mb-4 text-xs text-muted-foreground">
            {t("knowledge.create.filesNotRestored")}
          </p>
        )}
        {recoveredDataset && (
          <div role="alert" className="mb-4 rounded-md border border-primary/30 bg-primary/5 p-4 text-sm">
            <p className="font-medium">
              {t("knowledge.create.existingDraftFoundNamed", { name: recoveredDataset.name })}
            </p>
            <p className="mt-1 text-muted-foreground">
              {t("knowledge.create.existingDraftNextStep")}
            </p>
            <Button className="mt-3" onClick={() => navigateAway(`/knowledge/${recoveredDataset.dataset_id}`)}>
              {t("knowledge.create.openExistingDraft")}
            </Button>
            <Button variant="outline" className="ml-2 mt-3" onClick={() => setDiscardDraftOpen(true)}>
              {t("knowledge.create.startAnotherDataset")}
            </Button>
          </div>
        )}
        {draftLookupFailure && (
          <div role="alert" className="mb-4 rounded-md border border-destructive/40 bg-destructive/5 p-4 text-sm">
            <p>{t(draftLookupFailure === "access" ? "knowledge.create.draftAccessChanged" : "knowledge.create.draftLookupUnavailable")}</p>
            <Button variant="outline" className="mt-3" onClick={() => window.location.reload()}>
              {t("knowledge.create.checkAgain")}
            </Button>
            <Button variant="outline" className="ml-2 mt-3" onClick={() => setDiscardDraftOpen(true)}>
              {t("knowledge.create.startAnotherDataset")}
            </Button>
          </div>
        )}
        {requestDatasetId && !createdDatasetId && !checkingDraft && !draftLookupFailure && !isSubmitting && (
          <Button variant="outline" className="mb-4" onClick={() => setDiscardDraftOpen(true)}>
            {t("knowledge.create.startAnotherDataset")}
          </Button>
        )}
        {isSubmitting && (
          <p role="status" className="mb-4 text-sm text-muted-foreground">
            {t("knowledge.create.leaveWhileSubmitting")}
          </p>
        )}
        <SourceUploadFailureAlert
          error={error}
          datasetCreated={Boolean(createdDatasetId)}
          outcomeUnknown={outcomeUnknown}
          files={pendingFiles}
          urls={pendingUrls}
        />

        {step === 1 && (
          <DatasetCreateBasicStep
            name={name}
            nameError={nameError}
            description={description}
            visibility={visibility}
            kbType={kbType}
            useCase={useCase}
            embeddingModel={embeddingModel}
            onNameChange={(value) => {
              setName(value);
              if (nameError) setNameError(null);
            }}
            onDescriptionChange={setDescription}
            onVisibilityChange={setVisibility}
            onKbTypeChange={setKbType}
            onUseCaseChange={setUseCase}
            onEmbeddingModelChange={setEmbeddingModel}
          />
        )}

        {step === 2 && (
          <DatasetCreateSourcesStep
            pendingFiles={pendingFiles}
            pendingUrls={pendingUrls}
            urlInput={urlInput}
            urlTitle={urlTitle}
            onFilesSelect={handleFilesSelect}
            onRemoveFile={handleRemoveFile}
            onUrlInputChange={setUrlInput}
            onUrlTitleChange={setUrlTitle}
            onAddUrl={handleAddUrl}
            onRemoveUrl={handleRemoveUrl}
          />
        )}

        {step === 3 && (
          <DatasetCreateIndexStep
            chunkingMode={chunkingMode}
            maxChunkSize={maxChunkSize}
            metadataExtract={metadataExtract}
            excelHeaderConcat={excelHeaderConcat}
            multiTurnRewrite={multiTurnRewrite}
            rerankModel={rerankModel}
            scoreThreshold={scoreThreshold}
            maxRecall={maxRecall}
            onChunkingModeChange={handleChunkingModeSelect}
            onMaxChunkSizeChange={setMaxChunkSize}
            onMetadataExtractChange={setMetadataExtract}
            onExcelHeaderConcatChange={setExcelHeaderConcat}
            onMultiTurnRewriteChange={setMultiTurnRewrite}
            onRerankModelChange={setRerankModel}
            onScoreThresholdChange={setScoreThreshold}
            onMaxRecallChange={setMaxRecall}
          />
        )}

        <div className="sticky bottom-0 z-10 -mx-4 mt-8 flex items-center justify-between border-t bg-background/95 px-4 py-4 backdrop-blur-sm sm:static sm:mx-0 sm:bg-transparent sm:px-0 sm:pt-6 sm:backdrop-blur-none">
          <div>
            {step > 1 && !createdDatasetId && (
              <Button variant="outline" onClick={() => setStep((currentStep) => currentStep - 1)}>
                {t("knowledge.create.previous")}
              </Button>
            )}
          </div>
          <div className="flex items-center gap-3">
            <Button variant="outline" onClick={leaveWizard}>
              {t("knowledge.create.cancel")}
            </Button>
            {step < 3 && createdDatasetId ? (
              <Button variant="primary" onClick={handleSubmit} disabled={isSubmitting || checkingDraft || Boolean(draftLookupFailure) || Boolean(recoveredDataset)}>
                {isSubmitting && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
                {t("knowledge.create.retryFailedSources")}
              </Button>
            ) : step < 3 ? (
              <Button variant="primary" onClick={handleNextStep}>
                {t("knowledge.create.next")}
                <ArrowRight className="ml-2 h-4 w-4" />
              </Button>
            ) : (
              <Button variant="primary" onClick={handleSubmit} disabled={isSubmitting || checkingDraft || Boolean(draftLookupFailure) || Boolean(recoveredDataset)}>
                {isSubmitting ? (
                  <>
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                    {t("knowledge.create.creating")}
                  </>
                ) : (
                  t("knowledge.create.confirm")
                )}
              </Button>
            )}
          </div>
        </div>
      </div>
      <AlertDialog open={discardDraftOpen} onOpenChange={setDiscardDraftOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("knowledge.create.discardDraftTitle")}</AlertDialogTitle>
            <AlertDialogDescription>{t("knowledge.create.discardDraftDescription")}</AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
            <AlertDialogAction onClick={() => {
              if (!isCurrent()) return;
              if (!writeCreatedDatasetDraft(createdDraftKey, null)) {
                setError(t("knowledge.create.draftStorageUnavailable"));
                return;
              }
              writeCreateFormDraft(formDraftKey, null);
              viewActive.current = false;
              window.location.reload();
            }}>
              {t("knowledge.create.startAnotherDataset")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
