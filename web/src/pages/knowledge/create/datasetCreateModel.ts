export interface PendingFile {
  id: string;
  file: File;
  name: string;
  size: number;
  status: "pending" | "uploading" | "done" | "error";
  error?: string;
  retrySafe?: boolean;
}

export interface PendingUrl {
  id: string;
  url: string;
  title: string;
  status: "pending" | "uploading" | "done" | "error";
  error?: string;
  retrySafe?: boolean;
}

export const EMBEDDING_MODELS = [
  {
    provider: "gemini",
    model: "gemini-embedding-001",
    nameKey: "knowledge.create.embeddingGemini001",
    dimension: 1024,
  },
  {
    provider: "dashscope",
    model: "text-embedding-v4",
    nameKey: "knowledge.create.embeddingDashscopeV4",
    dimension: 1024,
  },
  {
    provider: "dashscope",
    model: "text-embedding-v3",
    nameKey: "knowledge.create.embeddingDashscopeV3",
    dimension: 1024,
  },
  {
    provider: "dashscope",
    model: "text-embedding-v2",
    nameKey: "knowledge.create.embeddingDashscopeV2",
    dimension: 1536,
  },
  {
    provider: "siliconflow",
    model: "BAAI/bge-m3",
    nameKey: "knowledge.create.embeddingBgeM3",
    dimension: 1024,
  },
  {
    provider: "siliconflow",
    model: "Pro/BAAI/bge-m3",
    nameKey: "knowledge.create.embeddingBgeM3Pro",
    dimension: 1024,
  },
  {
    provider: "siliconflow",
    model: "BAAI/bge-large-zh-v1.5",
    nameKey: "knowledge.create.embeddingBgeLargeZh15",
    dimension: 1024,
  },
  {
    provider: "siliconflow",
    model: "BAAI/bge-large-en-v1.5",
    nameKey: "knowledge.create.embeddingBgeLargeEn15",
    dimension: 1024,
  },
  {
    provider: "siliconflow",
    model: "netease-youdao/bce-embedding-base_v1",
    nameKey: "knowledge.create.embeddingBceBase",
    dimension: 512,
  },
] as const;

export type VisibilityType = "private" | "tenant" | "public";
export type KBType = "document" | "data";
export type UseCase = "basic_qa" | "rich_text_response";

export const MAX_NAME_LENGTH = 100;
export const MAX_FILE_SIZE_MB = 16;
export const MAX_FILE_SIZE = MAX_FILE_SIZE_MB * 1024 * 1024;
export const SUPPORTED_FILE_EXTENSIONS = /\.(pdf|docx|txt|md|html)$/i;
export const URL_PATTERN = /^https?:\/\/([\w-]+\.)+[\w-]+(\/[\w\-./?%&=#]*)?$/i;

interface UploadErrorResponse {
  status?: number;
  data?: unknown;
}

interface UploadErrorEnvelope {
  response?: UploadErrorResponse;
}

export interface FailedSourceSummary {
  key: string;
  name: string;
  error: string;
}

function responseDetail(data: unknown): string | null {
  if (typeof data === "string") return data.trim() || null;
  if (!data || typeof data !== "object") return null;

  const detail = (data as { detail?: unknown }).detail;
  if (typeof detail === "string") return detail.trim() || null;
  return null;
}

export function getSourceUploadError(
  error: unknown,
  messages: { fallback: string; requestTooLarge: string }
): string {
  const response =
    error && typeof error === "object"
      ? (error as UploadErrorEnvelope).response
      : undefined;

  if (response?.status === 413) return messages.requestTooLarge;

  const detail = responseDetail(response?.data);
  if (detail) return detail;
  if (error instanceof Error && error.message.trim()) return error.message.trim();
  if (typeof error === "string" && error.trim()) return error.trim();
  return messages.fallback;
}

/** A validation response proves no source record was accepted. */
export function isDefiniteUploadRejection(error: unknown): boolean {
  const response = error && typeof error === "object"
    ? (error as UploadErrorEnvelope).response
    : undefined;
  return [413, 422].includes(response?.status ?? -1);
}

export function listFailedSources(
  files: PendingFile[],
  urls: PendingUrl[],
  fallback: string
): FailedSourceSummary[] {
  return [
    ...files
      .filter((file) => file.status === "error")
      .map((file) => ({
        key: `file:${file.id}`,
        name: file.name,
        error: file.error || fallback,
      })),
    ...urls
      .filter((url) => url.status === "error")
      .map((url) => ({
        key: `url:${url.id}`,
        name: url.title || url.url,
        error: url.error || fallback,
      })),
  ];
}
