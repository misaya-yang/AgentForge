export interface BatchUploadError {
  filename: string;
  error: string;
  file_index?: number;
  document_id?: string;
  retry_safe?: boolean;
}

interface BatchUploadReceipt {
  accepted: number;
  errors: BatchUploadError[];
}

interface UploadHandlers<TFile extends { name: string }> {
  uploadBatch: (files: TFile[]) => Promise<BatchUploadReceipt>;
  uploadOne: (file: TFile) => Promise<unknown>;
  describeError: (error: unknown) => string;
  isRetrySafe?: (error: unknown) => boolean;
  unknownOutcomeMessage?: string;
}

export interface DatasetUploadOutcome<TFile> {
  accepted: number;
  failures: Array<{ file: TFile; error: string; documentId?: string; retrySafe?: false }>;
}

export interface DatasetUploadConfigInput {
  documentCount?: number;
  chunkingConfig: Record<string, unknown>;
  rerank: { enabled: boolean; model: string };
  embedding: {
    changed: boolean;
    provider: string;
    model: string;
    dimension: number;
  };
}

/** Existing documents pin chunking and embedding identity; uploads only tune retrieval. */
export function buildDatasetUploadConfigPatch(input: DatasetUploadConfigInput) {
  const patch: {
    chunking_config?: Record<string, unknown>;
    retrieval_config: { rerank: { enabled: boolean; model: string } };
    embedding_provider?: string;
    embedding_model?: string;
    embedding_dimension?: number;
  } = { retrieval_config: { rerank: input.rerank } };

  if (input.documentCount !== 0) return patch;
  patch.chunking_config = input.chunkingConfig;
  if (input.embedding.changed) {
    patch.embedding_provider = input.embedding.provider;
    patch.embedding_model = input.embedding.model;
    patch.embedding_dimension = input.embedding.dimension;
  }
  return patch;
}

/** Upload once, retaining every rejected source so the same dialog can retry it. */
export async function uploadDatasetFiles<TFile extends { name: string }>(
  files: TFile[],
  handlers: UploadHandlers<TFile>
): Promise<DatasetUploadOutcome<TFile>> {
  if (files.length >= 3) {
    const receipt = await handlers.uploadBatch(files);
    const failuresByIndex = new Map<number, BatchUploadError>();
    for (const failure of receipt.errors) {
      const fallbackIndices = files.flatMap((file, index) =>
        file.name === failure.filename ? [index] : []
      );
      const index = failure.file_index ??
        (fallbackIndices.length === 1 ? fallbackIndices[0] : -1);
      if (
        !Number.isInteger(index) ||
        index < 0 ||
        index >= files.length ||
        files[index].name !== failure.filename ||
        failuresByIndex.has(index)
      ) {
        throw new Error("Batch upload returned errors that do not match the submitted files");
      }
      failuresByIndex.set(index, failure);
    }
    if (receipt.accepted + failuresByIndex.size !== files.length) {
      throw new Error("Batch upload returned errors that do not match the submitted files");
    }
    const failures = [...failuresByIndex].sort(([left], [right]) => left - right).map(([index, failure]) => {
      const retrySafe = failure.retry_safe === true && !failure.document_id;
      return {
        file: files[index],
        error: retrySafe
          ? failure.error
          : handlers.unknownOutcomeMessage ?? "Upload outcome is uncertain; check the document list before retrying",
        ...(failure.document_id ? { documentId: failure.document_id } : {}),
        ...(retrySafe ? {} : { retrySafe: false as const }),
      };
    });
    return { accepted: receipt.accepted, failures };
  }

  const failures: Array<{ file: TFile; error: string }> = [];
  for (const file of files) {
    try {
      await handlers.uploadOne(file);
    } catch (error) {
      failures.push({
        file,
        error: handlers.describeError(error),
        ...(handlers.isRetrySafe?.(error) ? {} : { retrySafe: false as const }),
      });
    }
  }
  return { accepted: files.length - failures.length, failures };
}
