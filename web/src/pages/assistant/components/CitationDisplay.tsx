/**
 * Citation Display Component
 *
 * Phase 3: Displays RAG citations with source attribution and quality indicators.
 * Shows which sources were used to generate the response with relevance scores.
 */

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { motion, AnimatePresence } from "framer-motion";
import {
  BookOpen,
  ChevronDown,
  ChevronUp,
  ExternalLink,
  CheckCircle2,
  AlertCircle,
  Info,
} from "lucide-react";
import { cn } from "@/lib/utils";
import type { RAGCitation, RAGEvaluation, RAGQualityBreakdown } from "../types";

interface CitationDisplayProps {
  citations: RAGCitation[];
  evaluation?: RAGEvaluation;
  className?: string;
}

/** Quality score indicator with color coding */
function QualityBadge({ score }: { score: number }) {
  const { t } = useTranslation();

  const getColor = () => {
    // High = accent, medium = neutral muted, low = destructive.
    // Keeps the single-accent rule (no emerald/amber distractors).
    if (score >= 80) return "bg-[hsl(var(--assistant-accent-soft))] text-[hsl(var(--assistant-accent))]";
    if (score >= 60) return "bg-[hsl(var(--assistant-surface-soft))] text-[hsl(var(--assistant-text-secondary))]";
    return "bg-[hsl(var(--destructive))]/15 text-[hsl(var(--destructive))]";
  };

  const getLabel = () => {
    if (score >= 80) return t("assistant.ragQualityHigh", "High");
    if (score >= 60) return t("assistant.ragQualityMedium", "Medium");
    return t("assistant.ragQualityLow", "Low");
  };

  return (
    <span className={cn("px-2 py-0.5 rounded-full text-[10px] font-medium", getColor())}>
      {getLabel()} ({Math.round(score)})
    </span>
  );
}

/** Single citation item */
function CitationItem({ citation, index }: { citation: RAGCitation; index: number }) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);

  const getStatusIcon = () => {
    // Used = accent-filled check, implicit = muted info, retrieved = muted alert.
    // No blue. Status differentiation comes from icon shape, not hue.
    switch (citation.status) {
      case "used":
        return <CheckCircle2 className="h-3.5 w-3.5 text-[hsl(var(--assistant-accent))]" />;
      case "implicit":
        return <Info className="h-3.5 w-3.5 text-[hsl(var(--assistant-text-secondary))]" />;
      default:
        return <AlertCircle className="h-3.5 w-3.5 text-[hsl(var(--assistant-text-tertiary))]" />;
    }
  };

  const getStatusLabel = () => {
    switch (citation.status) {
      case "used":
        return t("assistant.citationUsed", "Cited");
      case "implicit":
        return t("assistant.citationImplicit", "Used");
      default:
        return t("assistant.citationUnused", "Retrieved");
    }
  };

  return (
    <motion.div
      initial={{ opacity: 0, y: -4 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ delay: index * 0.05 }}
      className={cn(
        "border rounded-lg overflow-hidden",
        // Single-accent: "used" gets the gold accent outline, everything
        // else is neutral hairline. Semantic differentiation lives in the
        // status icon, not a rainbow of borders.
        citation.status === "used"
          ? "border-[hsl(var(--assistant-accent))]/30 bg-[hsl(var(--assistant-accent-soft))]/30"
          : "border-[hsl(var(--assistant-border-soft))] bg-[hsl(var(--assistant-surface-soft))]/40"
      )}
    >
      {/* Header */}
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full flex items-center gap-2 px-3 py-2 text-left hover:bg-[hsl(var(--assistant-surface-soft))] transition-colors"
      >
        <span className="shrink-0 w-5 h-5 flex items-center justify-center rounded bg-[hsl(var(--assistant-surface-soft))] text-[10px] font-medium text-[hsl(var(--assistant-text-secondary))] font-mono tabular-nums">
          {index + 1}
        </span>

        {getStatusIcon()}

        <div className="flex-1 min-w-0">
          <span className="text-xs font-medium text-[hsl(var(--assistant-text-primary))] truncate block">
            {citation.source_title || citation.dataset_name}
          </span>
        </div>

        <span className="text-[10px] text-[hsl(var(--assistant-text-secondary))]">
          {getStatusLabel()}
        </span>

        <span className="text-[10px] text-[hsl(var(--assistant-text-tertiary))] font-mono tabular-nums">
          {(citation.relevance_score * 100).toFixed(0)}%
        </span>

        {expanded ? (
          <ChevronUp className="h-3.5 w-3.5 text-[hsl(var(--assistant-text-tertiary))]" />
        ) : (
          <ChevronDown className="h-3.5 w-3.5 text-[hsl(var(--assistant-text-tertiary))]" />
        )}
      </button>

      {/* Expanded content */}
      <AnimatePresence>
        {expanded && (
          <motion.div
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.2 }}
            className="border-t border-[hsl(var(--assistant-border-soft))]"
          >
            <div className="px-3 py-2 space-y-2">
              {/* Source URL — accent color per single-accent rule. */}
              {citation.source_url && (
                <a
                  href={citation.source_url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="flex items-center gap-1.5 text-xs text-[hsl(var(--assistant-accent))] hover:underline"
                >
                  <ExternalLink className="h-3 w-3" />
                  <span className="truncate">{citation.source_url}</span>
                </a>
              )}

              {/* Context preview */}
              {citation.context_preview && (
                <div className="text-xs text-[hsl(var(--assistant-text-secondary))] bg-[hsl(var(--assistant-surface-bg))] rounded p-2 leading-relaxed">
                  <span className="text-[hsl(var(--assistant-text-tertiary))]">Preview: </span>
                  {citation.context_preview.length > 200
                    ? `${citation.context_preview.slice(0, 200)}...`
                    : citation.context_preview}
                </div>
              )}

              {/* Cited text — gold accent for the one that was actually used. */}
              {citation.cited_text && citation.status === "used" && (
                <div className="text-xs text-[hsl(var(--assistant-text-primary))] bg-[hsl(var(--assistant-accent-soft))]/40 rounded p-2 border-l-2 border-[hsl(var(--assistant-accent))]">
                  <span className="text-[hsl(var(--assistant-accent))] font-medium">
                    {t("assistant.citedText", "Cited")}:
                  </span>{" "}
                  {citation.cited_text}
                </div>
              )}

              {/* Metadata */}
              <div className="flex items-center gap-3 text-[10px] text-[hsl(var(--assistant-text-tertiary))]">
                <span>Dataset: {citation.dataset_name}</span>
                <span>Chunk: {citation.chunk_id.slice(0, 8)}</span>
              </div>
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </motion.div>
  );
}

/** Quality breakdown visualization */
function QualityBreakdown({ breakdown }: { breakdown: RAGQualityBreakdown }) {
  const { t } = useTranslation();

  const items = [
    { key: "relevance", label: t("assistant.ragRelevance", "Relevance"), value: breakdown.relevance, max: 25 },
    { key: "coverage", label: t("assistant.ragCoverage", "Coverage"), value: breakdown.coverage, max: 25 },
    { key: "usage", label: t("assistant.ragUsage", "Usage"), value: breakdown.usage, max: 25 },
    { key: "citations", label: t("assistant.ragCitations", "Citations"), value: breakdown.citations, max: 25 },
  ];

  return (
    <div className="grid grid-cols-4 gap-1.5">
      {items.map((item) => (
        <div key={item.key} className="text-center">
          <div className="h-1.5 bg-[hsl(var(--assistant-border))] rounded-full overflow-hidden">
            <div
              className={cn(
                "h-full rounded-full transition-all",
                item.value >= 20
                  ? "bg-[hsl(var(--assistant-accent))]"
                  : item.value >= 15
                    ? "bg-[hsl(var(--assistant-accent))]/60"
                    : "bg-[hsl(var(--destructive))]"
              )}
              style={{ width: `${(item.value / item.max) * 100}%` }}
            />
          </div>
          <span className="text-[9px] text-[hsl(var(--assistant-text-tertiary))] mt-0.5 block">
            {item.label}
          </span>
        </div>
      ))}
    </div>
  );
}

export function CitationDisplay({ citations, evaluation, className }: CitationDisplayProps) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);

  if (!citations || !Array.isArray(citations) || citations.length === 0) return null;

  // Show all citations (retrieved sources), not just "used" ones
  // This makes sense because users want to see all sources that were retrieved and considered
  const relevantCitations = citations;
  const usedCount = citations.filter((c) => c.status === "used").length;

  return (
    <div className={cn("mt-3", className)}>
      {/* Header with summary */}
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full flex items-center gap-2 px-3 py-2 rounded-lg bg-[hsl(var(--assistant-surface-soft))] hover:bg-[hsl(var(--assistant-chip-bg))] transition-colors text-left"
      >
        <BookOpen className="h-4 w-4 text-[hsl(var(--assistant-text-secondary))]" />

        <div className="flex-1 min-w-0">
          <span className="text-xs font-medium text-[hsl(var(--assistant-text-primary))]">
            {t("assistant.citationsTitle", "{{count}} Sources", { count: relevantCitations.length })}
          </span>
          {usedCount > 0 && (
            <span className="text-[10px] text-[hsl(var(--assistant-text-secondary))] ml-2">
              ({t("assistant.citedCount", "{{count}} cited", { count: usedCount })})
            </span>
          )}
        </div>

        {evaluation && <QualityBadge score={evaluation.quality_score} />}

        {expanded ? (
          <ChevronUp className="h-4 w-4 text-[hsl(var(--assistant-text-tertiary))]" />
        ) : (
          <ChevronDown className="h-4 w-4 text-[hsl(var(--assistant-text-tertiary))]" />
        )}
      </button>

      {/* Expanded content */}
      <AnimatePresence>
        {expanded && (
          <motion.div
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.2 }}
            className="overflow-hidden"
          >
            <div className="mt-2 space-y-2">
              {/* Quality breakdown */}
              {evaluation && (
                <div className="px-3 py-2 bg-[hsl(var(--assistant-surface-soft))] rounded-lg">
                  <div className="flex items-center justify-between mb-2">
                    <span className="text-[10px] font-medium text-[hsl(var(--assistant-text-secondary))]">
                      {t("assistant.ragQuality", "Response Quality")}
                    </span>
                    <span className="text-xs font-medium text-[hsl(var(--assistant-text-primary))] font-mono tabular-nums">
                      {Math.round(evaluation.quality_score)}/100
                    </span>
                  </div>
                  <QualityBreakdown breakdown={evaluation.quality_breakdown} />
                  <div className="flex items-center justify-between mt-2 text-[10px] text-[hsl(var(--assistant-text-tertiary))]">
                    <span>
                      {t("assistant.ragGrounding", "{{percent}}% grounded", {
                        percent: Math.round(evaluation.response_grounding * 100),
                      })}
                    </span>
                    <span>
                      {t("assistant.ragChunksUsed", "{{used}}/{{total}} chunks used", {
                        used: evaluation.chunks_used,
                        total: evaluation.chunks_retrieved,
                      })}
                    </span>
                  </div>
                </div>
              )}

              {/* Citation list */}
              <div className="space-y-1.5">
                {relevantCitations.map((citation, index) => (
                  <CitationItem key={citation.citation_id} citation={citation} index={index} />
                ))}
              </div>
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}
