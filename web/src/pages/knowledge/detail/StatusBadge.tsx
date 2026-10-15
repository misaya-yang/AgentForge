import React from "react";
import { AlertCircle, CheckCircle, File, Loader2 } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Badge } from "@/components/ui/badge";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";

type StatusBadgeProps = {
  status: string;
  progress?: number;
  metadata?: {
    processing_mode?: string;
    pages_processed?: number;
    total_pages?: number;
    segments_created?: number;
  };
};

export function StatusBadge({ status, progress, metadata }: StatusBadgeProps) {
  const { t } = useTranslation();
  const s = (status || "").toLowerCase();
  
  // Get page progress for scanned documents
  const pagesProcessed = metadata?.pages_processed;
  const totalPages = metadata?.total_pages;
  const hasPageProgress = typeof pagesProcessed === "number" && typeof totalPages === "number" && totalPages > 0;

  const configs: Record<
    string,
    { icon: React.ReactNode; label: string; className: string }
  > = {
    completed: {
      icon: <CheckCircle className="h-3 w-3" />,
      label: t("knowledge.status.completed"),
      className: "bg-emerald-500/15 text-emerald-700 dark:text-emerald-400 border-emerald-500/30",
    },
    failed: {
      icon: <AlertCircle className="h-3 w-3" />,
      label: t("knowledge.status.failed"),
      className: "bg-rose-500/15 text-rose-700 dark:text-rose-400 border-rose-500/30",
    },
    queued: {
      icon: <Loader2 className="h-3 w-3" />,
      label: t("knowledge.status.queued"),
      className: "bg-slate-500/15 text-slate-700 dark:text-slate-400 border-slate-500/30",
    },
    detecting: {
      icon: <Loader2 className="h-3 w-3 animate-spin" />,
      label: t("knowledge.status.detecting"),
      className: "bg-purple-500/15 text-purple-700 dark:text-purple-400 border-purple-500/30",
    },
    processing: {
      icon: <Loader2 className="h-3 w-3 animate-spin" />,
      label: hasPageProgress
        ? t("knowledge.status.processingPages", { current: pagesProcessed, total: totalPages })
        : progress
          ? t("knowledge.status.processingProgress", { progress: Math.round(progress) })
          : t("knowledge.status.processing"),
      className: "bg-blue-500/15 text-blue-700 dark:text-blue-400 border-blue-500/30",
    },
    embedding: {
      icon: <Loader2 className="h-3 w-3 animate-spin" />,
      label: progress
        ? t("knowledge.status.embeddingProgress", { progress: Math.round(progress) })
        : t("knowledge.status.embedding"),
      className: "bg-primary/15 text-primary border-primary/30",
    },
    embedding_images: {
      icon: <Loader2 className="h-3 w-3 animate-spin" />,
      label: progress
        ? t("knowledge.status.embeddingImagesProgress", { progress: Math.round(progress) })
        : t("knowledge.status.embeddingImages"),
      className: "bg-violet-500/15 text-violet-700 dark:text-violet-400 border-violet-500/30",
    },
    uploading_images: {
      icon: <Loader2 className="h-3 w-3 animate-spin" />,
      label: progress
        ? t("knowledge.status.uploadingImagesProgress", { progress: Math.round(progress) })
        : t("knowledge.status.uploadingImages"),
      className: "bg-indigo-500/15 text-indigo-700 dark:text-indigo-400 border-indigo-500/30",
    },
    segmenting: {
      icon: <Loader2 className="h-3 w-3 animate-spin" />,
      label: progress
        ? t("knowledge.status.segmentingProgress", { progress: Math.round(progress) })
        : t("knowledge.status.segmenting"),
      className: "bg-amber-500/15 text-amber-700 dark:text-amber-400 border-amber-500/30",
    },
    parsing: {
      icon: <Loader2 className="h-3 w-3 animate-spin" />,
      label: progress
        ? t("knowledge.status.parsingProgress", { progress: Math.round(progress) })
        : t("knowledge.status.parsing"),
      className: "bg-sky-500/15 text-sky-700 dark:text-sky-400 border-sky-500/30",
    },
  };

  // Migration 101 forward lifecycle aliases. Keep the visual vocabulary
  // stable while the wire format moves from queued/segmenting/embedding/
  // failed to waiting/splitting/indexing/error.
  configs.waiting = configs.queued;
  configs.splitting = configs.segmenting;
  configs.indexing = configs.embedding;
  configs.error = configs.failed;

  const config = configs[s] || {
    icon: <File className="h-3 w-3" />,
    label: t("knowledge.status.uploaded"),
    className: "bg-muted/40 text-muted-foreground border-border/60",
  };

  const badge = (
    <Badge
      variant="outline"
      className={`text-xs font-medium cursor-default ${config.className}`}
    >
      {config.icon}
      <span className="ml-1">{config.label}</span>
    </Badge>
  );

  if (s === "failed" || s === "error") {
    return (
      <TooltipProvider>
        <Tooltip>
          <TooltipTrigger asChild>{badge}</TooltipTrigger>
          <TooltipContent>
            <p className="max-w-xs wrap-break-word text-xs">{t("knowledge.detail.processingFailedSummary")}</p>
          </TooltipContent>
        </Tooltip>
      </TooltipProvider>
    );
  }

  return badge;
}
