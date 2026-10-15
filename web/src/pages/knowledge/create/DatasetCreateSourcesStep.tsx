import { useRef } from "react";
import { useTranslation } from "react-i18next";
import { Check, FileText, Link, Loader2, Trash2, Upload } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import type {
  PendingFile,
  PendingUrl,
} from "@/pages/knowledge/create/datasetCreateModel";

interface DatasetCreateSourcesStepProps {
  pendingFiles: PendingFile[];
  pendingUrls: PendingUrl[];
  urlInput: string;
  urlTitle: string;
  onFilesSelect: (files: FileList | null) => void;
  onRemoveFile: (id: string) => void;
  onUrlInputChange: (value: string) => void;
  onUrlTitleChange: (value: string) => void;
  onAddUrl: () => void;
  onRemoveUrl: (id: string) => void;
}

export function DatasetCreateSourcesStep({
  pendingFiles,
  pendingUrls,
  urlInput,
  urlTitle,
  onFilesSelect,
  onRemoveFile,
  onUrlInputChange,
  onUrlTitleChange,
  onAddUrl,
  onRemoveUrl,
}: DatasetCreateSourcesStepProps) {
  const { t } = useTranslation();
  const fileInputRef = useRef<HTMLInputElement>(null);

  return (
    <div className="space-y-6">
      <div className="p-4 bg-blue-50 dark:bg-blue-950/30 border border-blue-200 dark:border-blue-900 rounded-lg">
        <p className="text-sm text-blue-700 dark:text-blue-300">
          {t("knowledge.create.optionalHint")}
        </p>
      </div>

      <div
        className="border-2 border-dashed border-border rounded-lg p-8 text-center hover:border-primary/40 transition cursor-pointer"
        role="button"
        tabIndex={0}
        aria-label={t("knowledge.create.uploadFiles")}
        onClick={() => fileInputRef.current?.click()}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            fileInputRef.current?.click();
          }
        }}
        onDragOver={(event) => {
          event.preventDefault();
          event.stopPropagation();
        }}
        onDrop={(event) => {
          event.preventDefault();
          event.stopPropagation();
          onFilesSelect(event.dataTransfer.files);
        }}
      >
        <Upload className="h-10 w-10 mx-auto text-muted-foreground/70" />
        <p className="mt-3 text-sm font-medium text-foreground/80">
          {t("knowledge.create.uploadFiles")}
        </p>
        <p className="text-xs text-muted-foreground mt-1">
          {t("knowledge.create.supportedFormats")}
        </p>
        <p className="text-xs text-muted-foreground/70 mt-1">
          {t("knowledge.create.fileSizeLimit")}
        </p>
        <input
          ref={fileInputRef}
          type="file"
          multiple
          className="hidden"
          accept=".pdf,.docx,.txt,.md,.html"
          onChange={(event) => {
            onFilesSelect(event.target.files);
            event.currentTarget.value = "";
          }}
        />
      </div>

      {pendingFiles.length > 0 && (
        <div className="space-y-2">
          {pendingFiles.map((pendingFile) => (
            <div
              key={pendingFile.id}
              className="flex items-start justify-between gap-3 p-3 bg-card rounded-lg border"
            >
              <div className="flex min-w-0 items-center gap-3">
                <div className="p-2 bg-red-500/10 dark:bg-red-500/15 rounded">
                  <FileText className="h-5 w-5 text-red-500" />
                </div>
                <div>
                  <p className="break-words text-sm font-medium text-foreground">
                    {pendingFile.name}
                  </p>
                  <p className="text-xs text-muted-foreground">
                    {(pendingFile.size / 1024).toFixed(1)} KB
                  </p>
                </div>
              </div>
              <div className="flex items-center gap-2">
                {pendingFile.status === "uploading" && (
                  <Loader2 className="h-4 w-4 animate-spin text-primary" />
                )}
                {pendingFile.status === "done" && (
                  <Check className="h-4 w-4 text-green-500" />
                )}
                {pendingFile.status === "error" && (
                  <span className="max-w-xs break-words text-right text-xs text-red-500">
                    {pendingFile.error}
                  </span>
                )}
                {(pendingFile.status === "pending" || pendingFile.status === "error") && (
                  <button
                    onClick={() => onRemoveFile(pendingFile.id)}
                    className="p-1 hover:bg-secondary/60 rounded"
                  >
                    <Trash2 className="h-4 w-4 text-muted-foreground/70" />
                  </button>
                )}
              </div>
            </div>
          ))}
        </div>
      )}

      <div className="pt-4 border-t">
        <Label className="text-sm font-medium">{t("knowledge.create.addUrl")}</Label>
        <div className="mt-2 flex gap-2">
          <div className="flex-1 space-y-2">
            <Input
              placeholder={t("knowledge.create.urlPlaceholder")}
              value={urlInput}
              onChange={(event) => onUrlInputChange(event.target.value)}
            />
            <Input
              placeholder={t("knowledge.create.urlTitle")}
              value={urlTitle}
              onChange={(event) => onUrlTitleChange(event.target.value)}
            />
          </div>
          <Button
            variant="outline"
            onClick={onAddUrl}
            disabled={!urlInput.trim()}
            className="self-start"
          >
            <Link className="h-4 w-4 mr-1" />
            {t("knowledge.create.addButton")}
          </Button>
        </div>
      </div>

      {pendingUrls.length > 0 && (
        <div className="space-y-2">
          {pendingUrls.map((pendingUrl) => (
            <div
              key={pendingUrl.id}
              className="flex items-start justify-between gap-3 p-3 bg-card rounded-lg border"
            >
              <div className="flex items-center gap-3">
                <div className="p-2 bg-primary/5 rounded">
                  <Link className="h-5 w-5 text-primary" />
                </div>
                <div>
                  <p className="text-sm font-medium text-foreground">{pendingUrl.title}</p>
                  <p className="text-xs text-muted-foreground truncate max-w-[300px]">
                    {pendingUrl.url}
                  </p>
                </div>
              </div>
              <div className="flex items-center gap-2">
                {pendingUrl.status === "uploading" && (
                  <Loader2 className="h-4 w-4 animate-spin text-primary" />
                )}
                {pendingUrl.status === "done" && (
                  <Check className="h-4 w-4 text-green-500" />
                )}
                {pendingUrl.status === "error" && (
                  <span className="max-w-xs break-words text-right text-xs text-red-500">
                    {pendingUrl.error}
                  </span>
                )}
                {(pendingUrl.status === "pending" || pendingUrl.status === "error") && (
                  <button
                    onClick={() => onRemoveUrl(pendingUrl.id)}
                    className="p-1 hover:bg-secondary/60 rounded"
                  >
                    <Trash2 className="h-4 w-4 text-muted-foreground/70" />
                  </button>
                )}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
