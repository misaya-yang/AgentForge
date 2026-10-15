import { Alert, App as AntApp, Button, Progress, Switch, Upload } from "antd";
import { FileUp, UploadCloud } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import type { EvalExampleImportItem } from "@/api/eval";
import {
  GOLDEN_IMPORT_BATCH_SIZE,
  importGoldenCasesInBatches,
  parseGoldenJsonl,
  validateGoldenCases,
  type GoldenBatchImportProgress,
  type GoldenCaseValidationError,
} from "../goldenImport";

const ACCEPTED_EXTENSIONS = [".jsonl", ".json", ".txt"];
const MAX_FILE_BYTES = 8 * 1024 * 1024;

interface GoldenJsonlImportProps {
  datasetId: string | null;
  onImported?: () => void | Promise<void>;
  readOnly?: boolean;
}

function formatValidationErrors(errors: GoldenCaseValidationError[], limit = 6): string {
  return errors
    .slice(0, limit)
    .map((entry) => `${entry.case_id}: ${entry.errors.join(", ")}`)
    .join("\n");
}

export function GoldenJsonlImport({ datasetId, onImported, readOnly = false }: GoldenJsonlImportProps) {
  const { t } = useTranslation();
  const { message } = AntApp.useApp();
  const [fileName, setFileName] = useState<string | null>(null);
  const [cases, setCases] = useState<EvalExampleImportItem[]>([]);
  const [parseError, setParseError] = useState<string | null>(null);
  const [importing, setImporting] = useState(false);
  const [progress, setProgress] = useState<GoldenBatchImportProgress | null>(null);
  const [lastImportSummary, setLastImportSummary] = useState<string | null>(null);
  const [skipDuplicates, setSkipDuplicates] = useState(true);

  const validation = useMemo(() => validateGoldenCases(cases), [cases]);
  const batchCount = useMemo(
    () => Math.max(1, Math.ceil(cases.length / GOLDEN_IMPORT_BATCH_SIZE)),
    [cases.length]
  );

  async function readGoldenFile(file: File) {
    setParseError(null);
    setLastImportSummary(null);
    setProgress(null);
    if (file.size > MAX_FILE_BYTES) {
      setFileName(file.name);
      setCases([]);
      setParseError(t("eval.goldenImport.fileTooLarge", { maxMb: 8 }));
      return;
    }
    const text = await file.text();
    try {
      const parsed = parseGoldenJsonl(text);
      if (parsed.cases.length === 0) {
        setFileName(file.name);
        setCases([]);
        setParseError(t("eval.goldenImport.emptyFile"));
        return;
      }
      setFileName(file.name);
      setCases(parsed.cases);
    } catch (error) {
      setFileName(file.name);
      setCases([]);
      setParseError(error instanceof Error ? error.message : String(error));
    }
  }

  async function handleImport() {
    if (!datasetId) {
      message.error(t("eval.workbench.createDatasetFirst"));
      return;
    }
    if (!validation.valid || cases.length === 0) {
      message.error(t("eval.goldenImport.fixValidationFirst"));
      return;
    }
    setImporting(true);
    setProgress(null);
    setLastImportSummary(null);
    try {
      const result = await importGoldenCasesInBatches(datasetId, cases, {
        mode: skipDuplicates ? "skip_duplicates" : "append",
        onProgress: (next) => setProgress(next),
      });
      const summary = t("eval.goldenImport.importSuccess", {
        imported: result.imported,
        skipped: result.skipped,
        batches: result.batchCount,
      });
      setLastImportSummary(summary);
      message.success(summary);
      await onImported?.();
    } catch (error) {
      const text = error instanceof Error ? error.message : String(error);
      message.error(text);
      setLastImportSummary(text);
    } finally {
      setImporting(false);
      setProgress(null);
    }
  }

  const progressPercent = progress
    ? Math.round((progress.importedSoFar / Math.max(progress.totalCases, 1)) * 100)
    : 0;

  return (
    <section className="eval-golden-import" data-testid="golden-jsonl-import">
      <div className="eval-golden-import-heading">
        <div>
          <h3>{t("eval.goldenImport.title")}</h3>
          <p>{t("eval.goldenImport.description")}</p>
        </div>
        <FileUp size={18} />
      </div>

      <Upload.Dragger
        className="eval-golden-import-dropzone"
        accept={ACCEPTED_EXTENSIONS.join(",")}
        multiple={false}
        showUploadList={false}
        disabled={readOnly || importing}
        beforeUpload={(file) => {
          void readGoldenFile(file);
          return false;
        }}
      >
        <p className="eval-golden-import-dropzone-icon">
          <UploadCloud size={22} />
        </p>
        <p className="eval-golden-import-dropzone-title">{t("eval.goldenImport.dropTitle")}</p>
        <p className="eval-golden-import-dropzone-copy">{t("eval.goldenImport.dropHint")}</p>
      </Upload.Dragger>

      {fileName ? (
        <div className="eval-golden-import-meta">
          <span>{t("eval.goldenImport.selectedFile", { name: fileName })}</span>
          <span>{t("eval.goldenImport.caseCount", { count: cases.length })}</span>
          {cases.length > 0 ? (
            <span>{t("eval.goldenImport.batchPlan", { batches: batchCount, size: GOLDEN_IMPORT_BATCH_SIZE })}</span>
          ) : null}
        </div>
      ) : null}

      {parseError ? (
        <Alert type="error" showIcon title={t("eval.goldenImport.parseFailed")} description={parseError} />
      ) : null}

      {!parseError && cases.length > 0 && !validation.valid ? (
        <Alert
          type="error"
          showIcon
          title={t("eval.goldenImport.validationFailed", { count: validation.errors.length })}
          description={formatValidationErrors(validation.errors)}
        />
      ) : null}

      {!parseError && cases.length > 0 && validation.valid ? (
        <Alert
          type="success"
          showIcon
          title={t("eval.goldenImport.validationPassed", { count: validation.caseCount })}
          description={t("eval.goldenImport.readyToImport")}
        />
      ) : null}

      {importing && progress ? (
        <div className="eval-golden-import-progress">
          <Progress
            percent={progressPercent}
            status="active"
            aria-label={t("eval.goldenImport.progressAria")}
          />
          <span>
            {t("eval.goldenImport.progressCopy", {
              batch: progress.batchIndex,
              batches: progress.batchCount,
              imported: progress.importedSoFar,
              total: progress.totalCases,
            })}
          </span>
        </div>
      ) : null}

      {lastImportSummary ? (
        <Alert type="info" showIcon title={t("eval.goldenImport.lastResult")} description={lastImportSummary} />
      ) : null}

      <div className="eval-golden-import-options">
        <label className="eval-golden-import-toggle">
          <Switch
            checked={skipDuplicates}
            onChange={setSkipDuplicates}
            disabled={readOnly || importing}
            aria-label={t("eval.goldenImport.skipDuplicatesAria")}
          />
          <span>{t("eval.goldenImport.skipDuplicates")}</span>
        </label>
        <span className="eval-golden-import-hint">{t("eval.goldenImport.skipDuplicatesHint")}</span>
      </div>

      <div className="eval-golden-import-actions">
        <Button
          type="primary"
          icon={<UploadCloud size={15} />}
          onClick={() => void handleImport()}
          loading={importing}
          disabled={readOnly || !datasetId || !validation.valid || cases.length === 0}
          data-testid="golden-jsonl-import-submit"
        >
          {t("eval.goldenImport.importButton")}
        </Button>
        {!datasetId ? (
          <span className="eval-golden-import-hint">{t("eval.goldenImport.datasetRequired")}</span>
        ) : null}
      </div>

      <details className="eval-golden-import-format">
        <summary>{t("eval.goldenImport.formatTitle")}</summary>
        <pre>{`{"case_id":"assistant.refund_policy.basic","split":"regression","input":{"input_preview":"..."},"expected_output":{"contains":"refund"},"expected_trajectory":{"required_span_kinds":["lifecycle","model_invocation"]},"assertions":[{"type":"output_contains","value":"refund"}],"metadata":{"review_status":"approved","tags":["support"]}}`}</pre>
      </details>
    </section>
  );
}
