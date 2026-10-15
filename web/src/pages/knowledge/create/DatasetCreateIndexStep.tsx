import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Eye, HelpCircle, Loader2, Sparkles } from "lucide-react";

import { previewChunking, type ChunkPreviewItem } from "@/api/knowledge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
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
import { Textarea } from "@/components/ui/textarea";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import type { ChunkingConfig, ChunkingMode } from "@/types/knowledge";

const CHUNKING_MODES: Array<{ id: ChunkingMode; nameKey: string; descKey: string }> = [
  {
    id: "automatic",
    nameKey: "knowledge.create.chunkAutomatic",
    descKey: "knowledge.create.chunkAutomaticDesc",
  },
  {
    id: "fixed_size",
    nameKey: "knowledge.create.chunkFixedSize",
    descKey: "knowledge.create.chunkFixedSizeDesc",
  },
  {
    id: "paragraph",
    nameKey: "knowledge.create.chunkParagraph",
    descKey: "knowledge.create.chunkParagraphDesc",
  },
  {
    id: "heading",
    nameKey: "knowledge.create.chunkHeading",
    descKey: "knowledge.create.chunkHeadingDesc",
  },
  {
    id: "recursive",
    nameKey: "knowledge.create.chunkRecursive",
    descKey: "knowledge.create.chunkRecursiveDesc",
  },
  {
    id: "hierarchical",
    nameKey: "knowledge.create.chunkHierarchical",
    descKey: "knowledge.create.chunkHierarchicalDesc",
  },
];

const RERANK_MODELS = [
  { id: "default", nameKey: "knowledge.create.rerankDefault" },
  { id: "gte-rerank", name: "GTE-ReRank" },
  { id: "gte-rerank-v2", name: "GTE-ReRank v2" },
  { id: "bge-reranker-v2-m3", name: "BGE Reranker v2-m3" },
];

type ChunkPreviewConfig = Pick<
  ChunkingConfig,
  | "mode"
  | "chunk_size"
  | "chunk_overlap"
  | "remove_extra_spaces"
  | "strict_section_traceability"
>;

function ChunkPreviewSection({ config }: { config: ChunkPreviewConfig }) {
  const { t } = useTranslation();
  const [text, setText] = useState(`# Sample Header

Here is some sample text content to demonstrate how the chunking works.
It supports markdown structure detection and automatic splitting.

## Section 2

Longer paragraphs will be split recursively based on the chunk size setting.
Try pasting your own content here to test.`);
  const [chunks, setChunks] = useState<ChunkPreviewItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [showPreview, setShowPreview] = useState(false);

  const handlePreview = async () => {
    if (!text.trim()) return;
    setLoading(true);
    try {
      const response = await previewChunking("temp", text, config);
      setChunks(response.chunks);
    } catch (error) {
      console.error(error);
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="border rounded-lg p-4 bg-card">
      <div className="flex items-center justify-between mb-4">
        <div className="flex items-center gap-2">
          <Eye className="h-4 w-4 text-primary" />
          <Label className="text-sm font-medium">{t("knowledge.create.chunkPreview")}</Label>
        </div>
        <Dialog open={showPreview} onOpenChange={setShowPreview}>
          <DialogTrigger asChild>
            <Button variant="outline" size="sm">
              <Eye className="mr-2 h-4 w-4" />
              {t("knowledge.create.testChunking")}
            </Button>
          </DialogTrigger>
          <DialogContent className="max-w-3xl h-[80vh] flex flex-col">
            <DialogHeader>
              <DialogTitle>{t("knowledge.create.chunkPreviewTitle")}</DialogTitle>
            </DialogHeader>
            <div className="flex-1 flex gap-4 min-h-0 pt-4">
              <div className="flex-1 flex flex-col gap-2">
                <Label>{t("knowledge.create.testText")}</Label>
                <Textarea
                  value={text}
                  onChange={(event) => setText(event.target.value)}
                  className="flex-1 resize-none font-mono text-sm"
                  placeholder={t("knowledge.create.testTextPlaceholder")}
                />
              </div>
              <div className="flex-1 flex flex-col gap-2">
                <div className="flex items-center justify-between">
                  <Label>
                    {t("knowledge.create.chunkResults")} ({chunks.length})
                  </Label>
                  <Button size="sm" onClick={handlePreview} disabled={loading}>
                    {loading ? (
                      <Loader2 className="h-4 w-4 animate-spin" />
                    ) : (
                      t("knowledge.create.executeChunking")
                    )}
                  </Button>
                </div>
                <div className="flex-1 border rounded-md bg-muted/40 p-4 overflow-y-auto">
                  <div className="space-y-4">
                    {chunks.map((chunk, index) => (
                      <div
                        key={index}
                        className="bg-card p-3 rounded border shadow-xs text-sm"
                      >
                        <div className="mb-2 text-xs text-muted-foreground/70 flex justify-between">
                          <span># {index + 1}</span>
                          <span>{chunk.char_count} chars</span>
                        </div>
                        <div className="whitespace-pre-wrap">{chunk.content}</div>
                        {chunk.metadata && Object.keys(chunk.metadata).length > 0 && (
                          <div className="mt-2 pt-2 border-t text-xs text-muted-foreground">
                            {JSON.stringify(chunk.metadata)}
                          </div>
                        )}
                      </div>
                    ))}
                    {chunks.length === 0 && !loading && (
                      <div className="text-center text-muted-foreground/70 py-10">
                        {t("knowledge.create.clickToPreview")}
                      </div>
                    )}
                  </div>
                </div>
              </div>
            </div>
          </DialogContent>
        </Dialog>
      </div>
      <div className="text-xs text-muted-foreground">
        {t("knowledge.create.previewHint")}
      </div>
    </div>
  );
}

interface DatasetCreateIndexStepProps {
  chunkingMode: ChunkingMode;
  maxChunkSize: number;
  metadataExtract: boolean;
  excelHeaderConcat: boolean;
  multiTurnRewrite: boolean;
  rerankModel: string;
  scoreThreshold: number;
  maxRecall: number;
  onChunkingModeChange: (value: ChunkingMode) => void;
  onMaxChunkSizeChange: (value: number) => void;
  onMetadataExtractChange: (value: boolean) => void;
  onExcelHeaderConcatChange: (value: boolean) => void;
  onMultiTurnRewriteChange: (value: boolean) => void;
  onRerankModelChange: (value: string) => void;
  onScoreThresholdChange: (value: number) => void;
  onMaxRecallChange: (value: number) => void;
}

export function DatasetCreateIndexStep({
  chunkingMode,
  maxChunkSize,
  metadataExtract,
  excelHeaderConcat,
  multiTurnRewrite,
  rerankModel,
  scoreThreshold,
  maxRecall,
  onChunkingModeChange,
  onMaxChunkSizeChange,
  onMetadataExtractChange,
  onExcelHeaderConcatChange,
  onMultiTurnRewriteChange,
  onRerankModelChange,
  onScoreThresholdChange,
  onMaxRecallChange,
}: DatasetCreateIndexStepProps) {
  const { t } = useTranslation();

  return (
    <div className="space-y-8">
      <div>
        <Label className="text-sm font-medium">
          {t("knowledge.create.chunkingMode")} <span className="text-red-500">*</span>
        </Label>
        <div
          className="mt-3 grid grid-cols-1 gap-3 sm:grid-cols-3"
          role="radiogroup"
          aria-label={t("knowledge.create.chunkingMode")}
        >
          {CHUNKING_MODES.map((mode) => (
            <Card
              key={mode.id}
              role="radio"
              aria-checked={chunkingMode === mode.id}
              tabIndex={0}
              className={`p-4 cursor-pointer transition-colors ${
                chunkingMode === mode.id
                  ? "border-2 border-primary bg-primary/5"
                  : "border hover:border-border"
              }`}
              onClick={() => onChunkingModeChange(mode.id)}
              onKeyDown={(event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  onChunkingModeChange(mode.id);
                }
              }}
            >
              <div className="flex items-start justify-between">
                <h4 className="text-sm font-medium text-foreground">{t(mode.nameKey)}</h4>
                <div
                  className={`w-4 h-4 rounded-full border-2 flex items-center justify-center ${
                    chunkingMode === mode.id
                      ? "border-primary bg-primary/50"
                      : "border-border"
                  }`}
                >
                  {chunkingMode === mode.id && (
                    <div className="w-2 h-2 rounded-full bg-card" />
                  )}
                </div>
              </div>
              <p className="text-xs text-muted-foreground mt-2 leading-relaxed">
                {t(mode.descKey)}
              </p>
            </Card>
          ))}
        </div>
      </div>

      <div className="p-4 bg-muted/40 rounded-lg">
        <div className="flex items-center justify-between mb-3">
          <Label className="text-sm font-medium">
            {t("knowledge.create.maxChunkSize")} <span className="text-red-500">*</span>
          </Label>
        </div>
        <div className="flex items-center gap-4">
          <input
            type="range"
            min={50}
            max={6000}
            step={10}
            value={maxChunkSize}
            onChange={(event) => onMaxChunkSizeChange(Number(event.target.value))}
            className="flex-1"
          />
          <Input
            type="number"
            value={maxChunkSize}
            onChange={(event) =>
              onMaxChunkSizeChange(
                Math.max(50, Math.min(6000, Number(event.target.value) || 50))
              )
            }
            className="w-24"
          />
        </div>
        <div className="flex justify-between text-xs text-muted-foreground/70 mt-1">
          <span>50</span>
          <span>6000</span>
        </div>
      </div>

      <ChunkPreviewSection
        config={{
          mode: chunkingMode,
          chunk_size: maxChunkSize,
          chunk_overlap: Math.min(50, Math.floor(maxChunkSize * 0.1)),
          remove_extra_spaces: true,
        }}
      />

      <div className="space-y-4">
        <div className="flex items-center justify-between p-3 rounded-lg bg-card border">
          <div className="flex items-center gap-2">
            <span className="text-sm text-foreground/80">
              {t("knowledge.create.metadataExtract")}
            </span>
            <TooltipProvider>
              <Tooltip>
                <TooltipTrigger>
                  <HelpCircle className="h-4 w-4 text-muted-foreground/70" />
                </TooltipTrigger>
                <TooltipContent>
                  <p className="max-w-xs">{t("knowledge.create.metadataExtractHint")}</p>
                </TooltipContent>
              </Tooltip>
            </TooltipProvider>
          </div>
          <Switch checked={metadataExtract} onCheckedChange={onMetadataExtractChange} />
        </div>

        <div className="flex items-center justify-between p-3 rounded-lg bg-card border">
          <div className="flex items-center gap-2">
            <span className="text-sm text-foreground/80">
              {t("knowledge.create.excelHeaderConcat")}
            </span>
            <TooltipProvider>
              <Tooltip>
                <TooltipTrigger>
                  <HelpCircle className="h-4 w-4 text-muted-foreground/70" />
                </TooltipTrigger>
                <TooltipContent>
                  <p className="max-w-xs">{t("knowledge.create.excelHeaderConcatHint")}</p>
                </TooltipContent>
              </Tooltip>
            </TooltipProvider>
          </div>
          <Switch
            checked={excelHeaderConcat}
            onCheckedChange={onExcelHeaderConcatChange}
          />
        </div>

        <div className="flex items-center justify-between p-3 rounded-lg bg-card border">
          <div className="flex items-center gap-2">
            <span className="text-sm text-foreground/80">
              {t("knowledge.create.multiTurnRewrite")}
            </span>
            <TooltipProvider>
              <Tooltip>
                <TooltipTrigger>
                  <HelpCircle className="h-4 w-4 text-muted-foreground/70" />
                </TooltipTrigger>
                <TooltipContent>
                  <p className="max-w-xs">{t("knowledge.create.multiTurnRewriteHint")}</p>
                </TooltipContent>
              </Tooltip>
            </TooltipProvider>
          </div>
          <Switch checked={multiTurnRewrite} onCheckedChange={onMultiTurnRewriteChange} />
        </div>
      </div>

      <div className="space-y-4 pt-4 border-t">
        <div>
          <div className="flex items-center gap-2 mb-2">
            <Label className="text-sm font-medium">{t("knowledge.create.rerankModel")}</Label>
            <TooltipProvider>
              <Tooltip>
                <TooltipTrigger>
                  <HelpCircle className="h-4 w-4 text-muted-foreground/70" />
                </TooltipTrigger>
                <TooltipContent>
                  <p className="max-w-xs">{t("knowledge.create.rerankModelHint")}</p>
                </TooltipContent>
              </Tooltip>
            </TooltipProvider>
          </div>
          <Select value={rerankModel} onValueChange={onRerankModelChange}>
            <SelectTrigger>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {RERANK_MODELS.map((model) => (
                <SelectItem key={model.id} value={model.id}>
                  <div className="flex items-center gap-2">
                    <Sparkles className="h-4 w-4 text-primary" />
                    <span>{model.nameKey ? t(model.nameKey) : model.name}</span>
                  </div>
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        <div>
          <div className="flex items-center justify-between mb-2">
            <div className="flex items-center gap-2">
              <Label className="text-sm font-medium">
                {t("knowledge.create.scoreThreshold")}
              </Label>
              <TooltipProvider>
                <Tooltip>
                  <TooltipTrigger>
                    <HelpCircle className="h-4 w-4 text-muted-foreground/70" />
                  </TooltipTrigger>
                  <TooltipContent>
                    <p className="max-w-xs">{t("knowledge.create.scoreThresholdHint")}</p>
                  </TooltipContent>
                </Tooltip>
              </TooltipProvider>
            </div>
          </div>
          <div className="flex items-center gap-4">
            <input
              type="range"
              min={0.01}
              max={1}
              step={0.01}
              value={scoreThreshold}
              onChange={(event) => onScoreThresholdChange(Number(event.target.value))}
              className="flex-1"
            />
            <Input
              type="number"
              value={scoreThreshold.toFixed(2)}
              onChange={(event) =>
                onScoreThresholdChange(
                  Math.max(0.01, Math.min(1, Number(event.target.value) || 0.2))
                )
              }
              className="w-24"
              step={0.01}
            />
          </div>
          <div className="flex justify-between text-xs text-muted-foreground/70 mt-1">
            <span>0.01</span>
            <span>1</span>
          </div>
        </div>

        <div>
          <div className="flex items-center justify-between mb-2">
            <Label className="text-sm font-medium">{t("knowledge.create.maxRecall")}</Label>
          </div>
          <div className="flex items-center gap-4">
            <input
              type="range"
              min={1}
              max={20}
              step={1}
              value={maxRecall}
              onChange={(event) => onMaxRecallChange(Number(event.target.value))}
              className="flex-1"
            />
            <Input
              type="number"
              value={maxRecall}
              onChange={(event) =>
                onMaxRecallChange(
                  Math.max(1, Math.min(20, Number(event.target.value) || 5))
                )
              }
              className="w-24"
            />
          </div>
          <div className="flex justify-between text-xs text-muted-foreground/70 mt-1">
            <span>1</span>
            <span>20</span>
          </div>
        </div>
      </div>
    </div>
  );
}
