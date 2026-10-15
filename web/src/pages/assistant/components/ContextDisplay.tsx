/**
 * Context Display Component
 *
 * Shows retrieved KB context/sources in a collapsible panel.
 */

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { motion, AnimatePresence } from "framer-motion";
import { Database, FileText, ChevronDown, ExternalLink } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";
import type { RetrievedContext } from "../types";
import { citationSource, type CitationSource } from "./citationSource";
import { SourceDocumentDialog } from "./SourceDocumentDialog";

interface ContextDisplayProps {
  contexts: RetrievedContext[];
}

export function ContextDisplay({ contexts }: ContextDisplayProps) {
  const [isOpen, setIsOpen] = useState(false);
  const [visibleCounts, setVisibleCounts] = useState<Record<string, number>>({});
  const [selectedSource, setSelectedSource] = useState<CitationSource | null>(null);
  const { t } = useTranslation();

  if (!contexts || !Array.isArray(contexts) || contexts.length === 0) return null;

  const totalChunks = contexts.reduce((sum, c) => sum + (c.chunks?.length || 0), 0);

  return (
    <div className="mb-4">
      <SourceDocumentDialog source={selectedSource} onClose={() => setSelectedSource(null)} />
      <button
        onClick={() => setIsOpen(!isOpen)}
        aria-expanded={isOpen}
        className="flex items-center gap-2 text-xs font-medium text-[hsl(var(--assistant-text-secondary))] hover:text-[hsl(var(--assistant-text-primary))] transition-colors group"
      >
        <div className="flex items-center justify-center w-6 h-6 rounded-lg bg-[hsl(var(--assistant-accent-soft))] group-hover:bg-[hsl(var(--assistant-accent-soft))]/80 transition-colors">
          <Database className="h-3.5 w-3.5 text-[hsl(var(--assistant-accent))]" />
        </div>
        <span>
          {totalChunks} {t("assistant.sources", "sources")}{" "}
          {t("assistant.from", "from")} {contexts.length}{" "}
          {contexts.length > 1
            ? t("assistant.datasets", "datasets")
            : t("assistant.dataset", "dataset")}
        </span>
        <motion.div
          animate={{ rotate: isOpen ? 180 : 0 }}
          transition={{ duration: 0.2 }}
        >
          <ChevronDown className="h-3.5 w-3.5" />
        </motion.div>
      </button>

      <AnimatePresence>
        {isOpen && (
          <motion.div
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.2, ease: "easeInOut" }}
            className="overflow-hidden"
          >
            <div className="mt-3 space-y-3 p-3 rounded-2xl bg-[hsl(var(--assistant-surface-soft))] border border-[hsl(var(--assistant-border-soft))]">
              {contexts.map((ctx) => (
                <div key={ctx.dataset_id} className="space-y-2">
                  <div className="flex items-center gap-2 text-xs font-medium text-[hsl(var(--assistant-text-primary))]">
                    <FileText className="h-3.5 w-3.5 text-[hsl(var(--assistant-accent))]" />
                    <span>{ctx.dataset_name}</span>
                    <span className="text-[hsl(var(--assistant-text-tertiary))] font-normal">
                      ({(ctx.took_ms || 0).toFixed(0)}ms)
                    </span>
                  </div>
                  <div className="max-h-96 overflow-y-auto space-y-2 pl-5">
                    {(ctx.chunks || []).slice(0, visibleCounts[ctx.dataset_id] ?? 3).map((chunk, idx) => {
                      const citation = citationSource(ctx.dataset_id, chunk);
                      return (
                      <motion.div
                        key={idx}
                        initial={{ opacity: 0, x: -10 }}
                        animate={{ opacity: 1, x: 0 }}
                        transition={{ delay: Math.min(idx, 2) * 0.05 }}
                        className="text-xs p-3 rounded-xl bg-[hsl(var(--assistant-surface-bg))] border border-[hsl(var(--assistant-border-soft))]"
                      >
                        <div className="flex items-center justify-between mb-1.5">
                          <div className="flex items-center gap-1.5">
                            <Badge
                              variant="secondary"
                              className={cn(
                                "text-[9px] px-1.5 py-0 font-mono tabular-nums",
                                chunk.score >= 0.8
                                  ? "bg-[hsl(var(--assistant-accent-soft))] text-[hsl(var(--assistant-accent))]"
                                  : chunk.score >= 0.6
                                    ? "bg-[hsl(var(--assistant-surface-soft))] text-[hsl(var(--assistant-text-secondary))]"
                                    : "bg-[hsl(var(--assistant-surface-soft))] text-[hsl(var(--assistant-text-tertiary))]"
                              )}
                            >
                              {(chunk.score * 100).toFixed(0)}% match
                            </Badge>
                          </div>
                          <button type="button" onClick={() => setSelectedSource(citation)}
                            className="flex items-center gap-1 text-[hsl(var(--assistant-accent))] hover:underline">
                            <ExternalLink className="h-3 w-3" />
                            <span>{t("assistant.openSource", "Open source")}</span>
                          </button>
                        </div>
                        <div className="line-clamp-3 text-[hsl(var(--assistant-text-secondary))] leading-relaxed">
                          {chunk.content}
                        </div>
                      </motion.div>
                    );})}
                    {(ctx.chunks?.length || 0) > (visibleCounts[ctx.dataset_id] ?? 3) && (
                      <button type="button" data-testid="assistant-source-show-more"
                        onClick={() => setVisibleCounts((current) => ({
                          ...current,
                          [ctx.dataset_id]: (current[ctx.dataset_id] ?? 3) + 20,
                        }))}
                        className="text-xs text-[hsl(var(--assistant-accent))] hover:underline">
                        {t("common.expand", "Expand")}
                        {" ("}{(ctx.chunks?.length || 0) - (visibleCounts[ctx.dataset_id] ?? 3)}{")"}
                      </button>
                    )}
                  </div>
                </div>
              ))}
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}
