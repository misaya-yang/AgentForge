import { useRef, useCallback, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { motion, AnimatePresence, useReducedMotion } from "motion/react";
import { ChevronDown, Send, Loader2, X, FileText, ImageIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { QuickActionsMenu } from "../components/QuickActionsMenu"; // Assume this exists or is moved
import { StyleSelector } from "../components/StyleSelector";
import type { UploadedFile } from "../hooks/useFileHandler";
import { isImageFile, formatFileSize } from "@/api/files";
import type { DatasetInfo, AssistantConfig } from "@/api/assistant";
import type { ModelReasoningOption } from "@/api/models";

const ASSISTANT_UI_V2 = import.meta.env.VITE_ASSISTANT_UI_V2 !== "false";

interface ChatInputAreaProps {
  composerId?: string;
  input: string;
  setInput: (val: string) => void;
  files: UploadedFile[];
  isUploading: boolean;
  selectionNotice: string;
  isStreaming: boolean;
  isComposerBlocked: boolean;
  isGeneratingImage: boolean;
  isImageMode: boolean;
  hasAvailableModel: boolean;
  supportsVision: boolean;
  handleFileSelect: (files: FileList | null) => void;
  removeFile: (index: number) => void;
  retryFile: (index: number) => void;
  toggleFileSelection: (index: number) => void;
  onSend: () => void;
  onStop: () => void;
  onCancelImageMode: () => void;
  handlePaste: (e: React.ClipboardEvent) => void;
  fileInputRef: React.RefObject<HTMLInputElement | null>;
  config: AssistantConfig | null;
  datasets: DatasetInfo[];
  datasetsLoadError: boolean;
  selectedDatasets: string[];
  onToggleDataset: (id: string) => void;
  webSearchEnabled: boolean;
  setWebSearchEnabled: (val: boolean) => void;
  thinkingLevel: string;
  setThinkingLevel: (val: string) => void;
  reasoningOptions: ModelReasoningOption[];
  handleImageGenerate: () => void;
  selectedStyle: string;
  setSelectedStyle: (style: string) => void;
  onOpenConnectors?: () => void;
  connectorCount?: number;
}

export function ChatInputArea({
  composerId,
  input,
  setInput,
  files,
  isUploading,
  selectionNotice,
  isStreaming,
  isComposerBlocked,
  isGeneratingImage,
  isImageMode,
  hasAvailableModel,
  supportsVision,
  handleFileSelect,
  removeFile,
  retryFile,
  toggleFileSelection,
  onSend,
  onStop,
  onCancelImageMode,
  handlePaste,
  fileInputRef,
  config,
  datasets,
  datasetsLoadError,
  selectedDatasets,
  onToggleDataset,
  webSearchEnabled,
  setWebSearchEnabled,
  thinkingLevel,
  setThinkingLevel,
  reasoningOptions,
  handleImageGenerate,
  selectedStyle,
  setSelectedStyle,
  onOpenConnectors,
  connectorCount = 0,
}: ChatInputAreaProps) {
  const { t } = useTranslation();
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const shouldReduceMotion = useReducedMotion();

  const hasUploadedFiles = files.some((f) => f.status === "success" && f.selected && f.response);
  const selectedFiles = files.filter((f) => f.status === "success" && f.selected && f.response);
  const unsupportedSelectedImage = !supportsVision && selectedFiles.some((f) => isImageFile(f.file));
  const unavailableDatasets = selectedDatasets.filter((id) => !datasets.some((dataset) => dataset.dataset_id === id));
  const scopeUnavailable = (selectedDatasets.length > 0 && (datasetsLoadError || unavailableDatasets.length > 0)) ||
    (webSearchEnabled && !config?.web_search_enabled);
  const canSend =
    !isComposerBlocked &&
    !isUploading &&
    !isGeneratingImage &&
    hasAvailableModel &&
    !scopeUnavailable &&
    (isImageMode
      ? Boolean(input.trim())
      : !unsupportedSelectedImage && Boolean(input.trim() || hasUploadedFiles));
  const selectedReasoningLabel =
    thinkingLevel === "auto"
      ? t("assistant.thinkingAuto", "Think auto")
      : reasoningOptions.find((option) => option.id === thinkingLevel)?.label || thinkingLevel;

  // Auto-resize textarea
  const handleTextareaChange = useCallback(
    (e: React.ChangeEvent<HTMLTextAreaElement>) => {
      setInput(e.target.value);
      e.target.style.height = "auto";
      e.target.style.height = Math.min(e.target.scrollHeight, 200) + "px";
    },
    [setInput]
  );

  // Handle key press (check isComposing to avoid sending during IME composition)
  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      // Don't send if user is composing with IME (e.g., typing Chinese/Japanese)
      if (e.nativeEvent.isComposing || e.keyCode === 229) {
        return;
      }
      if (e.key === "Escape" && (isStreaming || isImageMode)) {
        e.preventDefault();
        e.stopPropagation();
        if (isStreaming) onStop();
        else onCancelImageMode();
        return;
      }
      const isSubmitShortcut =
        (e.key === "Enter" && !e.shiftKey) ||
        (e.key === "Enter" && (e.metaKey || e.ctrlKey));
      if (isSubmitShortcut && canSend) {
        e.preventDefault();
        onSend();
      }
    },
    [canSend, isStreaming, isImageMode, onSend, onStop, onCancelImageMode]
  );

  // Reset height when input clears
  useEffect(() => {
    if (!input && textareaRef.current) {
      textareaRef.current.style.height = "auto";
    }
  }, [input]);

  return (
    <div className="border-t border-[hsl(var(--assistant-border))] bg-[hsl(var(--assistant-canvas-bg))]/90 backdrop-blur-xl">
      {/* File previews */}
      <AnimatePresence>
        {files.length > 0 && (
          <motion.div
            initial={shouldReduceMotion ? false : { height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={shouldReduceMotion ? { opacity: 0 } : { height: 0, opacity: 0 }}
            className="overflow-hidden border-b border-[hsl(var(--assistant-border-soft))]"
          >
            {selectionNotice === "limit" && (
              <p role="alert" className="px-4 pt-2 text-xs text-amber-600 dark:text-amber-400">
                {t("assistant.fileLimit", "At most five attachments can be added to one message.")}
              </p>
            )}
            {unsupportedSelectedImage && (
              <p role="alert" className="px-4 pt-2 text-xs text-amber-600 dark:text-amber-400">
                {t("assistant.imageModelRequired", "The selected model cannot read images. Choose a vision model or exclude the image before sending.")}
              </p>
            )}
            <div className="px-4 py-3 flex flex-wrap gap-2">
              {files.map((f, index) => (
                <motion.div
                  key={f.id}
                  initial={shouldReduceMotion ? false : { opacity: 0, scale: 0.96 }}
                  animate={{ opacity: 1, scale: 1 }}
                  exit={shouldReduceMotion ? { opacity: 0 } : { opacity: 0, scale: 0.96 }}
                  className={cn(
                    "relative group rounded-xl px-3 py-2 text-xs flex items-center gap-2 transition-colors overflow-hidden",
                    f.status === "error"
                      ? "bg-red-100 dark:bg-red-900/30 text-red-600 dark:text-red-400"
                      : f.status === "uploading"
                        ? "bg-primary/10 dark:bg-primary/20 text-primary dark:text-primary"
                        : f.status === "success"
                          ? "bg-emerald-100 dark:bg-emerald-900/30 text-emerald-600 dark:text-emerald-400"
                          : "bg-slate-100 dark:bg-slate-800 text-slate-600 dark:text-slate-400"
                  )}
                >
                  {f.status === "uploading" && f.progress !== undefined && (
                    <motion.div
                      className="absolute inset-0 bg-current opacity-10"
                      initial={{ width: 0 }}
                      animate={{ width: `${f.progress}%` }}
                      transition={{ duration: 0.3 }}
                    />
                  )}
                  {isImageFile(f.file) ? (
                    <ImageIcon className="h-4 w-4 shrink-0 relative z-10" />
                  ) : (
                    <FileText className="h-4 w-4 shrink-0 relative z-10" />
                  )}
                  <span className="truncate max-w-[100px] relative z-10 font-medium">
                    {f.file.name}
                  </span>
                  <span className="text-[10px] opacity-70 relative z-10">
                    {f.status === "uploading" && f.progress !== undefined
                      ? `${f.progress}%`
                      : formatFileSize(f.file.size)}
                  </span>
                  {f.status === "success" && (
                    <button
                      type="button"
                      onClick={() => toggleFileSelection(index)}
                      aria-pressed={f.selected}
                      className="relative z-10 rounded border border-current/30 px-1.5 py-0.5 text-[10px]"
                    >
                      {f.selected
                        ? t("assistant.fileIncluded", "Included in next message")
                        : t("assistant.fileExclude", "Not included")}
                    </button>
                  )}
                  {f.status === "error" && (
                    <>
                      <span role="alert" className="relative z-10 max-w-[160px] text-[10px]">
                        {f.errorCode === "unsupported"
                          ? t("assistant.fileUnsupported", "Unsupported file type")
                          : f.errorCode === "too_large"
                            ? t("assistant.fileTooLarge", "File exceeds the upload limit")
                            : t("assistant.fileUploadFailed", "Upload failed")}
                      </span>
                      {f.errorCode === "upload_failed" && (
                        <button
                          type="button"
                          onClick={() => retryFile(index)}
                          className="relative z-10 rounded border border-current/30 px-1.5 py-0.5 text-[10px]"
                        >
                          {t("assistant.fileRetry", "Retry")}
                        </button>
                      )}
                    </>
                  )}
                  <button
                    type="button"
                    onClick={() => removeFile(index)}
                    className="absolute -right-1 -top-1 z-20 rounded-full bg-slate-800 p-1 text-white opacity-70 transition-opacity hover:bg-slate-900 hover:opacity-100 focus-visible:opacity-100 dark:bg-slate-200 dark:text-slate-800 dark:hover:bg-white sm:opacity-0 sm:group-focus-within:opacity-100 sm:group-hover:opacity-100"
                    aria-label={`${t("common.delete")}: ${f.file.name}`}
                  >
                    <X className="h-3 w-3" />
                  </button>
                </motion.div>
              ))}
            </div>
          </motion.div>
        )}
      </AnimatePresence>

      <div className="px-3 pb-[max(12px,env(safe-area-inset-bottom))] pt-3 sm:p-4">
        <div className={cn("w-full mx-auto", ASSISTANT_UI_V2 ? "max-w-[760px]" : "max-w-3xl")}>
          {/* Input container — restrained: 10px radius, hairline border,
              no drop-shadow-sm at rest; focus-within swaps to accent-tinted
              ring without elevating the card. */}
          <div className="relative flex items-end gap-2 p-2 rounded-[10px] bg-[hsl(var(--assistant-surface-bg))] border border-[hsl(var(--assistant-border))] focus-within:border-[hsl(var(--assistant-accent))]/40 transition-colors duration-150">
            {/* Quick actions menu */}
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  type="button"
                  variant="ghost"
                  aria-label={t("assistant.thinkingLevel", "Thinking")}
                  disabled={isComposerBlocked || isGeneratingImage}
                  className="h-9 min-w-[108px] max-w-[148px] shrink-0 justify-between gap-1.5 rounded-md border border-transparent bg-[hsl(var(--assistant-surface-soft))] px-2.5 text-[12px] font-medium text-[hsl(var(--assistant-text-secondary))] shadow-none hover:border-[hsl(var(--assistant-border))] hover:bg-[hsl(var(--assistant-chip-bg))] hover:text-[hsl(var(--assistant-text-primary))] focus-visible:border-[hsl(var(--assistant-accent))]/45 focus-visible:ring-[3px] focus-visible:ring-[hsl(var(--assistant-accent))]/15"
                >
                  <span className="truncate">{selectedReasoningLabel}</span>
                  <ChevronDown className="h-3.5 w-3.5 shrink-0 text-[hsl(var(--assistant-text-tertiary))]" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent
                side="top"
                align="start"
                sideOffset={8}
                className="min-w-[168px] border-[hsl(var(--assistant-border))] bg-[hsl(var(--assistant-surface-bg))] text-[hsl(var(--assistant-text-primary))]"
              >
                <DropdownMenuRadioGroup value={thinkingLevel} onValueChange={setThinkingLevel}>
                  <DropdownMenuRadioItem value="auto">
                    {t("assistant.thinkingAuto", "Think auto")}
                  </DropdownMenuRadioItem>
                  {reasoningOptions.map((option) => (
                    <DropdownMenuRadioItem key={option.id} value={option.id}>
                      {option.label}
                    </DropdownMenuRadioItem>
                  ))}
                </DropdownMenuRadioGroup>
              </DropdownMenuContent>
            </DropdownMenu>
            <QuickActionsMenu
              onFileUpload={() => fileInputRef.current?.click()}
              onImageGenerate={handleImageGenerate}
              onToggleWebSearch={() => setWebSearchEnabled(!webSearchEnabled)}
              onOpenConnectors={onOpenConnectors}
              webSearchEnabled={webSearchEnabled}
              kbAvailable={config?.kb_enabled ?? false}
              webSearchAvailable={config?.web_search_enabled ?? false}
              disabled={isComposerBlocked || isGeneratingImage}
              connectorCount={connectorCount}
              datasets={datasets}
              selectedDatasets={selectedDatasets}
              onToggleDataset={onToggleDataset}
            />

            {/* Hidden file input */}
            <input
              ref={fileInputRef}
              type="file"
              multiple
              accept=".pdf,.docx,.doc,.md,.txt,.csv,.xlsx,.png,.jpg,.jpeg,.gif,.webp"
              className="hidden"
              disabled={isComposerBlocked || isUploading}
              onChange={(e) => {
                handleFileSelect(e.target.files);
                e.target.value = "";
              }}
            />

            {/* Text input */}
            <Textarea
              id={composerId}
              ref={textareaRef}
              value={input}
              onChange={handleTextareaChange}
              onKeyDown={handleKeyDown}
              onPaste={handlePaste}
              aria-label={t("assistant.composerAriaLabel", "Assistant message composer")}
              placeholder={
                isImageMode
                  ? t(
                      "assistant.imagePlaceholder",
                      "Describe the image you want to create... (ESC to cancel)"
                    )
                  : t(
                      hasAvailableModel
                        ? "assistant.placeholder"
                        : "assistant.noModelsPlaceholder",
                      hasAvailableModel
                        ? "Type your message... (Ctrl+V to paste images)"
                        : "No models available"
                    )
              }
              className={cn(
                "flex-1 min-h-[44px] max-h-[200px] resize-none border-0 bg-transparent focus-visible:ring-0 text-sm text-[hsl(var(--assistant-text-primary))]",
                isImageMode
                  ? "placeholder:text-[hsl(var(--assistant-accent))]"
                  : "placeholder:text-[hsl(var(--assistant-text-tertiary))]"
              )}
              disabled={isComposerBlocked || isGeneratingImage}
              rows={1}
            />

            {/* Send/Stop button — the ONE primary action on the page.
                Accent-tinted fill, 6px radius, no glow, no scale wiggle. */}
            {isStreaming ? (
              <Button
                type="button"
                size="icon"
                className="h-9 w-9 shrink-0 rounded-md bg-[hsl(var(--destructive))]/90 hover:bg-[hsl(var(--destructive))] text-white transition-colors duration-150"
                onClick={(event) => {
                  // The second click of Send can land on this replacement
                  // button. It must not cancel the newly admitted turn.
                  if (event.detail <= 1) onStop();
                }}
                aria-label={t("assistant.stopGenerating", "Stop generating")}
                aria-keyshortcuts="Escape"
              >
                <X className="h-4 w-4" />
              </Button>
            ) : (
              <Button
                type="button"
                size="icon"
                className={cn(
                  "h-9 w-9 shrink-0 rounded-md transition-colors duration-150",
                  canSend
                    ? "bg-[hsl(var(--assistant-accent))]/15 hover:bg-[hsl(var(--assistant-accent))]/25 text-[hsl(var(--assistant-accent))] border border-[hsl(var(--assistant-accent))]/20"
                    : "bg-transparent text-[hsl(var(--assistant-text-tertiary))] cursor-not-allowed"
                )}
                onClick={onSend}
                disabled={!canSend}
                aria-label={t("common.send", "Send")}
                aria-keyshortcuts="Enter,Control+Enter,Meta+Enter"
              >
                {isUploading || isGeneratingImage ? (
                  <Loader2 className="h-4 w-4 animate-spin" />
                ) : isImageMode ? (
                  <ImageIcon className="h-4 w-4" />
                ) : (
                  <Send className="h-4 w-4" />
                )}
              </Button>
            )}
          </div>

          {/* Control Bar - Style selector */}
          <div className="mt-2 flex flex-wrap items-center justify-between gap-2 px-1">
            <div className="flex items-center gap-2">
              <StyleSelector
                selectedStyle={selectedStyle}
                onSelect={setSelectedStyle}
                disabled={isComposerBlocked}
              />
            </div>
            <span className="text-[11px] text-[hsl(var(--assistant-text-tertiary))]">
              {t("assistant.disclaimer", "AI responses may be inaccurate")}
            </span>
          </div>
          <div className="mt-1 flex flex-wrap gap-1.5 px-1 text-[11px] text-[hsl(var(--assistant-text-tertiary))]" aria-label={t("assistant.roundScope", "This round's sources")}>
            <span>{t("assistant.roundScope", "This round's sources")}:</span>
            {selectedDatasets.length === 0 && selectedFiles.length === 0 && !webSearchEnabled && <span>{t("assistant.roundScopeNone", "No knowledge base or attachments selected")}</span>}
            {selectedDatasets.map((id) => (
              <span key={id} className="rounded border px-1.5">
                {t("assistant.companyKB")}: {datasets.find((dataset) => dataset.dataset_id === id)?.name || `${id} · ${t("assistant.scopeUnavailable", "unavailable")}`}
              </span>
            ))}
            {selectedFiles.map((file) => <span key={file.id} className="max-w-[150px] truncate rounded border px-1.5">{file.file.name}</span>)}
            {webSearchEnabled && <span className="rounded border px-1.5">{t("assistant.webSearchMenu", "Web search")}</span>}
          </div>
          {scopeUnavailable && <p role="alert" className="px-1 pt-1 text-xs text-amber-600">{t("assistant.datasetScopeUnavailable", "Selected knowledge sources are unavailable. Review this round's sources before sending.")}</p>}
        </div>
      </div>
    </div>
  );
}
