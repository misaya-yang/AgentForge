/**
 * ArtifactCard - Manus-style clickable artifact component
 *
 * A rich, interactive card component for displaying generated artifacts
 * (documents, images, code) with preview capabilities, download actions,
 * and smooth animations.
 *
 * Features:
 * - Type-specific icons and colors
 * - Hover preview for images
 * - Click to open/download
 * - Progress indicator for generating artifacts
 * - Version history (optional)
 */

import * as React from "react";
import { motion, AnimatePresence } from "framer-motion";
import {
  FileText,
  Image as ImageIcon,
  Download,
  ExternalLink,
  Eye,
  Check,
  Loader2,
  FileSpreadsheet,
  File,
  FileCode,
  FileType,
  Presentation,
  X,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { formatFileSize } from "@/lib/format";
import { useTranslation } from "react-i18next";

// ============================================================================
// Types
// ============================================================================

export type ArtifactType =
  | "document"
  | "image"
  | "code"
  | "spreadsheet"
  | "presentation"
  | "pdf"
  | "file";

export type ArtifactStatus = "generating" | "ready" | "error";

export interface ArtifactData {
  id: string;
  type: ArtifactType;
  name: string;
  format?: string;
  url?: string;
  previewUrl?: string;
  content?: string;
  mimeType?: string;
  size?: number;
  status?: ArtifactStatus;
  progress?: number;
  createdAt?: Date;
  metadata?: Record<string, unknown>;
}

interface ArtifactCardProps {
  artifact: ArtifactData;
  onClick?: () => void;
  onDownload?: () => void;
  onPreview?: () => void;
  variant?: "default" | "compact" | "inline";
  showActions?: boolean;
  className?: string;
}

interface ArtifactPreviewModalProps {
  artifact: ArtifactData;
  isOpen: boolean;
  onClose: () => void;
}

// ============================================================================
// Helpers
// ============================================================================

const typeConfig: Record<
  ArtifactType,
  {
    icon: React.ElementType;
    color: string;
    bgColor: string;
    label: string;
  }
> = {
  document: {
    icon: FileText,
    color: "text-blue-600 dark:text-blue-400",
    bgColor: "bg-blue-100 dark:bg-blue-900/40",
    label: "DOC",
  },
  image: {
    icon: ImageIcon,
    color: "text-purple-600 dark:text-purple-400",
    bgColor: "bg-purple-100 dark:bg-purple-900/40",
    label: "IMG",
  },
  code: {
    icon: FileCode,
    color: "text-emerald-600 dark:text-emerald-400",
    bgColor: "bg-emerald-100 dark:bg-emerald-900/40",
    label: "CODE",
  },
  spreadsheet: {
    icon: FileSpreadsheet,
    color: "text-green-600 dark:text-green-400",
    bgColor: "bg-green-100 dark:bg-green-900/40",
    label: "XLS",
  },
  presentation: {
    icon: Presentation,
    color: "text-orange-600 dark:text-orange-400",
    bgColor: "bg-orange-100 dark:bg-orange-900/40",
    label: "PPT",
  },
  pdf: {
    icon: FileType,
    color: "text-red-600 dark:text-red-400",
    bgColor: "bg-red-100 dark:bg-red-900/40",
    label: "PDF",
  },
  file: {
    icon: File,
    color: "text-slate-600 dark:text-slate-400",
    bgColor: "bg-slate-100 dark:bg-slate-800",
    label: "FILE",
  },
};

function getArtifactType(artifact: ArtifactData): ArtifactType {
  if (artifact.type) return artifact.type;

  const format = artifact.format?.toLowerCase() || "";
  const mimeType = artifact.mimeType?.toLowerCase() || "";

  if (mimeType.startsWith("image/") || ["png", "jpg", "jpeg", "gif", "webp", "svg"].includes(format)) {
    return "image";
  }
  if (mimeType.includes("pdf") || format === "pdf") {
    return "pdf";
  }
  if (mimeType.includes("spreadsheet") || ["xlsx", "xls", "csv"].includes(format)) {
    return "spreadsheet";
  }
  if (mimeType.includes("presentation") || ["pptx", "ppt"].includes(format)) {
    return "presentation";
  }
  if (mimeType.includes("word") || ["docx", "doc", "md", "txt"].includes(format)) {
    return "document";
  }
  if (["js", "ts", "py", "java", "cpp", "go", "rs", "json", "yaml", "xml"].includes(format)) {
    return "code";
  }

  return "file";
}

function getFormatLabel(artifact: ArtifactData): string {
  if (artifact.format) return artifact.format.toUpperCase();
  const type = getArtifactType(artifact);
  return typeConfig[type].label;
}

// ============================================================================
// Preview Modal
// ============================================================================

function ArtifactPreviewModal({ artifact, isOpen, onClose }: ArtifactPreviewModalProps) {
  const { t } = useTranslation();
  const type = getArtifactType(artifact);

  if (!isOpen) return null;

  return (
    <AnimatePresence>
      <motion.div
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-xs"
        onClick={onClose}
      >
        <motion.div
          initial={{ scale: 0.95, opacity: 0 }}
          animate={{ scale: 1, opacity: 1 }}
          exit={{ scale: 0.95, opacity: 0 }}
          className="relative max-w-4xl max-h-[90vh] w-full m-4 rounded-2xl bg-white dark:bg-slate-900 shadow-2xl overflow-hidden"
          onClick={(e) => e.stopPropagation()}
        >
          {/* Header */}
          <div className="flex items-center justify-between px-4 py-3 border-b border-slate-200 dark:border-slate-800">
            <div className="flex items-center gap-3">
              <span className="text-sm font-semibold text-slate-900 dark:text-slate-100">
                {artifact.name}
              </span>
              <span className="px-2 py-0.5 text-[10px] font-bold rounded bg-slate-100 dark:bg-slate-800 text-slate-500">
                {getFormatLabel(artifact)}
              </span>
            </div>
            <button
              onClick={onClose}
              className="p-1.5 rounded-lg hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors"
            >
              <X className="h-4 w-4 text-slate-400" />
            </button>
          </div>

          {/* Content */}
          <div className="overflow-auto max-h-[calc(90vh-60px)]">
            {type === "image" && artifact.url && (
              <div className="flex items-center justify-center p-4 bg-slate-100 dark:bg-slate-800">
                <img
                  src={artifact.url}
                  alt={artifact.name}
                  className="max-w-full max-h-[70vh] object-contain rounded-lg"
                />
              </div>
            )}

            {type === "code" && artifact.content && (
              <pre className="p-4 text-sm font-mono text-slate-700 dark:text-slate-300 bg-slate-50 dark:bg-slate-800/50 overflow-auto">
                {artifact.content}
              </pre>
            )}

            {(type === "document" || type === "file") && artifact.content && (
              <div className="p-4 prose dark:prose-invert max-w-none">
                <pre className="whitespace-pre-wrap text-sm">{artifact.content}</pre>
              </div>
            )}

            {!artifact.content && !artifact.url && (
              <div className="flex flex-col items-center justify-center py-12 text-slate-400">
                <File className="h-12 w-12 mb-3 opacity-50" />
                <p className="text-sm">{t("artifact.noPreview", "Preview not available")}</p>
                {artifact.url && (
                  <a
                    href={artifact.url}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="mt-3 text-blue-500 hover:underline text-sm"
                  >
                    {t("artifact.openInNewTab", "Open in new tab")}
                  </a>
                )}
              </div>
            )}
          </div>
        </motion.div>
      </motion.div>
    </AnimatePresence>
  );
}

// ============================================================================
// Main Component
// ============================================================================

export function ArtifactCard({
  artifact,
  onClick,
  onDownload,
  onPreview,
  variant = "default",
  showActions = true,
  className,
}: ArtifactCardProps) {
  const { t } = useTranslation();
  const [showPreviewModal, setShowPreviewModal] = React.useState(false);
  const [isHovered, setIsHovered] = React.useState(false);

  const type = getArtifactType(artifact);
  const config = typeConfig[type];
  const Icon = config.icon;
  const status = artifact.status || "ready";

  const handleClick = () => {
    if (status === "generating") return;
    onClick?.();
  };

  const handleDownload = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (onDownload) {
      onDownload();
    } else if (artifact.url) {
      window.open(artifact.url, "_blank");
    }
  };

  const handlePreview = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (onPreview) {
      onPreview();
    } else {
      setShowPreviewModal(true);
    }
  };

  // Compact variant (inline chip style)
  if (variant === "compact" || variant === "inline") {
    return (
      <>
        <motion.button
          whileHover={{ scale: 1.02 }}
          whileTap={{ scale: 0.98 }}
          onClick={handleClick}
          disabled={status === "generating"}
          className={cn(
            "inline-flex items-center gap-2 px-3 py-1.5 rounded-lg text-sm font-medium",
            "border border-slate-200 dark:border-slate-700",
            "bg-white dark:bg-slate-800/50",
            "hover:bg-slate-50 dark:hover:bg-slate-800",
            "hover:border-slate-300 dark:hover:border-slate-600",
            "transition-all duration-200",
            status === "generating" && "opacity-70 cursor-wait",
            className
          )}
        >
          {status === "generating" ? (
            <Loader2 className={cn("h-4 w-4 animate-spin", config.color)} />
          ) : (
            <Icon className={cn("h-4 w-4", config.color)} />
          )}
          <span className="truncate max-w-[150px]">{artifact.name}</span>
          {status === "ready" && artifact.size && (
            <span className="text-xs text-slate-400">
              {formatFileSize(artifact.size)}
            </span>
          )}
        </motion.button>

        <ArtifactPreviewModal
          artifact={artifact}
          isOpen={showPreviewModal}
          onClose={() => setShowPreviewModal(false)}
        />
      </>
    );
  }

  // Default variant (full card)
  return (
    <>
      <motion.div
        initial={{ opacity: 0, y: 8 }}
        animate={{ opacity: 1, y: 0 }}
        whileHover={{ y: -2 }}
        onHoverStart={() => setIsHovered(true)}
        onHoverEnd={() => setIsHovered(false)}
        onClick={handleClick}
        className={cn(
          "group relative flex items-center gap-4 p-4 rounded-xl",
          "bg-white dark:bg-slate-800/50",
          "border border-slate-200/80 dark:border-slate-700/50",
          "hover:border-slate-300 dark:hover:border-slate-600",
          "hover:shadow-lg hover:shadow-slate-200/50 dark:hover:shadow-black/20",
          "transition-all duration-300",
          status === "generating" && "opacity-80",
          onClick && status === "ready" && "cursor-pointer",
          className
        )}
      >
        {/* Type badge */}
        <div
          className={cn(
            "flex items-center justify-center w-12 h-12 rounded-xl",
            "transition-transform duration-300",
            config.bgColor,
            isHovered && "scale-105"
          )}
        >
          {status === "generating" ? (
            <Loader2 className={cn("h-6 w-6 animate-spin", config.color)} />
          ) : (
            <Icon className={cn("h-6 w-6", config.color)} />
          )}
        </div>

        {/* Info */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <p className="text-sm font-semibold text-slate-900 dark:text-slate-100 truncate">
              {artifact.name}
            </p>
            {status === "ready" && (
              <motion.div
                initial={{ scale: 0 }}
                animate={{ scale: 1 }}
                className="shrink-0"
              >
                <Check className="h-3.5 w-3.5 text-green-500" />
              </motion.div>
            )}
          </div>

          <div className="flex items-center gap-2 mt-1">
            <span className="text-[10px] font-bold px-1.5 py-0.5 rounded bg-slate-100 dark:bg-slate-800 text-slate-500">
              {getFormatLabel(artifact)}
            </span>
            {artifact.size && (
              <span className="text-xs text-slate-400 dark:text-slate-500">
                {formatFileSize(artifact.size)}
              </span>
            )}
            {status === "generating" && artifact.progress !== undefined && (
              <span className="text-xs text-blue-500">
                {Math.round(artifact.progress * 100)}%
              </span>
            )}
          </div>

          {/* Progress bar for generating */}
          {status === "generating" && artifact.progress !== undefined && (
            <div className="mt-2 w-full h-1 bg-slate-100 dark:bg-slate-700 rounded-full overflow-hidden">
              <motion.div
                initial={{ width: 0 }}
                animate={{ width: `${artifact.progress * 100}%` }}
                className="h-full bg-blue-500 rounded-full"
              />
            </div>
          )}
        </div>

        {/* Actions */}
        {showActions && status === "ready" && (
          <div className="flex items-center gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
            {(type === "image" || artifact.content) && (
              <button
                onClick={handlePreview}
                className="p-2 rounded-lg hover:bg-slate-100 dark:hover:bg-slate-700 transition-colors"
                title={t("artifact.preview", "Preview")}
              >
                <Eye className="h-4 w-4 text-slate-500" />
              </button>
            )}
            {artifact.url && (
              <button
                onClick={handleDownload}
                className="p-2 rounded-lg hover:bg-slate-100 dark:hover:bg-slate-700 transition-colors"
                title={t("artifact.download", "Download")}
              >
                <Download className="h-4 w-4 text-slate-500" />
              </button>
            )}
            {artifact.url && (
              <a
                href={artifact.url}
                target="_blank"
                rel="noopener noreferrer"
                onClick={(e) => e.stopPropagation()}
                className="p-2 rounded-lg hover:bg-slate-100 dark:hover:bg-slate-700 transition-colors"
                title={t("artifact.openInNewTab", "Open in new tab")}
              >
                <ExternalLink className="h-4 w-4 text-slate-500" />
              </a>
            )}
          </div>
        )}

        {/* Image preview on hover */}
        {type === "image" && artifact.url && isHovered && (
          <motion.div
            initial={{ opacity: 0, scale: 0.9 }}
            animate={{ opacity: 1, scale: 1 }}
            className="absolute -top-2 right-0 translate-x-full ml-4 z-10 hidden lg:block"
          >
            <div className="w-48 rounded-lg overflow-hidden shadow-xl border border-slate-200 dark:border-slate-700 bg-white dark:bg-slate-800">
              <img
                src={artifact.previewUrl || artifact.url}
                alt={artifact.name}
                className="w-full h-auto"
              />
            </div>
          </motion.div>
        )}
      </motion.div>

      <ArtifactPreviewModal
        artifact={artifact}
        isOpen={showPreviewModal}
        onClose={() => setShowPreviewModal(false)}
      />
    </>
  );
}

// ============================================================================
// Artifact List Component
// ============================================================================

interface ArtifactListProps {
  artifacts: ArtifactData[];
  onArtifactClick?: (artifact: ArtifactData) => void;
  variant?: "default" | "compact" | "inline";
  className?: string;
}

export function ArtifactList({
  artifacts,
  onArtifactClick,
  variant = "default",
  className,
}: ArtifactListProps) {
  const { t } = useTranslation();

  if (artifacts.length === 0) {
    return null;
  }

  if (variant === "inline") {
    return (
      <div className={cn("flex flex-wrap gap-2", className)}>
        {artifacts.map((artifact) => (
          <ArtifactCard
            key={artifact.id}
            artifact={artifact}
            variant="inline"
            onClick={() => onArtifactClick?.(artifact)}
          />
        ))}
      </div>
    );
  }

  return (
    <div className={cn("space-y-3", className)}>
      <h4 className="text-xs font-semibold text-slate-500 dark:text-slate-400 uppercase tracking-wider">
        {t("artifact.files", "Files")} ({artifacts.length})
      </h4>
      <div className="space-y-2">
        {artifacts.map((artifact) => (
          <ArtifactCard
            key={artifact.id}
            artifact={artifact}
            variant={variant}
            onClick={() => onArtifactClick?.(artifact)}
          />
        ))}
      </div>
    </div>
  );
}

export default ArtifactCard;
