import { useCallback, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import type { ContentItem } from "@/types/gateway";
import {
  Paperclip,
  Send,
  X,
  FileText,
  Image as ImageIcon,
  Loader2,
  CheckCircle2,
  AlertCircle,
} from "lucide-react";
import { cn } from "@/lib/utils";
import {
  uploadFileWithProgress,
  isFileTypeSupported,
  isImageFile,
  formatFileSize,
  type FileUploadResponse,
} from "@/api/files";
import { usePasteImage } from "@/hooks/usePasteImage";

interface UploadedFile {
  file: File;
  status: "pending" | "compressing" | "uploading" | "success" | "error";
  response?: FileUploadResponse;
  error?: string;
  /** Upload progress 0-100 */
  progress?: number;
}

export function MultimodalInput({
  onSend,
  onStop,
  onTextChange,
  isStreaming = false,
  composerId,
  disabled,
  includeFiles = true,
}: {
  onSend: (inputs: ContentItem[], filePaths?: string[]) => void;
  onStop?: () => void;
  onTextChange?: (text: string) => void;
  isStreaming?: boolean;
  composerId?: string;
  disabled?: boolean;
  includeFiles?: boolean;
}) {
  const { t } = useTranslation();
  const [text, setText] = useState("");
  const [files, setFiles] = useState<UploadedFile[]>([]);
  const [isUploading, setIsUploading] = useState(false);
  const fileRef = useRef<HTMLInputElement | null>(null);

  // Upload files asynchronously with compression and progress
  const uploadFiles = useCallback(async (filesToUpload: UploadedFile[]) => {
    if (filesToUpload.length === 0) return;

    setIsUploading(true);

    // Upload each file
    for (const fileEntry of filesToUpload) {
      // For images, show compressing status first
      const isImage = isImageFile(fileEntry.file);
      if (isImage && fileEntry.file.size > 2 * 1024 * 1024) {
        setFiles((prev) =>
          prev.map((f) =>
            f.file === fileEntry.file
              ? { ...f, status: "compressing", progress: 0 }
              : f
          )
        );
      }

      // Update status to uploading
      setFiles((prev) =>
        prev.map((f) =>
          f.file === fileEntry.file
            ? { ...f, status: "uploading", progress: 0 }
            : f
        )
      );

      try {
        // Use uploadFileWithProgress which handles compression internally
        const response = await uploadFileWithProgress(
          fileEntry.file,
          (event) => {
            // Update progress
            setFiles((prev) =>
              prev.map((f) =>
                f.file === fileEntry.file
                  ? { ...f, progress: event.percent }
                  : f
              )
            );
          },
          true // Enable image compression
        );

        // Update with success
        setFiles((prev) =>
          prev.map((f) =>
            f.file === fileEntry.file
              ? { ...f, status: "success", response, progress: 100 }
              : f
          )
        );
      } catch (error) {
        // Update with error
        setFiles((prev) =>
          prev.map((f) =>
            f.file === fileEntry.file
              ? {
                  ...f,
                  status: "error",
                  error:
                    error instanceof Error ? error.message : "Upload failed",
                }
              : f
          )
        );
      }
    }

    setIsUploading(false);
  }, []);

  // Handle file selection and upload
  const handleFileSelect = useCallback(
    async (selectedFiles: FileList | null) => {
      if (!selectedFiles || selectedFiles.length === 0) return;

      // Calculate remaining slots BEFORE processing
      const MAX_FILES = 5;

      setFiles((currentFiles) => {
        const remainingSlots = MAX_FILES - currentFiles.length;

        if (remainingSlots <= 0) {
          console.warn("Maximum file limit reached (5 files)");
          return currentFiles;
        }

        // Filter supported files and limit to remaining slots
        const validFiles = Array.from(selectedFiles)
          .filter((file) => {
            if (!isFileTypeSupported(file)) {
              console.warn(`Unsupported file type: ${file.name}`);
              return false;
            }
            return true;
          })
          .slice(0, remainingSlots);

        if (validFiles.length === 0) return currentFiles;

        // Add files to state with pending status
        const newFiles: UploadedFile[] = validFiles.map((file) => ({
          file,
          status: "pending" as const,
        }));

        // Start uploads outside of setState (side effect)
        setTimeout(() => uploadFiles(newFiles), 0);

        return [...currentFiles, ...newFiles];
      });
    },
    [uploadFiles]
  );

  // Handle paste event for images (shared hook)
  const handlePaste = usePasteImage(handleFileSelect);

  // Remove file from list
  const removeFile = useCallback((index: number) => {
    setFiles((prev) => prev.filter((_, i) => i !== index));
  }, []);

  // Handle send
  async function handleSend() {
    const inputs: ContentItem[] = [];
    const uploadedPaths: string[] = [];

    // Get successfully uploaded files
    const successfulUploads = files.filter(
      (f) => f.status === "success" && f.response
    );

    // Add text content with file references if files were uploaded
    if (text.trim()) {
      let messageText = text.trim();

      // Append file paths to message for agent to understand
      if (successfulUploads.length > 0) {
        const fileRefs = successfulUploads
          .map((f) => f.response!.file_path)
          .join(", ");
        messageText += `\n\n[${t("assistant.attachments.notice", { files: fileRefs })}]`;
        uploadedPaths.push(...successfulUploads.map((f) => f.response!.file_path));
      }

      inputs.push({ type: "text", data: messageText });
    } else if (successfulUploads.length > 0) {
      // If no text but files uploaded, create a message about the files
      const fileRefs = successfulUploads
        .map((f) => f.response!.file_path)
        .join(", ");
      inputs.push({
        type: "text",
        data: t("assistant.attachments.analyze", { files: fileRefs }),
      });
      uploadedPaths.push(...successfulUploads.map((f) => f.response!.file_path));
    }

    // Add file content items for display purposes
    for (const upload of successfulUploads) {
      const isImage = isImageFile(upload.file);
      inputs.push({
        type: isImage ? "image" : "file",
        url: upload.response!.file_path,
        mime_type: upload.file.type,
        metadata: {
          filename: upload.file.name,
          file_path: upload.response!.file_path,
          file_id: upload.response!.file_id,
        },
      });
    }

    onSend(inputs, uploadedPaths);
    setText("");
    setFiles([]);
  }

  const hasUploadedFiles = files.some(
    (f) => f.status === "success" && f.response
  );
  const hasFailedUploads = files.some((f) => f.status === "error");
  const canStop = isStreaming && typeof onStop === "function";
  const canSend =
    !disabled && !isUploading && !isStreaming && (text.trim() || hasUploadedFiles);

  return (
    <div className="w-full">
      {/* File Preview Area */}
      {files.length > 0 && (
        <div className="px-4 pt-3 pb-2 flex flex-wrap gap-2 border-b border-border/50">
          {files.map((f, index) => (
            <div
              key={`${f.file.name}-${index}`}
              className={cn(
                "relative group rounded-lg px-3 py-2 text-xs flex items-center gap-2 transition-colors overflow-hidden",
                f.status === "error"
                  ? "bg-red-100 dark:bg-red-900/30 text-red-600 dark:text-red-400"
                  : f.status === "compressing"
                    ? "bg-amber-100 dark:bg-amber-900/30 text-amber-600 dark:text-amber-400"
                    : f.status === "uploading"
                      ? "bg-blue-100 dark:bg-blue-900/30 text-blue-600 dark:text-blue-400"
                      : f.status === "success"
                        ? "bg-green-100 dark:bg-green-900/30 text-green-600 dark:text-green-400"
                        : "bg-muted text-muted-foreground"
              )}
            >
              {/* Progress bar background */}
              {(f.status === "uploading" || f.status === "compressing") && f.progress !== undefined && (
                <div
                  className="absolute inset-0 bg-current opacity-10 transition-all duration-300"
                  style={{ width: `${f.progress}%` }}
                />
              )}

              {/* Icon */}
              {isImageFile(f.file) ? (
                <ImageIcon className="h-4 w-4 shrink-0 relative z-10" />
              ) : (
                <FileText className="h-4 w-4 shrink-0 relative z-10" />
              )}

              {/* Filename and size/progress */}
              <div className="flex flex-col min-w-0 relative z-10">
                <span className="truncate max-w-[120px]">{f.file.name}</span>
                <span className="text-[10px] opacity-70">
                  {f.status === "compressing"
                    ? t("assistant.upload.compressing")
                    : f.status === "uploading" && f.progress !== undefined
                      ? `${f.progress}%`
                      : formatFileSize(f.file.size)}
                </span>
              </div>

              {/* Status indicator */}
              <div className="shrink-0 relative z-10">
                {(f.status === "uploading" || f.status === "compressing") && (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                )}
                {f.status === "success" && (
                  <CheckCircle2 className="h-3.5 w-3.5" />
                )}
                {f.status === "error" && (
                  <AlertCircle className="h-3.5 w-3.5" />
                )}
              </div>

              {/* Remove button */}
              <button
                type="button"
                onClick={() => removeFile(index)}
                className="absolute -right-1.5 -top-1.5 z-20 rounded-full bg-foreground/80 p-1 text-background opacity-70 transition-opacity hover:bg-foreground hover:opacity-100 focus-visible:opacity-100 sm:opacity-0 sm:group-focus-within:opacity-100 sm:group-hover:opacity-100"
                disabled={f.status === "uploading" || f.status === "compressing"}
                aria-label={`${t("common.delete")}: ${f.file.name}`}
              >
                <X className="h-3 w-3" />
              </button>
            </div>
          ))}
        </div>
      )}

      {/* Error message */}
      {hasFailedUploads && (
        <div className="px-4 py-2 text-xs text-red-500 dark:text-red-400" role="alert">
          {t("assistant.upload.failedNotice")}
        </div>
      )}

      {/* Input Area */}
      <div className="flex items-end gap-2 p-3">
        <input
          ref={fileRef}
          type="file"
          multiple
          accept=".pdf,.docx,.doc,.md,.txt,.csv,.xlsx,.png,.jpg,.jpeg,.gif,.webp,.bmp"
          className="hidden"
          disabled={disabled || isUploading}
          onChange={(e) => {
            handleFileSelect(e.target.files);
            // Reset input so same file can be selected again
            e.target.value = "";
          }}
        />

        {/* Attach Button */}
        {includeFiles && (
          <Button
            variant="ghost"
            size="icon"
            type="button"
            disabled={disabled || isUploading || files.length >= 5}
            onClick={() => fileRef.current?.click()}
            aria-label={t("common.addAttachment")}
            className={cn(
              "h-10 w-10 shrink-0 rounded-xl transition-colors",
              files.length > 0
                ? "text-primary hover:text-primary/80 bg-primary/10 dark:bg-primary/20"
                : "text-muted-foreground hover:text-foreground hover:bg-muted/60"
            )}
          >
            <Paperclip className="h-5 w-5" />
          </Button>
        )}

        {/* Text Input */}
        <Textarea
          id={composerId}
          placeholder={
            files.length > 0 ? t("assistant.attachments.placeholder") : t("assistant.inputPlaceholder")
          }
          aria-label={t("playground.composerAriaLabel", "Playground message composer")}
          value={text}
          onChange={(e) => { setText(e.target.value); onTextChange?.(e.target.value); }}
          rows={1}
          disabled={disabled}
          className="min-h-[44px] max-h-[200px] resize-none border-0 bg-transparent focus-visible:ring-0 text-base placeholder:text-muted-foreground/60"
          onKeyDown={(e) => {
            if (e.key === "Escape" && canStop) {
              e.preventDefault();
              onStop?.();
              return;
            }
            const isSubmitShortcut =
              (e.key === "Enter" && !e.shiftKey) ||
              (e.key === "Enter" && (e.metaKey || e.ctrlKey));
            if (isSubmitShortcut && canSend) {
              e.preventDefault();
              handleSend();
            }
          }}
          onPaste={includeFiles ? handlePaste : undefined}
        />

        {/* Send Button */}
        {canStop ? (
          <Button
            size="icon"
            type="button"
            onClick={() => onStop?.()}
            aria-label={t("assistant.stopGenerating", "Stop generating")}
            aria-keyshortcuts="Escape"
            className="h-10 w-10 shrink-0 rounded-lg bg-destructive text-destructive-foreground transition-colors hover:bg-destructive/90"
          >
            <X className="h-5 w-5" />
          </Button>
        ) : (
          <Button
            size="icon"
            type="button"
            disabled={!canSend}
            onClick={handleSend}
            aria-label={t("common.send")}
            aria-keyshortcuts="Enter,Control+Enter,Meta+Enter"
            className={cn(
              "h-10 w-10 shrink-0 rounded-lg transition-colors duration-150",
              canSend
                ? "bg-primary text-primary-foreground hover:bg-primary/90"
                : "bg-muted text-muted-foreground opacity-50"
            )}
          >
            {isUploading ? (
              <Loader2 className="h-5 w-5 animate-spin" />
            ) : (
              <Send className="h-5 w-5" />
            )}
          </Button>
        )}
      </div>
    </div>
  );
}
