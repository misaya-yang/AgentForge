import { isAxiosError } from "axios";
import {
  Activity,
  AlertTriangle,
  CheckCircle2,
  Database,
  GitBranch,
  Loader2,
  Play,
  RefreshCw,
  RotateCcw,
  ShieldAlert,
  Square,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  abortEmbeddingMigration,
  backfillEmbeddingMigration,
  cutoverEmbeddingMigration,
  describeEmbeddingMigration,
  gateEmbeddingMigration,
  getEmbeddingMigrationActionJob,
  rollbackEmbeddingMigration,
  startEmbeddingMigration,
  verifyEmbeddingMigration,
  type EmbeddingCollectionBinding,
  type EmbeddingCollectionHealthReceipt,
  type EmbeddingMigrationDescription,
  type EmbeddingMigrationActionJob,
  type EmbeddingMigrationJob,
  type StartEmbeddingMigrationRequest,
} from "@/api/knowledge";
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
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
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
import { Textarea } from "@/components/ui/textarea";
import { toast } from "@/hooks/use-toast";
import { DATASET_EMBEDDING_MODELS } from "@/pages/knowledge/detail/useDatasetUploadController";

import { EmbeddingActionJobCard } from "./EmbeddingActionJobCard";
import {
  clearEmbeddingActionJobPointer,
  embeddingActionJobPollDelay,
  isEmbeddingActionName,
  mergeEmbeddingActionJob,
  persistEmbeddingActionJobPointer,
  readEmbeddingActionJobPointer,
  selectServerEmbeddingActionJob,
  shouldPollEmbeddingActionJob,
} from "./embeddingMigrationJobs";

import {
  getEmbeddingMigrationControls,
  getEmbeddingMigrationProgress,
  selectEmbeddingMigrationJob,
  shouldPollEmbeddingMigration,
  type EmbeddingMigrationActionName,
} from "./embeddingMigrationState";

interface EmbeddingMigrationPanelProps {
  datasetId: string;
  active: boolean;
  onMigrationChange?: () => void;
}

interface OperatorError {
  status?: number;
  detail: string;
}

type ConfirmedAction = "cutover" | "rollback" | "abort";
type BusyAction = EmbeddingMigrationActionName | "start" | "refresh";

function parseOperatorError(error: unknown): OperatorError {
  if (isAxiosError(error)) {
    const responseData = error.response?.data as
      | { detail?: unknown; message?: unknown }
      | string
      | undefined;
    let detail = error.message;
    if (typeof responseData === "string" && responseData.trim()) {
      detail = responseData;
    } else if (responseData && typeof responseData === "object") {
      if (typeof responseData.detail === "string") detail = responseData.detail;
      else if (typeof responseData.message === "string") detail = responseData.message;
    }
    return { status: error.response?.status, detail };
  }
  return { detail: error instanceof Error ? error.message : String(error) };
}

function browserLocalStorage(): Storage | null {
  try {
    return typeof window === "undefined" ? null : window.localStorage;
  } catch {
    return null;
  }
}

function readString(record: Record<string, unknown> | null | undefined, key: string): string {
  const value = record?.[key];
  return typeof value === "string" ? value : "";
}

function readNumber(
  record: Record<string, unknown> | null | undefined,
  key: string
): number | null {
  const value = record?.[key];
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function formatTimestamp(value: string | null | undefined, locale: string): string {
  if (!value) return "—";
  const timestamp = Date.parse(value);
  if (!Number.isFinite(timestamp)) return value;
  return new Intl.DateTimeFormat(locale, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(timestamp);
}

function statusBadgeClass(state: string): string {
  if (["ready", "completed", "healthy"].includes(state)) {
    return "border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300";
  }
  if (["failed", "gate_failed", "drifted"].includes(state)) {
    return "border-destructive/30 bg-destructive/10 text-destructive";
  }
  if (["shadow_build", "backfilling", "verified", "gating"].includes(state)) {
    return "border-primary/30 bg-primary/10 text-primary";
  }
  return "border-border bg-muted/60 text-muted-foreground";
}

function operatorErrorTitleKey(status: number | undefined): string {
  switch (status) {
    case 409:
      return "conflict";
    case 404:
      return "notFound";
    case 503:
      return "unavailable";
    default:
      return "generic";
  }
}

function actionButtonVariant(
  action: EmbeddingMigrationActionName
): "destructive" | "primary" | "outline" {
  if (action === "rollback" || action === "abort") return "destructive";
  if (action === "cutover") return "primary";
  return "outline";
}

async function enqueueDurableEmbeddingAction(
  datasetId: string,
  migrationId: string,
  action: "backfill" | "verify" | "gate"
): Promise<EmbeddingMigrationActionJob> {
  switch (action) {
    case "backfill":
      return backfillEmbeddingMigration(datasetId, migrationId);
    case "verify":
      return verifyEmbeddingMigration(datasetId, migrationId);
    case "gate":
      return gateEmbeddingMigration(datasetId, migrationId);
  }
}

function MigrationActionIcon({
  action,
  busy,
}: {
  action: EmbeddingMigrationActionName;
  busy: boolean;
}) {
  const className = "mr-1.5 h-3.5 w-3.5";
  if (busy) return <Loader2 className={`${className} animate-spin`} />;
  switch (action) {
    case "backfill":
      return <Play className={className} />;
    case "verify":
    case "gate":
    case "cutover":
      return <CheckCircle2 className={className} />;
    case "rollback":
      return <RotateCcw className={className} />;
    case "abort":
      return <Square className={className} />;
  }
}

function BindingSummary({
  binding,
  emptyLabel,
}: {
  binding: EmbeddingCollectionBinding | null | undefined;
  emptyLabel: string;
}) {
  const { t } = useTranslation();
  if (!binding) return <p className="text-sm text-muted-foreground">{emptyLabel}</p>;
  return (
    <div className="space-y-1.5">
      <p className="truncate font-mono text-xs text-foreground" title={binding.collection_name}>
        {binding.collection_name}
      </p>
      <p className="text-xs text-muted-foreground">
        {binding.embedding_provider} · {binding.embedding_model}
        {binding.embedding_model_version ? ` @ ${binding.embedding_model_version}` : ""}
      </p>
      <div className="flex flex-wrap items-center gap-2">
        <Badge variant="outline" className={statusBadgeClass(binding.state)}>
          {t(`knowledge.detail.embeddingMigration.bindingStates.${binding.state}`)}
        </Badge>
        <span className="text-xs text-muted-foreground">
          {t("knowledge.detail.embeddingMigration.dimensions", {
            count: binding.embedding_dimension,
          })}
        </span>
      </div>
    </div>
  );
}

function TargetGenerationSummary({
  binding,
  collectionName,
  migrationState,
}: {
  binding: EmbeddingCollectionBinding | null;
  collectionName: string;
  migrationState: string | null;
}) {
  const { t } = useTranslation();
  if (binding) {
    return (
      <BindingSummary
        binding={binding}
        emptyLabel={t("knowledge.detail.embeddingMigration.noShadow")}
      />
    );
  }
  if (collectionName) {
    return (
      <div className="space-y-1.5">
        <p className="truncate font-mono text-xs text-foreground" title={collectionName}>
          {collectionName}
        </p>
        <Badge variant="outline" className={statusBadgeClass(migrationState ?? "unknown")}>
          {migrationState
            ? t(`knowledge.detail.embeddingMigration.states.${migrationState}`)
            : "—"}
        </Badge>
      </div>
    );
  }
  return (
    <p className="text-sm text-muted-foreground">
      {t("knowledge.detail.embeddingMigration.noShadow")}
    </p>
  );
}

function HealthEvidence({
  title,
  evidence,
}: {
  title: string;
  evidence: Record<string, unknown> | null | undefined;
}) {
  const { t } = useTranslation();
  const pointCount = readNumber(evidence, "point_count");
  const pointDigest = readString(evidence, "point_ids_sha256");
  const textDigest = readString(evidence, "source_text_sha256");
  return (
    <div className="rounded-lg border border-border/50 bg-background/60 p-2.5">
      <p className="text-xs font-medium text-foreground">{title}</p>
      {evidence ? (
        <div className="mt-1 space-y-1 text-[11px] text-muted-foreground">
          <p>{t("knowledge.detail.embeddingMigration.pointCount", { count: pointCount ?? "?" })}</p>
          {pointDigest ? (
            <p className="truncate font-mono" title={pointDigest}>
              points sha256: {pointDigest}
            </p>
          ) : null}
          {textDigest ? (
            <p className="truncate font-mono" title={textDigest}>
              source sha256: {textDigest}
            </p>
          ) : null}
        </div>
      ) : (
        <p className="mt-1 text-[11px] text-muted-foreground">
          {t("knowledge.detail.embeddingMigration.noEvidence")}
        </p>
      )}
    </div>
  );
}

function CollectionHealthReceipt({
  receipt,
  verifiedPoints,
}: {
  receipt: EmbeddingCollectionHealthReceipt | null | undefined;
  verifiedPoints?: number | null;
}) {
  const { t } = useTranslation();
  const state = receipt?.checked_live ? receipt.status : "unknown";
  return (
    <div
      className="rounded-xl border border-border/60 bg-muted/20 p-3"
      data-testid="embedding-collection-health"
    >
      <div className="flex items-center justify-between gap-2">
        <p className="truncate font-mono text-xs text-foreground" title={receipt?.collection_name ?? ""}>
          {receipt?.collection_name || "—"}
        </p>
        <Badge variant="outline" className={statusBadgeClass(state)}>
          {t(`knowledge.detail.embeddingMigration.healthStates.${state}`)}
        </Badge>
      </div>
      {receipt ? (
        <div className="mt-2 space-y-2 text-xs text-muted-foreground">
          <div className="flex flex-wrap gap-x-4 gap-y-1">
            <span>
              {receipt.checked_live
                ? t("knowledge.detail.embeddingMigration.checkedLive")
                : t("knowledge.detail.embeddingMigration.notCheckedLive")}
            </span>
            <span>
              {t("knowledge.detail.embeddingMigration.pendingChunks", {
                count: receipt.pending_chunks ?? "?",
              })}
            </span>
          </div>
          <div className="grid gap-2 sm:grid-cols-2">
            <HealthEvidence
              title={t("knowledge.detail.embeddingMigration.authorityEvidence")}
              evidence={receipt.authority}
            />
            <HealthEvidence
              title={t("knowledge.detail.embeddingMigration.targetEvidence")}
              evidence={receipt.target_scope}
            />
            <HealthEvidence
              title={t("knowledge.detail.embeddingMigration.verifiedAuthorityEvidence")}
              evidence={receipt.verified_authority}
            />
            <HealthEvidence
              title={t("knowledge.detail.embeddingMigration.verifiedTargetEvidence")}
              evidence={receipt.verified_target_scope}
            />
          </div>
          {receipt.reason ? <p className="text-foreground">{receipt.reason}</p> : null}
          {receipt.gate_report ? (
            <p>
              {t("knowledge.detail.embeddingMigration.gateReceipt", {
                value: String(receipt.gate_report.passed ?? "?"),
              })}
            </p>
          ) : null}
        </div>
      ) : (
        <div className="mt-2 space-y-1 text-xs text-muted-foreground">
          <p>{t("knowledge.detail.embeddingMigration.healthNotProbed")}</p>
          {verifiedPoints !== null && verifiedPoints !== undefined ? (
            <p>
              {t("knowledge.detail.embeddingMigration.ledgerVerifiedPoints", {
                count: verifiedPoints,
              })}
            </p>
          ) : null}
        </div>
      )}
    </div>
  );
}

interface StartWizardProps {
  open: boolean;
  serving: EmbeddingCollectionBinding | null;
  busy: boolean;
  onOpenChange: (open: boolean) => void;
  onSubmit: (request: StartEmbeddingMigrationRequest) => Promise<void>;
}

function StartMigrationWizard({
  open,
  serving,
  busy,
  onOpenChange,
  onSubmit,
}: StartWizardProps) {
  const { t } = useTranslation();
  const [step, setStep] = useState<1 | 2>(1);
  const [provider, setProvider] = useState("");
  const [model, setModel] = useState("");
  const [version, setVersion] = useState("");
  const [dimension, setDimension] = useState("1024");
  const [capabilities, setCapabilities] = useState("");
  const [validationError, setValidationError] = useState("");

  useEffect(() => {
    if (!open) return;
    const suggested =
      DATASET_EMBEDDING_MODELS.find(
        (item) =>
          item.provider !== serving?.embedding_provider ||
          item.model !== serving?.embedding_model ||
          item.dimension !== serving?.embedding_dimension
      ) ?? DATASET_EMBEDDING_MODELS[0];
    setStep(1);
    setProvider(suggested.provider);
    setModel(suggested.model);
    setVersion("");
    setDimension(String(suggested.dimension));
    setCapabilities("");
    setValidationError("");
  }, [open, serving]);

  const request = useMemo<StartEmbeddingMigrationRequest>(() => {
    const capabilityList = capabilities
      .split(",")
      .map((item) => item.trim())
      .filter(Boolean);
    return {
      embedding_provider: provider.trim(),
      embedding_model: model.trim(),
      embedding_model_version: version.trim(),
      embedding_dimension: Number(dimension),
      ...(capabilities.trim() ? { capabilities: [...new Set(capabilityList)] } : {}),
    };
  }, [capabilities, dimension, model, provider, version]);

  function selectKnownModel(value: string) {
    const [selectedProvider, ...modelParts] = value.split(":");
    const selectedModel = modelParts.join(":");
    const option = DATASET_EMBEDDING_MODELS.find(
      (item) => item.provider === selectedProvider && item.model === selectedModel
    );
    if (!option) return;
    setProvider(option.provider);
    setModel(option.model);
    setDimension(String(option.dimension));
    setValidationError("");
  }

  function continueToReview() {
    if (!request.embedding_provider || !request.embedding_model) {
      setValidationError(t("knowledge.detail.embeddingMigration.validationIdentity"));
      return;
    }
    if (
      !Number.isInteger(request.embedding_dimension) ||
      request.embedding_dimension < 1 ||
      request.embedding_dimension > 8192
    ) {
      setValidationError(t("knowledge.detail.embeddingMigration.validationDimension"));
      return;
    }
    if (
      serving &&
      serving.embedding_provider === request.embedding_provider &&
      serving.embedding_model === request.embedding_model &&
      serving.embedding_model_version === (request.embedding_model_version ?? "") &&
      serving.embedding_dimension === request.embedding_dimension
    ) {
      setValidationError(t("knowledge.detail.embeddingMigration.validationSameIdentity"));
      return;
    }
    setValidationError("");
    setStep(2);
  }

  return (
    <Dialog open={open} onOpenChange={(next) => !busy && onOpenChange(next)}>
      <DialogContent className="max-w-2xl" data-testid="embedding-start-wizard">
        <DialogHeader>
          <DialogTitle>{t("knowledge.detail.embeddingMigration.startTitle")}</DialogTitle>
          <DialogDescription>
            {t("knowledge.detail.embeddingMigration.startDescription", { step })}
          </DialogDescription>
        </DialogHeader>

        {step === 1 ? (
          <div className="space-y-4">
            <div className="space-y-2">
              <Label>{t("knowledge.detail.embeddingMigration.knownTarget")}</Label>
              <Select
                value={`${provider}:${model}`}
                onValueChange={selectKnownModel}
              >
                <SelectTrigger data-testid="embedding-known-model">
                  <SelectValue placeholder={t("knowledge.detail.embeddingMigration.customTarget")} />
                </SelectTrigger>
                <SelectContent>
                  {DATASET_EMBEDDING_MODELS.map((item) => (
                    <SelectItem
                      key={`${item.provider}:${item.model}`}
                      value={`${item.provider}:${item.model}`}
                    >
                      {item.label} · {item.dimension}D
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="embedding-target-provider">
                  {t("knowledge.detail.embeddingMigration.provider")}
                </Label>
                <Input
                  id="embedding-target-provider"
                  value={provider}
                  onChange={(event) => setProvider(event.target.value)}
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="embedding-target-model">
                  {t("knowledge.detail.embeddingMigration.model")}
                </Label>
                <Input
                  id="embedding-target-model"
                  value={model}
                  onChange={(event) => setModel(event.target.value)}
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="embedding-target-version">
                  {t("knowledge.detail.embeddingMigration.version")}
                </Label>
                <Input
                  id="embedding-target-version"
                  value={version}
                  placeholder={t("knowledge.detail.embeddingMigration.versionPlaceholder")}
                  onChange={(event) => setVersion(event.target.value)}
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="embedding-target-dimension">
                  {t("knowledge.detail.embeddingMigration.dimension")}
                </Label>
                <Input
                  id="embedding-target-dimension"
                  type="number"
                  min={1}
                  max={8192}
                  value={dimension}
                  onChange={(event) => setDimension(event.target.value)}
                />
              </div>
            </div>
            <div className="space-y-2">
              <Label htmlFor="embedding-target-capabilities">
                {t("knowledge.detail.embeddingMigration.capabilities")}
              </Label>
              <Input
                id="embedding-target-capabilities"
                value={capabilities}
                placeholder={t("knowledge.detail.embeddingMigration.capabilitiesPlaceholder")}
                onChange={(event) => setCapabilities(event.target.value)}
              />
              <p className="text-xs text-muted-foreground">
                {t("knowledge.detail.embeddingMigration.capabilitiesHint")}
              </p>
            </div>
            {validationError ? (
              <p className="text-sm text-destructive" role="alert">
                {validationError}
              </p>
            ) : null}
          </div>
        ) : (
          <div className="space-y-4" data-testid="embedding-start-review">
            <div className="grid gap-3 sm:grid-cols-2">
              <div className="rounded-xl border border-border/60 bg-muted/20 p-3">
                <p className="text-xs font-medium text-muted-foreground">
                  {t("knowledge.detail.embeddingMigration.servingGeneration")}
                </p>
                <BindingSummary
                  binding={serving}
                  emptyLabel={t("knowledge.detail.embeddingMigration.noServing")}
                />
              </div>
              <div className="rounded-xl border border-primary/20 bg-primary/5 p-3">
                <p className="text-xs font-medium text-muted-foreground">
                  {t("knowledge.detail.embeddingMigration.targetGeneration")}
                </p>
                <p className="mt-1 text-sm font-medium text-foreground">
                  {request.embedding_provider} · {request.embedding_model}
                </p>
                <p className="mt-1 text-xs text-muted-foreground">
                  {request.embedding_model_version || t("knowledge.detail.embeddingMigration.noVersion")}
                  {" · "}
                  {request.embedding_dimension}D
                </p>
                <p className="mt-1 text-xs text-muted-foreground">
                  {request.capabilities
                    ? request.capabilities.join(", ") || t("knowledge.detail.embeddingMigration.noCapabilities")
                    : t("knowledge.detail.embeddingMigration.inheritCapabilities")}
                </p>
              </div>
            </div>
            <div className="rounded-xl border border-amber-500/25 bg-amber-500/10 p-3 text-sm text-amber-800 dark:text-amber-200">
              <p className="font-medium">{t("knowledge.detail.embeddingMigration.startConfirmTitle")}</p>
              <p className="mt-1 text-xs">
                {t("knowledge.detail.embeddingMigration.startConfirmDescription")}
              </p>
            </div>
          </div>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={busy}>
            {t("common.cancel")}
          </Button>
          {step === 2 ? (
            <Button variant="quiet" onClick={() => setStep(1)} disabled={busy}>
              {t("knowledge.detail.embeddingMigration.back")}
            </Button>
          ) : null}
          {step === 1 ? (
            <Button variant="primary" onClick={continueToReview} data-testid="embedding-start-next">
              {t("knowledge.detail.embeddingMigration.review")}
            </Button>
          ) : (
            <Button
              variant="primary"
              onClick={() => void onSubmit(request)}
              disabled={busy}
              data-testid="embedding-start-confirm"
            >
              {busy ? <Loader2 className="mr-2 h-4 w-4 animate-spin" /> : <GitBranch className="mr-2 h-4 w-4" />}
              {t("knowledge.detail.embeddingMigration.createShadow")}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function EmbeddingMigrationPanel({
  datasetId,
  active,
  onMigrationChange,
}: EmbeddingMigrationPanelProps) {
  const { i18n, t } = useTranslation();
  const [description, setDescription] = useState<EmbeddingMigrationDescription | null>(null);
  const [loading, setLoading] = useState(false);
  const [busyAction, setBusyAction] = useState<BusyAction | null>(null);
  const [operatorError, setOperatorError] = useState<OperatorError | null>(null);
  const [wizardOpen, setWizardOpen] = useState(false);
  const [confirmAction, setConfirmAction] = useState<ConfirmedAction | null>(null);
  const [retentionSeconds, setRetentionSeconds] = useState("604800");
  const [keepShadow, setKeepShadow] = useState(true);
  const [abortReason, setAbortReason] = useState("");
  const [purgeShadow, setPurgeShadow] = useState(true);
  const [actionJob, setActionJob] = useState<EmbeddingMigrationActionJob | null>(null);
  const [jobPollError, setJobPollError] = useState<string | null>(null);
  const requestSequence = useRef(0);
  const actionJobRef = useRef<EmbeddingMigrationActionJob | null>(null);
  const onMigrationChangeRef = useRef(onMigrationChange);
  onMigrationChangeRef.current = onMigrationChange;

  const migration = selectEmbeddingMigrationJob(description) as EmbeddingMigrationJob | null;
  const state = migration?.state ?? null;
  const progress = getEmbeddingMigrationProgress(description);
  const controls = getEmbeddingMigrationControls(
    state,
    progress.pending,
    actionJob,
    migration?.migration_id ?? null
  );
  const { actions, actionJobState, canStart } = controls;

  const adoptActionJob = useCallback(
    (nextJob: EmbeddingMigrationActionJob | null) => {
      if (!nextJob) {
        actionJobRef.current = null;
        setActionJob(null);
        return null;
      }
      const previous = actionJobRef.current;
      const merged = mergeEmbeddingActionJob(previous, nextJob);
      actionJobRef.current = merged;
      setActionJob(merged);
      const storage = browserLocalStorage();
      if (!storage) return merged;
      if (shouldPollEmbeddingActionJob(merged)) {
        persistEmbeddingActionJobPointer(storage, datasetId, merged);
      } else {
        clearEmbeddingActionJobPointer(storage, datasetId, merged.job_id);
      }
      return merged;
    },
    [datasetId]
  );

  const restoreActionJob = useCallback(
    async (next: EmbeddingMigrationDescription, requestId: number) => {
      const selectedMigration = selectEmbeddingMigrationJob(next) as EmbeddingMigrationJob | null;
      const serverJob = selectServerEmbeddingActionJob(
        next.active_action_job,
        next.recent_action_jobs,
        selectedMigration?.migration_id
      );
      if (serverJob) {
        if (requestId !== requestSequence.current) return;
        setJobPollError(null);
        adoptActionJob(serverJob);
        return;
      }
      if (!selectedMigration) {
        adoptActionJob(null);
        const storage = browserLocalStorage();
        if (storage) clearEmbeddingActionJobPointer(storage, datasetId);
        return;
      }

      const storage = browserLocalStorage();
      if (!storage) {
        adoptActionJob(null);
        return;
      }
      const pointer = readEmbeddingActionJobPointer(
        storage,
        datasetId,
        selectedMigration.migration_id
      );
      if (!pointer) {
        adoptActionJob(null);
        return;
      }

      const placeholder: EmbeddingMigrationActionJob = {
        job_id: pointer.jobId,
        migration_id: pointer.migrationId,
        dataset_id: datasetId,
        action: pointer.action,
        state: "queued",
        payload: {},
        result: null,
        error: null,
        attempt_count: 0,
        poll_after_ms: pointer.pollAfterMs,
      };
      adoptActionJob(placeholder);
      try {
        const recovered = await getEmbeddingMigrationActionJob(
          datasetId,
          pointer.migrationId,
          pointer.jobId
        );
        if (requestId !== requestSequence.current) return;
        setJobPollError(null);
        adoptActionJob(recovered);
      } catch (error) {
        if (requestId !== requestSequence.current) return;
        const parsed = parseOperatorError(error);
        setJobPollError(parsed.detail);
        if (parsed.status === 404) {
          clearEmbeddingActionJobPointer(storage, datasetId, pointer.jobId);
          adoptActionJob(null);
          setOperatorError(parsed);
        }
      }
    },
    [adoptActionJob, datasetId]
  );

  const refresh = useCallback(
    async (silent = false) => {
      const requestId = ++requestSequence.current;
      if (!silent) setLoading(true);
      if (!silent) setBusyAction("refresh");
      try {
        const next = await describeEmbeddingMigration(datasetId);
        if (requestId !== requestSequence.current) return;
        setDescription(next);
        if (!silent) setOperatorError(null);
        await restoreActionJob(next, requestId);
      } catch (error) {
        if (requestId !== requestSequence.current) return;
        setOperatorError(parseOperatorError(error));
      } finally {
        if (requestId === requestSequence.current) {
          setLoading(false);
          setBusyAction((current) => (current === "refresh" ? null : current));
        }
      }
    },
    [datasetId, restoreActionJob]
  );

  useEffect(() => {
    setDescription(null);
    setOperatorError(null);
    setJobPollError(null);
    actionJobRef.current = null;
    setActionJob(null);
    if (active) void refresh();
  }, [active, datasetId, refresh]);

  useEffect(() => {
    if (!active || !shouldPollEmbeddingMigration(state)) return;
    const timer = window.setInterval(() => void refresh(true), 5_000);
    return () => window.clearInterval(timer);
  }, [active, refresh, state]);

  const handleJobPollError = useCallback(
    (currentJob: EmbeddingMigrationActionJob, error: unknown): boolean => {
      const parsed = parseOperatorError(error);
      setJobPollError(parsed.detail);
      if (parsed.status !== 404) return true;

      const storage = browserLocalStorage();
      if (storage) {
        clearEmbeddingActionJobPointer(storage, datasetId, currentJob.job_id);
      }
      adoptActionJob(null);
      setOperatorError(parsed);
      return false;
    },
    [adoptActionJob, datasetId]
  );

  useEffect(() => {
    const initialJob = actionJobRef.current;
    if (!active || !initialJob || !shouldPollEmbeddingActionJob(initialJob)) return;

    let stopped = false;
    let timer: number | undefined;

    function schedule(job: EmbeddingMigrationActionJob, delayOverride?: number) {
      const delay = delayOverride ?? embeddingActionJobPollDelay(job);
      timer = window.setTimeout(() => void poll(job), delay);
    }

    async function poll(currentJob: EmbeddingMigrationActionJob) {
      try {
        const response = await getEmbeddingMigrationActionJob(
          datasetId,
          currentJob.migration_id,
          currentJob.job_id
        );
        if (stopped) return;
        setJobPollError(null);
        const nextJob = adoptActionJob(response);
        if (nextJob && shouldPollEmbeddingActionJob(nextJob)) {
          schedule(nextJob);
        } else {
          await refresh(true);
          onMigrationChangeRef.current?.();
        }
      } catch (error) {
        if (stopped) return;
        if (handleJobPollError(currentJob, error)) {
          schedule(
            currentJob,
            Math.max(embeddingActionJobPollDelay(currentJob), 2_000)
          );
        }
      }
    }

    schedule(initialJob);
    return () => {
      stopped = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [
    active,
    actionJob?.job_id,
    actionJobState,
    adoptActionJob,
    datasetId,
    handleJobPollError,
    refresh,
  ]);

  async function pollActionJobNow(): Promise<void> {
    const currentJob = actionJobRef.current;
    if (!currentJob) return;
    setJobPollError(null);
    try {
      const response = await getEmbeddingMigrationActionJob(
        datasetId,
        currentJob.migration_id,
        currentJob.job_id
      );
      adoptActionJob(response);
      if (!shouldPollEmbeddingActionJob(response)) await refresh(true);
    } catch (error) {
      handleJobPollError(currentJob, error);
    }
  }

  function reportActionError(error: unknown) {
    const parsed = parseOperatorError(error);
    setOperatorError(parsed);
    const titleKey = operatorErrorTitleKey(parsed.status);
    toast.error(
      t(`knowledge.detail.embeddingMigration.errors.${titleKey}`),
      parsed.detail
    );
  }

  async function runAction(action: EmbeddingMigrationActionName): Promise<void> {
    if (!migration || busyAction) return;
    setBusyAction(action);
    setOperatorError(null);
    try {
      if (isEmbeddingActionName(action)) {
        const queuedJob = await enqueueDurableEmbeddingAction(
          datasetId,
          migration.migration_id,
          action
        );
        adoptActionJob(queuedJob);
        toast.success(
          queuedJob.reused
            ? t("knowledge.detail.embeddingMigration.actionJob.reusedToast")
            : t("knowledge.detail.embeddingMigration.actionJob.queuedToast")
        );
        await refresh(true);
        onMigrationChange?.();
        return;
      }

      switch (action) {
        case "cutover":
          if (
            !Number.isInteger(Number(retentionSeconds)) ||
            Number(retentionSeconds) < 0
          ) {
            throw new Error(
              t("knowledge.detail.embeddingMigration.validationRetention")
            );
          }
          await cutoverEmbeddingMigration(
            datasetId,
            migration.migration_id,
            Number(retentionSeconds)
          );
          break;
        case "rollback":
          await rollbackEmbeddingMigration(datasetId, migration.migration_id, keepShadow);
          break;
        case "abort":
          await abortEmbeddingMigration(datasetId, migration.migration_id, {
            reason: abortReason.trim() || undefined,
            purgeShadow,
          });
          break;
      }
      toast.success(t(`knowledge.detail.embeddingMigration.actionSuccess.${action}`));
      await refresh(true);
      onMigrationChange?.();
    } catch (error) {
      reportActionError(error);
      if (isAxiosError(error) && [404, 409].includes(error.response?.status ?? 0)) {
        await refresh(true);
      }
    } finally {
      setBusyAction(null);
    }
  }

  async function submitStart(request: StartEmbeddingMigrationRequest): Promise<void> {
    if (busyAction) return;
    setBusyAction("start");
    setOperatorError(null);
    try {
      await startEmbeddingMigration(datasetId, request);
      toast.success(t("knowledge.detail.embeddingMigration.actionSuccess.start"));
      setWizardOpen(false);
      await refresh(true);
      onMigrationChange?.();
    } catch (error) {
      reportActionError(error);
    } finally {
      setBusyAction(null);
    }
  }

  function requestAction(action: EmbeddingMigrationActionName) {
    if (["cutover", "rollback", "abort"].includes(action)) {
      setConfirmAction(action as ConfirmedAction);
      return;
    }
    void runAction(action);
  }

  function confirmCurrentAction() {
    const action = confirmAction;
    setConfirmAction(null);
    if (action) void runAction(action);
  }

  const targetCollection =
    description?.target_binding?.collection_name ||
    readString(migration?.totals, "target_collection");
  const verifiedPoints = readNumber(migration?.totals, "verified_points");
  const targetBinding = description?.target_binding ?? null;

  return (
    <Card className="p-5" data-testid="embedding-migration-panel">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h3 className="flex items-center gap-2 font-semibold text-foreground">
            <GitBranch className="h-5 w-5 text-primary" />
            {t("knowledge.detail.embeddingMigration.title")}
          </h3>
          <p className="mt-1 text-xs text-muted-foreground">
            {t("knowledge.detail.embeddingMigration.subtitle")}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => void refresh()}
            disabled={busyAction !== null}
            aria-label={t("knowledge.detail.embeddingMigration.refresh")}
            data-testid="embedding-migration-refresh"
          >
            <RefreshCw className={`mr-1.5 h-3.5 w-3.5 ${busyAction === "refresh" ? "animate-spin" : ""}`} />
            {t("knowledge.detail.embeddingMigration.refresh")}
          </Button>
          {canStart ? (
            <Button
              variant="primary"
              size="sm"
              onClick={() => setWizardOpen(true)}
              disabled={busyAction !== null || !description?.serving_binding}
              data-testid="embedding-start-open"
            >
              <GitBranch className="mr-1.5 h-3.5 w-3.5" />
              {t("knowledge.detail.embeddingMigration.start")}
            </Button>
          ) : null}
        </div>
      </div>

      {operatorError ? (
        <div
          className="mt-4 rounded-xl border border-destructive/25 bg-destructive/5 p-3"
          role="alert"
          data-testid={`embedding-error-${operatorError.status ?? "generic"}`}
        >
          <div className="flex items-start gap-2">
            <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-destructive" />
            <div>
              <p className="text-sm font-medium text-destructive">
                {operatorError.status
                  ? t("knowledge.detail.embeddingMigration.httpError", {
                      status: operatorError.status,
                    })
                  : t("knowledge.detail.embeddingMigration.requestFailed")}
              </p>
              <p className="mt-0.5 text-xs text-muted-foreground">{operatorError.detail}</p>
            </div>
          </div>
        </div>
      ) : null}

      {loading && !description ? (
        <div className="flex min-h-40 items-center justify-center text-muted-foreground">
          <Loader2 className="mr-2 h-4 w-4 animate-spin" />
          {t("knowledge.detail.embeddingMigration.loading")}
        </div>
      ) : null}

      {description ? (
        <div className="mt-4 space-y-4">
          <div className="grid gap-3 lg:grid-cols-3">
            <div className="rounded-xl border border-border/60 bg-muted/20 p-3">
              <p className="mb-2 flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
                <Database className="h-3.5 w-3.5" />
                {t("knowledge.detail.embeddingMigration.servingGeneration")}
              </p>
              <BindingSummary
                binding={description.serving_binding}
                emptyLabel={t("knowledge.detail.embeddingMigration.noServing")}
              />
            </div>
            <div className="rounded-xl border border-border/60 bg-muted/20 p-3">
              <p className="mb-2 flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
                <GitBranch className="h-3.5 w-3.5" />
                {t("knowledge.detail.embeddingMigration.targetGeneration")}
              </p>
              <TargetGenerationSummary
                binding={targetBinding}
                collectionName={targetCollection}
                migrationState={state}
              />
            </div>
            <div className="rounded-xl border border-border/60 bg-muted/20 p-3">
              <p className="mb-2 flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
                <Activity className="h-3.5 w-3.5" />
                {t("knowledge.detail.embeddingMigration.job")}
              </p>
              {migration ? (
                <div className="space-y-1.5" data-testid="embedding-job">
                  <Badge
                    variant="outline"
                    className={statusBadgeClass(migration.state)}
                    data-testid="embedding-migration-state"
                  >
                    {t(`knowledge.detail.embeddingMigration.states.${migration.state}`)}
                  </Badge>
                  <p className="truncate font-mono text-[11px] text-muted-foreground" title={migration.migration_id}>
                    {migration.migration_id}
                  </p>
                  <p className="text-xs text-muted-foreground">
                    {t("knowledge.detail.embeddingMigration.updatedAt", {
                      value: formatTimestamp(migration.updated_at, i18n.language),
                    })}
                  </p>
                  {migration.error ? (
                    <p className="text-xs text-destructive" data-testid="embedding-job-error">
                      {migration.error}
                    </p>
                  ) : null}
                </div>
              ) : (
                <p className="text-sm text-muted-foreground" data-testid="embedding-no-job">
                  {t("knowledge.detail.embeddingMigration.noJob")}
                </p>
              )}
            </div>
          </div>

          {migration ? (
            <div className="rounded-xl border border-border/60 p-3" data-testid="embedding-progress">
              <div className="flex items-center justify-between gap-3 text-xs">
                <span className="font-medium text-foreground">
                  {t("knowledge.detail.embeddingMigration.progress")}
                </span>
                <span className="font-mono text-muted-foreground">
                  {progress.pending === null
                    ? t("knowledge.detail.embeddingMigration.progressUnknown")
                    : t("knowledge.detail.embeddingMigration.progressValue", {
                        completed: progress.completed,
                        total: progress.total,
                        pending: progress.pending,
                      })}
                </span>
              </div>
              <div
                className="mt-2 h-2 overflow-hidden rounded-full bg-muted"
                role="progressbar"
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={Math.round(progress.percent)}
              >
                <div
                  className="h-full rounded-full bg-primary transition-[width] duration-300"
                  style={{ width: `${progress.percent}%` }}
                />
              </div>
              {migration.gate ? (
                <div className="mt-3 flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
                  <span>
                    {t("knowledge.detail.embeddingMigration.gateSamples", {
                      count: readNumber(migration.gate, "samples") ?? "?",
                    })}
                  </span>
                  <span>
                    {t("knowledge.detail.embeddingMigration.gateShadow", {
                      value: readNumber(migration.gate, "shadow_hit_rate") ?? "?",
                    })}
                  </span>
                  <span>
                    {t("knowledge.detail.embeddingMigration.gateServing", {
                      value: readNumber(migration.gate, "serving_hit_rate") ?? "?",
                    })}
                  </span>
                </div>
              ) : null}
            </div>
          ) : null}

          {actionJob ? (
            <EmbeddingActionJobCard
              job={actionJob}
              pollError={jobPollError}
              retrying={busyAction === actionJob.action}
              onPollNow={() => void pollActionJobNow()}
              onRetry={() => void runAction(actionJob.action)}
            />
          ) : null}

          {actions.length > 0 ? (
            <div className="flex flex-wrap items-center gap-2" data-testid="embedding-actions">
              {actions.map((action) => {
                return (
                  <Button
                    key={action}
                    variant={actionButtonVariant(action)}
                    size="sm"
                    onClick={() => requestAction(action)}
                    disabled={busyAction !== null}
                    data-testid={`embedding-action-${action}`}
                  >
                    <MigrationActionIcon action={action} busy={busyAction === action} />
                    {t(`knowledge.detail.embeddingMigration.actions.${action}`)}
                  </Button>
                );
              })}
            </div>
          ) : null}

          <div>
            <h4 className="flex items-center gap-2 text-sm font-semibold text-foreground">
              <Activity className="h-4 w-4 text-primary" />
              {t("knowledge.detail.embeddingMigration.collectionHealth")}
            </h4>
            <p className="mt-1 text-xs text-muted-foreground">
              {t("knowledge.detail.embeddingMigration.healthReceiptHint")}
            </p>
            <div className="mt-3">
              <CollectionHealthReceipt
                receipt={description.collection_health}
                verifiedPoints={verifiedPoints}
              />
            </div>
          </div>

          <div
            className="rounded-xl border border-amber-500/25 bg-amber-500/10 p-3"
            data-testid="embedding-lexical-blocked"
          >
            <div className="flex items-start gap-2">
              <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0 text-amber-700 dark:text-amber-300" />
              <div>
                <div className="flex flex-wrap items-center gap-2">
                  <p className="text-sm font-medium text-amber-900 dark:text-amber-100">
                    {t("knowledge.detail.embeddingMigration.lexicalTitle")}
                  </p>
                  <Badge variant="outline" className="border-amber-500/40 text-amber-800 dark:text-amber-200">
                    BLOCKED
                  </Badge>
                </div>
                <p className="mt-1 text-xs text-amber-800 dark:text-amber-200">
                  {t("knowledge.detail.embeddingMigration.lexicalBlocked")}
                </p>
              </div>
            </div>
          </div>
        </div>
      ) : null}

      <StartMigrationWizard
        open={wizardOpen}
        serving={description?.serving_binding ?? null}
        busy={busyAction === "start"}
        onOpenChange={setWizardOpen}
        onSubmit={submitStart}
      />

      <AlertDialog open={confirmAction !== null} onOpenChange={(open) => !open && setConfirmAction(null)}>
        <AlertDialogContent data-testid="embedding-action-confirmation">
          <AlertDialogHeader>
            <AlertDialogTitle>
              {confirmAction
                ? t(`knowledge.detail.embeddingMigration.confirm.${confirmAction}Title`)
                : ""}
            </AlertDialogTitle>
            <AlertDialogDescription>
              {confirmAction
                ? t(`knowledge.detail.embeddingMigration.confirm.${confirmAction}Description`)
                : ""}
            </AlertDialogDescription>
          </AlertDialogHeader>

          {confirmAction === "cutover" ? (
            <div className="space-y-2">
              <Label htmlFor="embedding-retention-seconds">
                {t("knowledge.detail.embeddingMigration.retentionSeconds")}
              </Label>
              <Input
                id="embedding-retention-seconds"
                type="number"
                min={0}
                value={retentionSeconds}
                onChange={(event) => setRetentionSeconds(event.target.value)}
              />
            </div>
          ) : null}
          {confirmAction === "rollback" ? (
            <label className="flex items-start gap-2 text-sm">
              <Checkbox checked={keepShadow} onCheckedChange={setKeepShadow} />
              <span>{t("knowledge.detail.embeddingMigration.keepShadow")}</span>
            </label>
          ) : null}
          {confirmAction === "abort" ? (
            <div className="space-y-3">
              <div className="space-y-2">
                <Label htmlFor="embedding-abort-reason">
                  {t("knowledge.detail.embeddingMigration.abortReason")}
                </Label>
                <Textarea
                  id="embedding-abort-reason"
                  value={abortReason}
                  onChange={(event) => setAbortReason(event.target.value)}
                />
              </div>
              <label className="flex items-start gap-2 text-sm">
                <Checkbox checked={purgeShadow} onCheckedChange={setPurgeShadow} />
                <span>{t("knowledge.detail.embeddingMigration.purgeShadow")}</span>
              </label>
            </div>
          ) : null}

          <AlertDialogFooter>
            <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
            <AlertDialogAction
              onClick={confirmCurrentAction}
              className={confirmAction === "cutover" ? "" : "bg-destructive text-destructive-foreground hover:bg-destructive/90"}
              data-testid="embedding-action-confirm"
            >
              {confirmAction
                ? t(`knowledge.detail.embeddingMigration.confirm.${confirmAction}Action`)
                : ""}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </Card>
  );
}
