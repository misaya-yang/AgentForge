/**
 * Document Preview Component
 *
 * Manus-style document display with:
 * - Google Doc-like embedded preview
 * - Title with document icon
 * - Markdown content rendering
 * - Expandable/collapsible sections
 * - Download and copy actions
 */

import { useState } from "react";
import { motion, AnimatePresence } from "motion/react";
import {
  FileText,
  ChevronDown,
  Download,
  Copy,
  ExternalLink,
  Check,
  MoreHorizontal,
} from "lucide-react";
import ReactMarkdown from "react-markdown";
import remarkMath from "remark-math";
import rehypeKatex from "rehype-katex";
import DOMPurify from "dompurify";
import { cn } from "@/lib/utils";
import { useLatexCopy } from "@/hooks/useLatexCopy";
import "katex/dist/katex.min.css";
import { Button } from "@/components/ui/button";
import { useTranslation } from "react-i18next";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

// =============================================================================
// Types
// =============================================================================

import { copyToClipboard } from "@/lib/clipboard";
import { artifactAccessErrorKey, downloadAssistantArtifact, openAssistantArtifact } from "@/lib/authenticatedDownload";
import { toast } from "@/hooks/use-toast";

export interface DocumentPreviewProps {
  title: string;
  content: string;
  format?: "markdown" | "text" | "html";
  downloadUrl?: string;
  className?: string;
  defaultExpanded?: boolean;
  maxHeight?: number;
}

// =============================================================================
// Document Header
// =============================================================================

function DocumentHeader({
  title,
  isExpanded,
  onToggle,
  downloadUrl,
  onCopy,
  copied,
  hasContent,
}: {
  title: string;
  isExpanded: boolean;
  onToggle: () => void;
  downloadUrl?: string;
  onCopy: () => void;
  copied: boolean;
  hasContent: boolean;
}) {
  const { t } = useTranslation();
  return (
    <div className="flex items-center justify-between px-4 py-3 bg-white dark:bg-slate-900 border-b border-slate-200 dark:border-slate-700">
      {/* Title with icon */}
      <div
        onClick={hasContent ? onToggle : undefined}
        className={cn(
          "flex items-center gap-3",
          hasContent && "cursor-pointer hover:opacity-80 transition-opacity"
        )}
      >
        <div className="w-10 h-10 rounded-lg bg-linear-to-br from-blue-500 to-blue-600 flex items-center justify-center shadow-xs">
          <FileText className="h-5 w-5 text-white" />
        </div>
        <div className="text-left">
          <h3 className="text-sm font-semibold text-slate-800 dark:text-slate-100 line-clamp-1">
            {title}
          </h3>
          <p className="text-xs text-slate-500 dark:text-slate-400">
            {t("assistant.document")}
          </p>
        </div>
        {hasContent && (
          <motion.div
            animate={{ rotate: isExpanded ? 180 : 0 }}
            transition={{ duration: 0.2 }}
            className="ml-2"
          >
            <ChevronDown className="h-4 w-4 text-slate-400" />
          </motion.div>
        )}
      </div>

      {/* Actions */}
      <div className="flex items-center gap-1">
        {/* Copy button - only show when there's content */}
        {hasContent && (
          <Button
            variant="ghost"
            size="sm"
            className="h-8 w-8 p-0"
            onClick={onCopy}
          >
            {copied ? (
              <Check className="h-4 w-4 text-emerald-500" />
            ) : (
              <Copy className="h-4 w-4 text-slate-500" />
            )}
          </Button>
        )}

        {/* Download button - always visible for documents without preview */}
        {downloadUrl && !hasContent && (
          <Button
            variant="outline"
            size="sm"
            className="h-8 px-3 gap-1.5"
            onClick={() => void downloadAssistantArtifact(downloadUrl, title).catch((error) => toast.error(t("artifact.errors.downloadTitle"), t(artifactAccessErrorKey(error))))}
          >
            <Download className="h-3.5 w-3.5" />
            <span className="text-xs">{t("common.download")}</span>
          </Button>
        )}

        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="ghost" size="sm" className="h-8 w-8 p-0">
              <MoreHorizontal className="h-4 w-4 text-slate-500" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            {downloadUrl && (
              <DropdownMenuItem onSelect={() => void downloadAssistantArtifact(downloadUrl, title).catch((error) => toast.error(t("artifact.errors.downloadTitle"), t(artifactAccessErrorKey(error))))}>
                <Download className="h-4 w-4" />
                {t("assistant.downloadDocument")}
              </DropdownMenuItem>
            )}
            {hasContent && (
              <DropdownMenuItem onClick={onCopy}>
                <Copy className="h-4 w-4 mr-2" />
                {t("common.copy")}
              </DropdownMenuItem>
            )}
            {downloadUrl && (
              <DropdownMenuItem onSelect={() => void openAssistantArtifact(downloadUrl).catch((error) => toast.error(t("artifact.errors.previewTitle"), t(artifactAccessErrorKey(error, "preview"))))}>
                <ExternalLink className="h-4 w-4" />
                {t("common.openInNewTab")}
              </DropdownMenuItem>
            )}
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </div>
  );
}

// =============================================================================
// Document Content
// =============================================================================

function DocumentContent({
  content,
  format,
  maxHeight,
}: {
  content: string;
  format: DocumentPreviewProps["format"];
  maxHeight?: number;
}) {
  return (
    <div
      className={cn(
        "px-6 py-4 overflow-auto bg-white dark:bg-slate-950",
        "prose prose-sm dark:prose-invert max-w-none",
        "prose-headings:text-slate-800 dark:prose-headings:text-slate-100",
        "prose-p:text-slate-600 dark:prose-p:text-slate-300",
        "prose-strong:text-slate-700 dark:prose-strong:text-slate-200",
        "prose-code:bg-slate-100 dark:prose-code:bg-slate-800 prose-code:px-1 prose-code:py-0.5 prose-code:rounded",
        "prose-pre:bg-slate-100 dark:prose-pre:bg-slate-800",
        "prose-li:text-slate-600 dark:prose-li:text-slate-300"
      )}
      style={{ maxHeight: maxHeight || 400 }}
    >
      {format === "markdown" ? (
        <ReactMarkdown
          remarkPlugins={[
            // Configure remark-math for inline ($) and display ($$) math
            [remarkMath, { singleDollarTextMath: true }],
          ]}
          rehypePlugins={[
            // Configure rehype-katex with htmlAndMathml for LaTeX copy support
            [rehypeKatex, { throwOnError: false, strict: false, output: "htmlAndMathml" }],
          ]}
          components={{
            // Custom heading styles
            h1: ({ children }) => (
              <h1 className="text-xl font-bold mt-6 mb-4 pb-2 border-b border-slate-200 dark:border-slate-700">
                {children}
              </h1>
            ),
            h2: ({ children }) => (
              <h2 className="text-lg font-semibold mt-5 mb-3">{children}</h2>
            ),
            h3: ({ children }) => (
              <h3 className="text-base font-semibold mt-4 mb-2">{children}</h3>
            ),
            // Custom list styles
            ul: ({ children }) => (
              <ul className="list-disc list-inside space-y-1 my-3">{children}</ul>
            ),
            ol: ({ children }) => (
              <ol className="list-decimal list-inside space-y-1 my-3">{children}</ol>
            ),
            // Custom paragraph
            p: ({ children }) => (
              <p className="my-2 leading-relaxed">{children}</p>
            ),
            // Custom blockquote
            blockquote: ({ children }) => (
              <blockquote className="border-l-4 border-blue-500 pl-4 my-4 italic text-slate-600 dark:text-slate-400">
                {children}
              </blockquote>
            ),
          }}
        >
          {content}
        </ReactMarkdown>
      ) : format === "html" ? (
        // Sanitize HTML to prevent XSS attacks
        <div dangerouslySetInnerHTML={{ __html: DOMPurify.sanitize(content) }} />
      ) : (
        <pre className="whitespace-pre-wrap font-sans">{content}</pre>
      )}
    </div>
  );
}

// =============================================================================
// Main Component
// =============================================================================

export function DocumentPreview({
  title,
  content,
  format = "markdown",
  downloadUrl,
  className,
  defaultExpanded = true,
  maxHeight = 400,
}: DocumentPreviewProps) {
  // Enable LaTeX copy support - copies original LaTeX source when selecting formulas
  useLatexCopy();

  const hasContent = Boolean(content && content.trim().length > 0);
  const [isExpanded, setIsExpanded] = useState(hasContent ? defaultExpanded : false);
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    if (content) {
      await copyToClipboard(content);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  };

  return (
    <motion.div
      initial={{ opacity: 0, y: 10 }}
      animate={{ opacity: 1, y: 0 }}
      className={cn(
        "rounded-xl border border-slate-200 dark:border-slate-700 overflow-hidden shadow-xs",
        className
      )}
    >
      {/* Header */}
      <DocumentHeader
        title={title}
        isExpanded={isExpanded}
        onToggle={() => hasContent && setIsExpanded(!isExpanded)}
        downloadUrl={downloadUrl}
        onCopy={handleCopy}
        copied={copied}
        hasContent={hasContent}
      />

      {/* Content - only show if there's actual content */}
      <AnimatePresence>
        {isExpanded && hasContent && (
          <motion.div
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.2, ease: "easeInOut" }}
          >
            <DocumentContent
              content={content}
              format={format}
              maxHeight={maxHeight}
            />
          </motion.div>
        )}
      </AnimatePresence>
    </motion.div>
  );
}

export default DocumentPreview;
