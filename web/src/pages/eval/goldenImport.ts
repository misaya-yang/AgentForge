import { importEvalExamples, type EvalExampleImportItem } from "@/api/eval";

export const GOLDEN_IMPORT_BATCH_SIZE = 500;

export interface GoldenCaseValidationError {
  case_id: string;
  errors: string[];
}

export interface GoldenParseResult {
  cases: EvalExampleImportItem[];
  lineCount: number;
}

export interface GoldenValidationResult {
  valid: boolean;
  caseCount: number;
  errors: GoldenCaseValidationError[];
}

export interface GoldenBatchImportProgress {
  batchIndex: number;
  batchCount: number;
  importedSoFar: number;
  totalCases: number;
}

export interface GoldenBatchImportResult {
  imported: number;
  skipped: number;
  batchCount: number;
  failedBatches: number;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function parseGoldenJsonl(text: string): GoldenParseResult {
  const cases: EvalExampleImportItem[] = [];
  const lines = text.split(/\r?\n/);
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index]?.trim();
    if (!line) continue;
    let payload: unknown;
    try {
      payload = JSON.parse(line);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      throw new Error(`Invalid JSONL at line ${index + 1}: ${message}`, { cause: error });
    }
    if (!isRecord(payload)) {
      throw new Error(`Golden case at line ${index + 1} must be an object`);
    }
    cases.push(normalizeGoldenCase(payload));
  }
  return { cases, lineCount: lines.length };
}

export function normalizeGoldenCase(raw: Record<string, unknown>): EvalExampleImportItem {
  const caseId = typeof raw.case_id === "string" ? raw.case_id.trim() : "";
  return {
    case_id: caseId,
    split: typeof raw.split === "string" && raw.split.trim() ? raw.split.trim() : "regression",
    input: isRecord(raw.input) ? raw.input : {},
    expected_output: isRecord(raw.expected_output) ? raw.expected_output : {},
    expected_trajectory: isRecord(raw.expected_trajectory) ? raw.expected_trajectory : {},
    assertions: Array.isArray(raw.assertions)
      ? raw.assertions.filter((item): item is Record<string, unknown> => isRecord(item))
      : [],
    metadata: isRecord(raw.metadata) ? raw.metadata : {},
    source_trace_id: typeof raw.source_trace_id === "string" ? raw.source_trace_id : null,
    source_span_id: typeof raw.source_span_id === "string" ? raw.source_span_id : null,
  };
}

function validateGoldenCase(caseItem: EvalExampleImportItem): string[] {
  const errors: string[] = [];
  if (!caseItem.case_id) {
    errors.push("case_id must be a non-empty string");
  }
  if (!isRecord(caseItem.input)) {
    errors.push("input must be an object");
  }
  if (!isRecord(caseItem.expected_output)) {
    errors.push("expected_output must be an object");
  }
  if (!isRecord(caseItem.expected_trajectory)) {
    errors.push("expected_trajectory must be an object");
  }
  if (!Array.isArray(caseItem.assertions)) {
    errors.push("assertions must be a list");
  }
  if (!isRecord(caseItem.metadata)) {
    errors.push("metadata must be an object");
  }
  if (!caseItem.split?.trim()) {
    errors.push("split must be a non-empty string");
  }
  const critical = caseItem.metadata?.critical;
  if (critical !== undefined && critical !== null && typeof critical !== "boolean") {
    errors.push("metadata.critical must be boolean when present");
  }
  return errors;
}

export function validateGoldenCases(cases: EvalExampleImportItem[]): GoldenValidationResult {
  const seen = new Set<string>();
  const errors: GoldenCaseValidationError[] = [];
  cases.forEach((caseItem, index) => {
    const caseId = caseItem.case_id || `line-${index + 1}`;
    const caseErrors = validateGoldenCase(caseItem);
    if (seen.has(caseId)) {
      caseErrors.push("duplicate case_id");
    }
    seen.add(caseId);
    if (caseErrors.length > 0) {
      errors.push({ case_id: caseId, errors: caseErrors });
    }
  });
  return {
    valid: errors.length === 0,
    caseCount: cases.length,
    errors,
  };
}

export function chunkGoldenCases<T>(cases: T[], batchSize = GOLDEN_IMPORT_BATCH_SIZE): T[][] {
  if (batchSize <= 0) return [cases];
  const batches: T[][] = [];
  for (let index = 0; index < cases.length; index += batchSize) {
    batches.push(cases.slice(index, index + batchSize));
  }
  return batches;
}

export async function importGoldenCasesInBatches(
  datasetId: string,
  cases: EvalExampleImportItem[],
  options?: {
    batchSize?: number;
    mode?: "skip_duplicates" | "append";
    onProgress?: (progress: GoldenBatchImportProgress) => void;
  }
): Promise<GoldenBatchImportResult> {
  const batches = chunkGoldenCases(cases, options?.batchSize ?? GOLDEN_IMPORT_BATCH_SIZE);
  let imported = 0;
  let skipped = 0;

  for (let batchIndex = 0; batchIndex < batches.length; batchIndex += 1) {
    const batch = batches[batchIndex] ?? [];
    try {
      const response = await importEvalExamples(datasetId, batch, {
        mode: options?.mode ?? "skip_duplicates",
      });
      imported += response.imported;
      skipped += response.skipped;
      options?.onProgress?.({
        batchIndex: batchIndex + 1,
        batchCount: batches.length,
        importedSoFar: imported,
        totalCases: cases.length,
      });
    } catch (error) {
      throw new Error(
        `Batch ${batchIndex + 1} of ${batches.length} failed after importing ${imported} case(s)`,
        { cause: error }
      );
    }
  }

  return {
    imported,
    skipped,
    batchCount: batches.length,
    failedBatches: 0,
  };
}

export function goldenCasesToJsonl(cases: EvalExampleImportItem[]): string {
  return cases.map((caseItem) => JSON.stringify(caseItem)).join("\n");
}
