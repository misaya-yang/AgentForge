/**
 * File Upload API Client
 *
 * API for uploading and managing files for AI analysis.
 */

import { api } from "@/lib/api";

// ============================================================
// Types
// ============================================================

export interface FileUploadResponse {
  file_id: string;
  file_path: string;
  filename: string;
  size_bytes: number;
  mime_type: string;
  uploaded_at: string;
  message: string;
}

export interface FileInfo {
  file_id: string;
  file_path: string;
  filename: string;
  size_bytes: number;
  mime_type: string;
  uploaded_at: string;
}

export interface FilesListResponse {
  files: FileInfo[];
  total: number;
}

// ============================================================
// File Upload APIs
// ============================================================






// ============================================================
// Image Compression
// ============================================================

/**
 * Image compression options.
 * Optimized for multimodal AI models which internally downsample to 224-336px.
 */
export interface ImageCompressionOptions {
  /** Max file size in MB (default: 2MB, DashScope limit is 3MB) */
  maxSizeMB?: number;
  /** Max width or height in pixels (default: 2048, sufficient for VLM models) */
  maxWidthOrHeight?: number;
  /** Initial quality 0-1 (default: 0.85, visually lossless) */
  initialQuality?: number;
  /** Use Web Worker for non-blocking compression (default: false; production CSP blocks CDN worker fallbacks) */
  useWebWorker?: boolean;
}

const DEFAULT_COMPRESSION_OPTIONS: Required<ImageCompressionOptions> = {
  maxSizeMB: 2,
  maxWidthOrHeight: 2048,
  initialQuality: 0.85,
  useWebWorker: false,
};

/**
 * Compress an image file before upload.
 * Only compresses if file is larger than maxSizeMB.
 *
 * @param file - The image file to compress
 * @param options - Compression options
 * @returns Compressed file (or original if no compression needed)
 */
async function compressImage(
  file: File,
  options?: ImageCompressionOptions
): Promise<File> {
  // Only compress images
  if (!file.type.startsWith("image/")) {
    return file;
  }

  const opts = { ...DEFAULT_COMPRESSION_OPTIONS, ...options };

  // Skip compression for small files
  if (file.size <= opts.maxSizeMB * 1024 * 1024) {
    return file;
  }

  try {
    // Load the codec only for an image that actually needs compression. Keep
    // chunk failures inside the existing original-file upload fallback.
    const { default: imageCompression } = await import("browser-image-compression");
    const compressed = await imageCompression(file, {
      maxSizeMB: opts.maxSizeMB,
      maxWidthOrHeight: opts.maxWidthOrHeight,
      initialQuality: opts.initialQuality,
      useWebWorker: opts.useWebWorker,
    });

    // Return as File with original name
    return new File([compressed], file.name, { type: compressed.type });
  } catch {
    return file;
  }
}

// ============================================================
// Upload with Progress
// ============================================================

export interface UploadProgressEvent {
  loaded: number;
  total: number;
  percent: number;
}

/**
 * Upload a file with progress callback.
 *
 * @param file - The file to upload
 * @param onProgress - Progress callback (0-100%)
 * @param compressImages - Whether to compress images before upload (default: true)
 * @returns Upload response
 */
export async function uploadFileWithProgress(
  file: File,
  onProgress?: (event: UploadProgressEvent) => void,
  compressImages = true,
  signal?: AbortSignal
): Promise<FileUploadResponse> {
  signal?.throwIfAborted();
  // Optionally compress images
  const fileToUpload = compressImages ? await compressImage(file) : file;

  signal?.throwIfAborted();
  const formData = new FormData();
  formData.append("file", fileToUpload);

  const { data } = await api.post<FileUploadResponse>(
    "/api/v1/files/upload",
    formData,
    {
      signal,
      headers: {
        "Content-Type": "multipart/form-data",
      },
      onUploadProgress: (progressEvent) => {
        if (onProgress && progressEvent.total) {
          const percent = Math.round(
            (progressEvent.loaded * 100) / progressEvent.total
          );
          onProgress({
            loaded: progressEvent.loaded,
            total: progressEvent.total,
            percent,
          });
        }
      },
    }
  );
  return data;
}

// ============================================================
// Utility Functions
// ============================================================

/**
 * Check if a file type is supported for upload.
 */
export function isFileTypeSupported(file: File): boolean {
  const supportedExtensions = [
    ".pdf",
    ".docx",
    ".md",
    ".txt",
    ".csv",
    ".xlsx",
    ".png",
    ".jpg",
    ".jpeg",
    ".gif",
    ".webp",
  ];

  const ext = "." + file.name.split(".").pop()?.toLowerCase();
  return supportedExtensions.includes(ext);
}

/**
 * Check if a file is an image.
 */
export function isImageFile(file: File): boolean {
  return file.type.startsWith("image/");
}


// Re-export from shared format utilities for backward compatibility.
export { formatFileSize } from "@/lib/format";
