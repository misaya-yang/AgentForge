import { useTranslation } from "react-i18next";
import { ChevronDown, FileText, Upload, X } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
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
import { Switch } from "@/components/ui/switch";
import type { ChunkingMode, Dataset } from "@/types/knowledge";
import {
  DATASET_EMBEDDING_MODELS,
  useDatasetUploadController,
} from "@/pages/knowledge/detail/useDatasetUploadController";

interface DatasetUploadDialogProps {
  datasetId?: string;
  dataset?: Dataset;
  open: boolean;
  pendingFiles: File[];
  uploading: boolean;
  onOpenChange: (open: boolean) => void;
  onPendingFilesChange: (files: File[]) => void;
  onFilesSelected: (files?: FileList | null) => void;
  onUploadingChange: (uploading: boolean) => void;
  onOpenFilePicker: () => void;
}

export function DatasetUploadDialog({
  datasetId,
  dataset,
  open,
  pendingFiles,
  uploading,
  onOpenChange,
  onPendingFilesChange,
  onFilesSelected,
  onUploadingChange,
  onOpenFilePicker,
}: DatasetUploadDialogProps) {
  const { t } = useTranslation();
  const {
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
    recoveryReceipts,
    recoveryFailed,
    refreshUploadRecovery,
    clearUploadRecovery,
    uploadFailures,
    clearUploadFailures,
    handleConfirmUpload,
  } = useDatasetUploadController({
    datasetId,
    dataset,
    pendingFiles,
    onOpenChange,
    onPendingFilesChange,
    onUploadingChange,
  });

  function handleDialogOpenChange(nextOpen: boolean) {
    if (!nextOpen && uploading) return;
    onOpenChange(nextOpen);
    if (!nextOpen) {
      onPendingFilesChange([]);
      clearUploadFailures();
    }
  }

  return (
    <>
      {(recoveryReceipts.length > 0 || recoveryFailed) && (
        <section data-testid="upload-recovery-receipts" role="status" className="rounded-lg border border-border bg-card p-4 text-sm space-y-2">
          <p className="font-medium">{t("knowledge.detail.uploadRecoveryTitle")}</p>
          <p className="text-muted-foreground">{t("knowledge.detail.uploadRecoveryHint")}</p>
          {recoveryFailed && <p role="alert">{t("knowledge.detail.uploadRecoveryUnavailable")}</p>}
          <div className="max-h-64 overflow-y-auto space-y-3">
            {recoveryReceipts.map((receipt) => (
              <div key={receipt.id}>
                <p className="text-xs text-muted-foreground">{new Date(receipt.createdAt).toLocaleString()}</p>
                <ul>
                  {receipt.files.map((file) => (
                    <li key={file.position}>
                      #{file.position} {file.name} ({(file.sizeBytes / 1024).toFixed(1)} KB): {t(`knowledge.detail.uploadRecoveryState.${file.state}`)}
                      {file.documentId && <> ({file.documentId})</>}
                    </li>
                  ))}
                </ul>
              </div>
            ))}
          </div>
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" size="sm" onClick={() => void refreshUploadRecovery()}>{t("knowledge.detail.uploadRecoveryRefresh")}</Button>
            <Button variant="outline" size="sm" disabled={uploading} onClick={() => {
              if (window.confirm(t("knowledge.detail.uploadRecoveryClearConfirm"))) clearUploadRecovery();
            }}>{t("knowledge.detail.uploadRecoveryClear")}</Button>
          </div>
        </section>
      )}
    <Dialog open={open} onOpenChange={handleDialogOpenChange}>
      <DialogContent className="max-w-4xl bg-card max-h-[90vh] overflow-hidden flex flex-col">
        <DialogHeader className="shrink-0">
          <DialogTitle className="text-xl font-semibold">
            {t("knowledge.detail.uploadDialogTitle")}
          </DialogTitle>
        </DialogHeader>

        <div className="flex-1 overflow-y-auto space-y-4 pr-1">
          <div className="flex gap-4">
            <div
              className="shrink-0 w-48 border-2 border-dashed border-border rounded-lg p-4 text-center hover:border-primary/40 transition-colors cursor-pointer bg-muted/40"
              onClick={onOpenFilePicker}
              onDragOver={(event) => {
                event.preventDefault();
                event.stopPropagation();
              }}
              onDrop={(event) => {
                event.preventDefault();
                event.stopPropagation();
                onFilesSelected(event.dataTransfer.files);
              }}
            >
              <Upload className="h-8 w-8 mx-auto text-muted-foreground/70 mb-2" />
              <p className="text-sm text-muted-foreground">
                {t("knowledge.detail.clickOrDragUpload")}
              </p>
              <p className="text-xs text-muted-foreground/70 mt-1">{t("knowledge.upload.supportedFormats")}</p>
            </div>

            <div className="flex-1 min-w-0">
              <Label className="text-sm font-medium text-foreground/80">
                {t("knowledge.detail.selectedFiles", { count: pendingFiles.length })}
              </Label>
              <div className="mt-2 flex flex-wrap gap-2 max-h-24 overflow-auto">
                {pendingFiles.map((file, index) => (
                  <Badge
                    key={index}
                    variant="secondary"
                    className="flex items-center gap-1 py-1 px-2 max-w-[200px]"
                  >
                    <FileText className="h-3 w-3 shrink-0" />
                    <span className="truncate text-xs">{file.name}</span>
                    <button
                      onClick={(event) => {
                        event.stopPropagation();
                        onPendingFilesChange(
                          pendingFiles.filter((_, fileIndex) => fileIndex !== index)
                        );
                      }}
                      className="ml-1 hover:text-red-600"
                    >
                      <X className="h-3 w-3" />
                    </button>
                  </Badge>
                ))}
                {pendingFiles.length === 0 && (
                  <p className="text-sm text-muted-foreground/70">
                    {t("knowledge.detail.selectFilesHint")}
                  </p>
                )}
              </div>
            </div>
          </div>

          <div className="border rounded-lg p-4 bg-primary/5">
            <Label className="text-sm font-medium text-foreground mb-3 block">
              {t("knowledge.detail.processingMode")}
            </Label>
            <div className="rounded-lg border border-primary bg-primary/10 p-3 ring-2 ring-primary/50">
              <div className="mb-1 flex items-center gap-2">
                <span className="text-lg">📝</span>
                <h4 className="text-sm font-medium">
                  {t("knowledge.detail.processingModes.text_only")}
                  <span className="ml-1 rounded bg-green-500/20 px-1.5 py-0.5 text-[10px] text-green-700 dark:text-green-400">
                    {t("knowledge.detail.recommended")}
                  </span>
                </h4>
              </div>
              <p className="text-xs text-muted-foreground">
                {t("knowledge.detail.processingModes.text_onlyDesc")}
              </p>
            </div>
          </div>

          <div className="border rounded-lg p-4">
            <Label className="text-sm font-medium text-foreground mb-3 block">
              {t("knowledge.detail.chunkingMethod")}
            </Label>
            <div className="grid grid-cols-3 gap-2 mb-4">
              {[
                {
                  id: "automatic",
                  name: t("knowledge.detail.uploadChunkModes.automatic"),
                  desc: t("knowledge.detail.uploadChunkModes.automaticDesc"),
                },
                {
                  id: "fixed_size",
                  name: t("knowledge.detail.uploadChunkModes.fixed_size"),
                  desc: t("knowledge.detail.uploadChunkModes.fixed_sizeDesc"),
                },
                {
                  id: "paragraph",
                  name: t("knowledge.detail.uploadChunkModes.paragraph"),
                  desc: t("knowledge.detail.uploadChunkModes.paragraphDesc"),
                },
                {
                  id: "heading",
                  name: t("knowledge.detail.uploadChunkModes.heading"),
                  desc: t("knowledge.detail.uploadChunkModes.headingDesc"),
                },
                {
                  id: "hierarchical",
                  name: t("knowledge.detail.uploadChunkModes.hierarchical"),
                  desc: t("knowledge.detail.uploadChunkModes.hierarchicalDesc"),
                },
                {
                  id: "separator",
                  name: t("knowledge.detail.uploadChunkModes.separator"),
                  desc: t("knowledge.detail.uploadChunkModes.separatorDesc"),
                },
                {
                  id: "regex",
                  name: t("knowledge.detail.uploadChunkModes.regex"),
                  desc: t("knowledge.detail.uploadChunkModes.regexDesc"),
                },
                {
                  id: "recursive",
                  name: t("knowledge.detail.uploadChunkModes.recursive"),
                  desc: t("knowledge.detail.uploadChunkModes.recursiveDesc"),
                },
                {
                  id: "qa",
                  name: t("knowledge.detail.uploadChunkModes.qa"),
                  desc: t("knowledge.detail.uploadChunkModes.qaDesc"),
                },
              ].map((mode) => (
                <div
                  key={mode.id}
                  className={`p-3 rounded-lg border cursor-pointer transition-[border-color,background-color,box-shadow] ${
                    uploadChunkMode === mode.id
                      ? "border-primary bg-primary/5 ring-1 ring-primary/50"
                      : "border-border hover:border-border"
                  }`}
                  onClick={() => setUploadChunkMode(mode.id as ChunkingMode)}
                >
                  <p className="text-sm font-medium text-foreground">{mode.name}</p>
                  <p className="text-xs text-muted-foreground mt-0.5">{mode.desc}</p>
                </div>
              ))}
            </div>

            <div className="bg-muted/40 rounded-lg p-4">
              {uploadChunkMode === "automatic" && (
                <p className="text-sm text-muted-foreground">
                  {t("knowledge.detail.autoModeHint")}
                </p>
              )}

              {uploadChunkMode === "fixed_size" && (
                <div className="space-y-4">
                  <div>
                    <div className="flex justify-between items-center mb-2">
                      <Label className="text-sm">
                        {t("knowledge.detail.chunkSizeLabel")}
                      </Label>
                      <span className="text-sm font-medium text-primary">{uploadChunkSize}</span>
                    </div>
                    <input
                      type="range"
                      min={100}
                      max={2000}
                      step={50}
                      value={uploadChunkSize}
                      onChange={(event) => setUploadChunkSize(Number(event.target.value))}
                      className="w-full h-2 bg-border rounded-lg appearance-none cursor-pointer accent-primary"
                    />
                  </div>
                  <div>
                    <div className="flex justify-between items-center mb-2">
                      <Label className="text-sm">
                        {t("knowledge.detail.overlapSizeLabel")}
                      </Label>
                      <span className="text-sm font-medium text-primary">
                        {uploadChunkOverlap}
                      </span>
                    </div>
                    <input
                      type="range"
                      min={0}
                      max={500}
                      step={10}
                      value={uploadChunkOverlap}
                      onChange={(event) => setUploadChunkOverlap(Number(event.target.value))}
                      className="w-full h-2 bg-border rounded-lg appearance-none cursor-pointer accent-primary"
                    />
                  </div>
                </div>
              )}

              {uploadChunkMode === "paragraph" && (
                <div className="space-y-4">
                  <div>
                    <div className="flex justify-between items-center mb-2">
                      <Label className="text-sm">
                        {t("knowledge.detail.maxChunkSizeLabel")}
                      </Label>
                      <span className="text-sm font-medium text-primary">{uploadChunkSize}</span>
                    </div>
                    <input
                      type="range"
                      min={200}
                      max={2000}
                      step={50}
                      value={uploadChunkSize}
                      onChange={(event) => setUploadChunkSize(Number(event.target.value))}
                      className="w-full h-2 bg-border rounded-lg appearance-none cursor-pointer accent-primary"
                    />
                  </div>
                  <div>
                    <div className="flex justify-between items-center mb-2">
                      <Label className="text-sm">
                        {t("knowledge.detail.minParagraphLength")}
                      </Label>
                      <span className="text-sm font-medium text-primary">
                        {uploadMinParagraphLength}
                      </span>
                    </div>
                    <input
                      type="range"
                      min={20}
                      max={200}
                      step={10}
                      value={uploadMinParagraphLength}
                      onChange={(event) =>
                        setUploadMinParagraphLength(Number(event.target.value))
                      }
                      className="w-full h-2 bg-border rounded-lg appearance-none cursor-pointer accent-primary"
                    />
                  </div>
                  <div className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      id="merge-short"
                      checked={uploadMergeShortParagraphs}
                      onChange={(event) => setUploadMergeShortParagraphs(event.target.checked)}
                      className="w-4 h-4 rounded text-primary"
                    />
                    <Label htmlFor="merge-short" className="text-sm cursor-pointer">
                      {t("knowledge.detail.mergeShortParagraphs")}
                    </Label>
                  </div>
                </div>
              )}

              {uploadChunkMode === "heading" && (
                <div className="space-y-4">
                  <div>
                    <Label className="text-sm mb-2 block">
                      {t("knowledge.detail.headingLevel")}
                    </Label>
                    <Select
                      value={uploadHeadingLevel}
                      onValueChange={(value) =>
                        setUploadHeadingLevel(value as "h1" | "h2" | "h3")
                      }
                    >
                      <SelectTrigger>
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="h1">{t("knowledge.detail.h1Title")}</SelectItem>
                        <SelectItem value="h2">{t("knowledge.detail.h2Title")}</SelectItem>
                        <SelectItem value="h3">{t("knowledge.detail.h3Title")}</SelectItem>
                      </SelectContent>
                    </Select>
                  </div>
                  <div>
                    <div className="flex justify-between items-center mb-2">
                      <Label className="text-sm">
                        {t("knowledge.detail.maxChunkSizeLabel")}
                      </Label>
                      <span className="text-sm font-medium text-primary">{uploadChunkSize}</span>
                    </div>
                    <input
                      type="range"
                      min={200}
                      max={3000}
                      step={100}
                      value={uploadChunkSize}
                      onChange={(event) => setUploadChunkSize(Number(event.target.value))}
                      className="w-full h-2 bg-border rounded-lg appearance-none cursor-pointer accent-primary"
                    />
                  </div>
                </div>
              )}

              {uploadChunkMode === "hierarchical" && (
                <div className="space-y-4">
                  <div className="p-3 bg-primary/5 rounded-lg mb-2">
                    <p className="text-sm text-primary/90">
                      {t("knowledge.detail.hierarchicalHint")}
                    </p>
                  </div>
                  <div>
                    <div className="flex justify-between items-center mb-2">
                      <Label className="text-sm">
                        {t("knowledge.detail.parentSizeLabel")}
                      </Label>
                      <span className="text-sm font-medium text-primary">
                        {uploadParentChunkSize}
                      </span>
                    </div>
                    <input
                      type="range"
                      min={500}
                      max={4000}
                      step={100}
                      value={uploadParentChunkSize}
                      onChange={(event) => setUploadParentChunkSize(Number(event.target.value))}
                      className="w-full h-2 bg-border rounded-lg appearance-none cursor-pointer accent-primary"
                    />
                  </div>
                  <div>
                    <div className="flex justify-between items-center mb-2">
                      <Label className="text-sm">
                        {t("knowledge.detail.childSizeLabel")}
                      </Label>
                      <span className="text-sm font-medium text-primary">
                        {uploadChildChunkSize}
                      </span>
                    </div>
                    <input
                      type="range"
                      min={100}
                      max={1000}
                      step={50}
                      value={uploadChildChunkSize}
                      onChange={(event) => setUploadChildChunkSize(Number(event.target.value))}
                      className="w-full h-2 bg-border rounded-lg appearance-none cursor-pointer accent-primary"
                    />
                  </div>
                  <div>
                    <div className="flex justify-between items-center mb-2">
                      <Label className="text-sm">
                        {t("knowledge.detail.childOverlapLabel")}
                      </Label>
                      <span className="text-sm font-medium text-primary">
                        {uploadChildOverlap}
                      </span>
                    </div>
                    <input
                      type="range"
                      min={0}
                      max={200}
                      step={10}
                      value={uploadChildOverlap}
                      onChange={(event) => setUploadChildOverlap(Number(event.target.value))}
                      className="w-full h-2 bg-border rounded-lg appearance-none cursor-pointer accent-primary"
                    />
                  </div>
                </div>
              )}

              {uploadChunkMode === "separator" && (
                <div className="space-y-4">
                  <div>
                    <Label className="text-sm mb-2 block">
                      {t("knowledge.detail.separatorLabel")}
                    </Label>
                    <Input
                      value={uploadSeparator}
                      onChange={(event) => setUploadSeparator(event.target.value)}
                      placeholder={t("knowledge.detail.separatorPlaceholder")}
                    />
                    <p className="text-xs text-muted-foreground mt-1">
                      {t("knowledge.detail.separatorHint")}
                    </p>
                  </div>
                  <div className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      id="keep-sep"
                      checked={uploadKeepSeparator}
                      onChange={(event) => setUploadKeepSeparator(event.target.checked)}
                      className="w-4 h-4 rounded text-primary"
                    />
                    <Label htmlFor="keep-sep" className="text-sm cursor-pointer">
                      {t("knowledge.detail.keepSeparator")}
                    </Label>
                  </div>
                  <div>
                    <div className="flex justify-between items-center mb-2">
                      <Label className="text-sm">
                        {t("knowledge.detail.maxChunkSizeLabel")}
                      </Label>
                      <span className="text-sm font-medium text-primary">{uploadChunkSize}</span>
                    </div>
                    <input
                      type="range"
                      min={200}
                      max={2000}
                      step={50}
                      value={uploadChunkSize}
                      onChange={(event) => setUploadChunkSize(Number(event.target.value))}
                      className="w-full h-2 bg-border rounded-lg appearance-none cursor-pointer accent-primary"
                    />
                  </div>
                </div>
              )}

              {uploadChunkMode === "regex" && (
                <div className="space-y-4">
                  <div>
                    <Label className="text-sm mb-2 block">
                      {t("knowledge.detail.regexPattern")}
                    </Label>
                    <Input
                      value={uploadRegexPattern}
                      onChange={(event) => setUploadRegexPattern(event.target.value)}
                      placeholder={t("knowledge.detail.regexPlaceholder")}
                    />
                    <p className="text-xs text-muted-foreground mt-1">
                      {t("knowledge.detail.regexHint")}
                    </p>
                  </div>
                  <div>
                    <Label className="text-sm mb-2 block">
                      {t("knowledge.detail.presetPatterns")}
                    </Label>
                    <Select onValueChange={setUploadRegexPattern}>
                      <SelectTrigger>
                        <SelectValue placeholder={t("knowledge.detail.selectPreset")} />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="(?=第[一二三四五六七八九十]+章)">
                          {t("knowledge.detail.presetChapter")}
                        </SelectItem>
                        <SelectItem value="(?=\\d+\\.)">
                          {t("knowledge.detail.presetNumber")}
                        </SelectItem>
                        <SelectItem value="(?=#{1,3}\\s)">
                          {t("knowledge.detail.presetMarkdown")}
                        </SelectItem>
                        <SelectItem value={"\n\n+"}>
                          {t("knowledge.detail.presetBlankLine")}
                        </SelectItem>
                      </SelectContent>
                    </Select>
                  </div>
                  <div>
                    <div className="flex justify-between items-center mb-2">
                      <Label className="text-sm">
                        {t("knowledge.detail.maxChunkSizeLabel")}
                      </Label>
                      <span className="text-sm font-medium text-primary">{uploadChunkSize}</span>
                    </div>
                    <input
                      type="range"
                      min={200}
                      max={2000}
                      step={50}
                      value={uploadChunkSize}
                      onChange={(event) => setUploadChunkSize(Number(event.target.value))}
                      className="w-full h-2 bg-border rounded-lg appearance-none cursor-pointer accent-primary"
                    />
                  </div>
                </div>
              )}

              {uploadChunkMode === "recursive" && (
                <div className="space-y-4">
                  <p className="text-sm text-muted-foreground mb-2">
                    {t("knowledge.detail.recursiveHint")}
                  </p>
                  <div>
                    <div className="flex justify-between items-center mb-2">
                      <Label className="text-sm">
                        {t("knowledge.detail.chunkSizeLabel")}
                      </Label>
                      <span className="text-sm font-medium text-primary">{uploadChunkSize}</span>
                    </div>
                    <input
                      type="range"
                      min={100}
                      max={2000}
                      step={50}
                      value={uploadChunkSize}
                      onChange={(event) => setUploadChunkSize(Number(event.target.value))}
                      className="w-full h-2 bg-border rounded-lg appearance-none cursor-pointer accent-primary"
                    />
                  </div>
                  <div>
                    <div className="flex justify-between items-center mb-2">
                      <Label className="text-sm">
                        {t("knowledge.detail.overlapSizeLabel")}
                      </Label>
                      <span className="text-sm font-medium text-primary">
                        {uploadChunkOverlap}
                      </span>
                    </div>
                    <input
                      type="range"
                      min={0}
                      max={500}
                      step={10}
                      value={uploadChunkOverlap}
                      onChange={(event) => setUploadChunkOverlap(Number(event.target.value))}
                      className="w-full h-2 bg-border rounded-lg appearance-none cursor-pointer accent-primary"
                    />
                  </div>
                </div>
              )}

              {uploadChunkMode === "qa" && (
                <div className="space-y-4">
                  <div className="p-3 bg-amber-500/10 dark:bg-amber-500/15 rounded-lg">
                    <p className="text-sm text-amber-700 dark:text-amber-400">
                      {t("knowledge.detail.qaHint")}
                    </p>
                  </div>
                  <div>
                    <Label className="text-sm mb-2 block">
                      {t("knowledge.detail.questionPrefix")}
                    </Label>
                    <Input
                      value={uploadQuestionPrefix}
                      onChange={(event) => setUploadQuestionPrefix(event.target.value)}
                      placeholder="Q:"
                    />
                  </div>
                  <div>
                    <Label className="text-sm mb-2 block">
                      {t("knowledge.detail.answerPrefix")}
                    </Label>
                    <Input
                      value={uploadAnswerPrefix}
                      onChange={(event) => setUploadAnswerPrefix(event.target.value)}
                      placeholder="A:"
                    />
                  </div>
                </div>
              )}
            </div>
          </div>

          <div className="border rounded-lg">
            <button
              className="w-full p-4 flex items-center justify-between text-left"
              onClick={() => {
                const element = document.getElementById("advanced-settings");
                if (element) element.classList.toggle("hidden");
              }}
            >
              <span className="text-sm font-medium text-foreground">
                {t("knowledge.detail.advancedSettings")}
              </span>
              <ChevronDown className="h-4 w-4 text-muted-foreground" />
            </button>
            <div id="advanced-settings" className="hidden px-4 pb-4 space-y-4">
              <div className="p-4 bg-muted/40 rounded-lg">
                <div className="flex items-center justify-between mb-3">
                  <div>
                    <Label className="text-sm font-medium">
                      {t("knowledge.detail.metadataEnhancement")}
                    </Label>
                    <p className="text-xs text-muted-foreground">
                      {t("knowledge.detail.metadataEnhancementHint")}
                    </p>
                  </div>
                  <Switch
                    checked={uploadMetadataEnabled}
                    onCheckedChange={setUploadMetadataEnabled}
                  />
                </div>
                {uploadMetadataEnabled && (
                  <div className="pl-4 border-l-2 border-primary/20 space-y-2 mt-3">
                    <label className="flex items-center gap-2 cursor-pointer">
                      <input
                        type="checkbox"
                        checked={uploadExtractTitle}
                        onChange={(event) => setUploadExtractTitle(event.target.checked)}
                        className="w-4 h-4 rounded text-primary"
                      />
                      <span className="text-sm">{t("knowledge.detail.extractTitle")}</span>
                    </label>
                    <label className="flex items-center gap-2 cursor-pointer">
                      <input
                        type="checkbox"
                        checked={uploadExtractSummary}
                        onChange={(event) => setUploadExtractSummary(event.target.checked)}
                        className="w-4 h-4 rounded text-primary"
                      />
                      <span className="text-sm">{t("knowledge.detail.extractSummary")}</span>
                    </label>
                    <label className="flex items-center gap-2 cursor-pointer">
                      <input
                        type="checkbox"
                        checked={uploadExtractKeywords}
                        onChange={(event) => setUploadExtractKeywords(event.target.checked)}
                        className="w-4 h-4 rounded text-primary"
                      />
                      <span className="text-sm">{t("knowledge.detail.extractKeywords")}</span>
                    </label>
                    <label className="flex items-center gap-2 cursor-pointer">
                      <input
                        type="checkbox"
                        checked={uploadExtractEntities}
                        onChange={(event) => setUploadExtractEntities(event.target.checked)}
                        className="w-4 h-4 rounded text-primary"
                      />
                      <span className="text-sm">{t("knowledge.detail.extractEntities")}</span>
                    </label>
                    <label className="flex items-center gap-2 cursor-pointer">
                      <input
                        type="checkbox"
                        checked={uploadDetectLanguage}
                        onChange={(event) => setUploadDetectLanguage(event.target.checked)}
                        className="w-4 h-4 rounded text-primary"
                      />
                      <span className="text-sm">{t("knowledge.detail.detectLanguage")}</span>
                    </label>
                  </div>
                )}
              </div>

              <div className="p-4 bg-muted/40 rounded-lg">
                <div className="flex items-center justify-between mb-3">
                  <div>
                    <Label className="text-sm font-medium">
                      {t("knowledge.detail.tableProcessing")}
                    </Label>
                    <p className="text-xs text-muted-foreground">
                      {t("knowledge.detail.tableProcessingHint")}
                    </p>
                  </div>
                  <Switch
                    checked={uploadTableEnabled}
                    onCheckedChange={setUploadTableEnabled}
                  />
                </div>
                {uploadTableEnabled && (
                  <div className="pl-4 border-l-2 border-primary/20 space-y-3 mt-3">
                    <div>
                      <Label className="text-sm mb-2 block">
                        {t("knowledge.detail.tableMode")}
                      </Label>
                      <Select
                        value={uploadTableMode}
                        onValueChange={(value) =>
                          setUploadTableMode(value as typeof uploadTableMode)
                        }
                      >
                        <SelectTrigger>
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          <SelectItem value="markdown">
                            {t("knowledge.detail.tableMarkdown")}
                          </SelectItem>
                          <SelectItem value="row_based">
                            {t("knowledge.detail.tableRowBased")}
                          </SelectItem>
                          <SelectItem value="structured">
                            {t("knowledge.detail.tableStructured")}
                          </SelectItem>
                        </SelectContent>
                      </Select>
                    </div>
                    <label className="flex items-center gap-2 cursor-pointer">
                      <input
                        type="checkbox"
                        checked={uploadTableIncludeHeaders}
                        onChange={(event) =>
                          setUploadTableIncludeHeaders(event.target.checked)
                        }
                        className="w-4 h-4 rounded text-primary"
                      />
                      <span className="text-sm">
                        {t("knowledge.detail.tableIncludeHeaders")}
                      </span>
                    </label>
                    <label className="flex items-center gap-2 cursor-pointer">
                      <input
                        type="checkbox"
                        checked={uploadTableGenerateSummary}
                        onChange={(event) =>
                          setUploadTableGenerateSummary(event.target.checked)
                        }
                        className="w-4 h-4 rounded text-primary"
                      />
                      <span className="text-sm">
                        {t("knowledge.detail.tableGenerateSummary")}
                      </span>
                    </label>
                  </div>
                )}
              </div>

              <div className="p-4 bg-muted/40 rounded-lg">
                <div className="flex items-center justify-between mb-3">
                  <div>
                    <Label className="text-sm font-medium">
                      {t("knowledge.detail.rerankModelConfig")}
                    </Label>
                    <p className="text-xs text-muted-foreground">
                      {t("knowledge.detail.rerankModelConfigHint")}
                    </p>
                  </div>
                  <Switch checked={rerankEnabled} onCheckedChange={setRerankEnabled} />
                </div>
                {rerankEnabled && (
                  <Select value={rerankModel} onValueChange={setRerankModel}>
                    <SelectTrigger className="mt-2">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="gte-rerank">
                        {t("knowledge.detail.gteRerankLabel")}
                      </SelectItem>
                      <SelectItem value="bge-reranker-v2-m3">BGE Reranker v2</SelectItem>
                    </SelectContent>
                  </Select>
                )}
              </div>

              <div className="p-4 bg-muted/40 rounded-lg">
                <Label className="text-sm font-medium mb-2 block">
                  {t("knowledge.detail.embeddingModelSelect")}
                </Label>
                <p className="text-xs text-muted-foreground mb-3">
                  {t("knowledge.detail.embeddingModelSelectHint")}
                </p>
                <Select value={uploadEmbeddingModel} onValueChange={setUploadEmbeddingModel}>
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {DATASET_EMBEDDING_MODELS.map((model) => {
                      const badgeMap: Record<string, [string, string]> = {
                        gemini: [
                          "bg-blue-100 text-blue-600 dark:bg-blue-900 dark:text-blue-300",
                          "G",
                        ],
                        dashscope: [
                          "bg-orange-100 text-orange-600 dark:bg-orange-900 dark:text-orange-300",
                          "A",
                        ],
                        siliconflow: [
                          "bg-green-100 text-green-600 dark:bg-green-900 dark:text-green-300",
                          "S",
                        ],
                      };
                      const [badgeClass, badgeLabel] = badgeMap[model.provider] || [
                        "bg-gray-100 text-gray-600",
                        "?",
                      ];
                      return (
                        <SelectItem
                          key={`${model.provider}:${model.model}`}
                          value={`${model.provider}:${model.model}`}
                        >
                          <div className="flex items-center gap-2">
                            <span
                              className={`w-5 h-5 rounded ${badgeClass} flex items-center justify-center text-xs font-bold`}
                            >
                              {badgeLabel}
                            </span>
                            <span>{model.label}</span>
                            <span className="text-xs text-muted-foreground">
                              {t("knowledge.detail.dimensionValue", { dimension: model.dimension })}
                            </span>
                            {"badge" in model && model.badge && (
                              <span className="text-[10px] px-1.5 py-0.5 rounded bg-blue-500 text-white">
                                {t(model.badge)}
                              </span>
                            )}
                          </div>
                        </SelectItem>
                      );
                    })}
                  </SelectContent>
                </Select>
              </div>
            </div>
          </div>

          <div className="p-3 bg-primary/5 rounded-lg border border-primary/20 flex items-center gap-3">
            <div className="w-8 h-8 bg-primary rounded flex items-center justify-center text-white text-xs font-bold shrink-0">
              {dataset?.embedding_provider === "gemini" ? "G" : "A"}
            </div>
            <div className="text-sm">
              <span className="font-medium">
                {dataset?.embedding_model || "text-embedding-v4"}
              </span>
              <span className="text-muted-foreground ml-2">
                {t("knowledge.detail.dimension", {
                  dim: dataset?.embedding_dimension || 1024,
                })}
              </span>
            </div>
          </div>
        </div>

        {uploadFailures.length > 0 && (
          <div role="alert" className="mt-3 max-h-40 shrink-0 overflow-y-auto rounded-md border border-amber-500/40 bg-amber-500/10 p-3 text-sm">
            <p className="font-medium">{t("knowledge.detail.uploadPartialFailures")}</p>
            <ul className="mt-2 space-y-1">
              {uploadFailures.map((failure, index) => (
                <li key={`${failure.name}-${index}`}>
                  <span className="font-medium">#{failure.position} {failure.name} ({(failure.sizeBytes / 1024).toFixed(1)} KB):</span> {failure.error}.{" "}
                  {failure.documentId
                    ? <>{t("knowledge.detail.uploadRecordCreated")} {t("knowledge.detail.uploadKnownDocumentId", { id: failure.documentId })}</>
                    : failure.retrySafe
                      ? t("knowledge.detail.uploadRetrySelected")
                      : t("knowledge.detail.uploadInspectBeforeRetry")}
                </li>
              ))}
            </ul>
          </div>
        )}
        <DialogFooter className="mt-4 pt-4 border-t shrink-0">
          <Button
            variant="outline"
            disabled={uploading}
            onClick={() => {
              if (uploading) return;
              onOpenChange(false);
              onPendingFilesChange([]);
              clearUploadFailures();
            }}
          >
            {t("knowledge.detail.uploadCancel")}
          </Button>
          <Button
            onClick={handleConfirmUpload}
            disabled={uploading || pendingFiles.length === 0}
            className="bg-primary hover:bg-primary/90 text-white"
          >
            {t("knowledge.detail.uploadConfirm", { count: pendingFiles.length })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
    </>
  );
}
