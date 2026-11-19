import { useState } from "react";
import { ChevronDown, ChevronUp } from "lucide-react";
import { useTranslation } from "react-i18next";

import type { RetrieveHit } from "@/types/knowledge";
import { Badge } from "@/components/ui/badge";
import { KnowledgeSourceButton } from "./KnowledgeSourceButton";
import { KnowledgeFeedbackControl } from "@/pages/knowledge/detail/KnowledgeFeedbackControl";

export function RetrievalResultCard({
  hit,
  index,
  highlightTerms = [],
  datasetId,
  traceId,
  queryFingerprint,
}: {
  hit: RetrieveHit;
  index: number;
  highlightTerms?: string[];
  datasetId?: string;
  traceId?: string;
  queryFingerprint?: string;
}) {
  const { t } = useTranslation();
  const [isExpanded, setIsExpanded] = useState(false);

  const highlightText = (text: string, terms: string[]) => {
    if (!terms.length || !text) return text;
    const escapedTerms = terms.map((term) => term.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
    const regex = new RegExp(`(${escapedTerms.join("|")})`, "gi");
    const parts = text.split(regex);

    return parts.map((part, i) => {
      const isMatch = terms.some((term) => term.toLowerCase() === part.toLowerCase());
      if (isMatch) {
        return (
          <mark key={i} className="bg-amber-500/25 dark:bg-amber-500/30 text-inherit px-0.5 rounded font-medium">
            {part}
          </mark>
        );
      }
      return part;
    });
  };

  const hasText = hit.text && hit.text.trim().length > 0;
  const textLength = hit.text?.length || 0;
  const showExpandButton = textLength > 200;

  const getScoreColor = (score: number) => {
    if (score >= 0.8) return "bg-emerald-500/15 text-emerald-700 dark:text-emerald-400 border-emerald-500/30";
    if (score >= 0.6) return "bg-teal-500/15 text-teal-700 dark:text-teal-400 border-teal-500/30";
    if (score >= 0.4) return "bg-amber-500/15 text-amber-700 dark:text-amber-400 border-amber-500/30";
    if (score >= 0.2) return "bg-yellow-500/15 text-yellow-700 dark:text-yellow-400 border-yellow-500/30";
    return "bg-rose-500/15 text-rose-700 dark:text-rose-400 border-rose-500/30";
  };

  const hasExactMatch = hit.metadata?._exact_match === true;
  const termMatches = hit.metadata?._term_matches as number | undefined;

  const rank = hit.metadata?._rank as number | undefined;
  const displayRank = rank || index + 1;

  const formatScore = (value: unknown): string => {
    if (value === "N/A" || value === null || value === undefined) return "N/A";
    if (typeof value === "number") return value.toFixed(4);
    if (typeof value === "string") {
      const num = parseFloat(value);
      return isNaN(num) ? value : num.toFixed(4);
    }
    return String(value);
  };

  const isScoreAvailable = (value: unknown): boolean => {
    return value !== "N/A" && value !== null && value !== undefined;
  };

  return (
    <div
      className={`bg-card rounded-xl border p-4 hover:border-primary/30 hover:shadow-xs transition-[border-color,box-shadow,background-color] duration-150 ${
        !hasText
          ? "border-rose-500/30 bg-rose-500/10"
          : hasExactMatch
            ? "border-emerald-500/30 bg-emerald-500/10 ring-2 ring-emerald-500/20"
            : "border-border/60"
      }`}
    >
      <div className="flex items-center justify-between mb-3">
        <div className="flex items-center gap-2">
          <span
            className={`inline-flex items-center justify-center w-7 h-7 rounded-lg text-white text-sm font-bold shadow-xs ${
              hasExactMatch
                ? "bg-linear-to-br from-emerald-500 to-teal-600"
                : "bg-linear-to-br from-primary to-accent"
            }`}
          >
            {displayRank}
          </span>
          <span className="text-xs text-muted-foreground font-mono">
            {hit.segment_id.slice(0, 10)}...
          </span>
          {hasExactMatch && (
            <Badge className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-400 border-emerald-500/30 text-xs font-medium">
              {`\u2713 ${t("knowledge.retrieval.exactMatch")}`}
            </Badge>
          )}
          {!hasExactMatch && termMatches !== undefined && termMatches > 0 && (
            <Badge className="bg-primary/10 text-primary border-primary/20 text-xs">
              {t("knowledge.retrieval.termMatches", { count: termMatches })}
            </Badge>
          )}
        </div>
        <Badge className={`font-mono text-xs px-2 py-1 ${getScoreColor(hit.score)}`}>
          {hit.score.toFixed(4)}
        </Badge>
      </div>

      {hasText ? (
        <div className="relative">
          <p
            className={`text-sm text-foreground/80 whitespace-pre-wrap leading-relaxed ${
              isExpanded ? "" : "line-clamp-4"
            }`}
          >
            {highlightTerms.length > 0 ? highlightText(hit.text, highlightTerms) : hit.text}
          </p>
          {showExpandButton && (
            <button
              onClick={() => setIsExpanded(!isExpanded)}
              className="mt-2 text-xs text-primary hover:text-primary/90 font-medium flex items-center gap-1"
            >
              {isExpanded ? (
                <>
                  {t("knowledge.retrieval.collapse")} <ChevronUp className="h-3 w-3" />
                </>
              ) : (
                <>
                  {t("knowledge.retrieval.expand", { count: textLength })} <ChevronDown className="h-3 w-3" />
                </>
              )}
            </button>
          )}
        </div>
      ) : (
        <p className="text-sm text-rose-500 italic">
          {`\u26A0\uFE0F ${t("knowledge.status.noTextContent")}`}
        </p>
      )}

      <div className="mt-3"><KnowledgeSourceButton datasetId={datasetId} hit={hit} /></div>

      {hit.metadata && (
        <div className="mt-3 pt-3 border-t border-border/60">
          {Array.isArray(hit.metadata._sources) && hit.metadata._sources.length > 0 && (
            <div className="mb-2 text-xs">
              <span className="text-muted-foreground font-medium">{t("knowledge.retrieval.source")} </span>
              {(hit.metadata._sources as string[]).map((src, i) => {
                const srcLower = src.toLowerCase();
                const isDense = srcLower === "dense" || srcLower === "vector";
                return (
                  <Badge
                    key={i}
                    className={`ml-1 text-xs ${
                      isDense
                        ? "bg-primary/10 text-primary border-primary/20"
                        : "bg-amber-500/15 text-amber-700 dark:text-amber-400"
                    }`}
                  >
                    {isDense ? t("knowledge.retrieval.vector") : "BM25"}
                  </Badge>
                );
              })}
            </div>
          )}

          <div className="grid grid-cols-2 md:grid-cols-4 gap-2 text-xs">
            <div className="bg-muted/60 rounded p-2">
              <div className="text-muted-foreground text-[10px] mb-1">
                {t("knowledge.retrieval.denseScore")}
              </div>
              <div
                className={`font-mono ${
                  isScoreAvailable(hit.metadata._dense_score_norm) ||
                  isScoreAvailable(hit.metadata._vector_score)
                    ? "text-primary"
                    : "text-muted-foreground"
                }`}
              >
                {formatScore(hit.metadata._dense_score_norm ?? hit.metadata._vector_score)}
              </div>
            </div>

            <div className="bg-muted/60 rounded p-2">
              <div className="text-muted-foreground text-[10px] mb-1">
                {t("knowledge.retrieval.bm25Score")}
              </div>
              <div
                className={`font-mono ${
                  isScoreAvailable(hit.metadata._bm25_score_norm) ||
                  isScoreAvailable(hit.metadata._keyword_score)
                    ? "text-amber-700 dark:text-amber-400"
                    : "text-muted-foreground"
                }`}
              >
                {formatScore(hit.metadata._bm25_score_norm ?? hit.metadata._keyword_score)}
              </div>
            </div>

            <div className="bg-muted/60 rounded p-2">
              <div className="text-muted-foreground text-[10px] mb-1">
                {t("knowledge.retrieval.fusionScore")}
              </div>
              <div
                className={`font-mono ${
                  isScoreAvailable(hit.metadata._fusion_score) ||
                  isScoreAvailable(hit.metadata._rrf_score)
                    ? "text-accent"
                    : "text-muted-foreground"
                }`}
              >
                {formatScore(hit.metadata._fusion_score ?? hit.metadata._rrf_score)}
              </div>
            </div>

            <div className="bg-muted/60 rounded p-2">
              <div className="text-muted-foreground text-[10px] mb-1">
                {t("knowledge.retrieval.rerankScore")}
              </div>
              <div
                className={`font-mono ${
                  isScoreAvailable(hit.metadata._rerank_score)
                    ? "text-rose-600"
                    : "text-muted-foreground"
                }`}
              >
                {formatScore(hit.metadata._rerank_score)}
              </div>
            </div>
          </div>

          {(isScoreAvailable(hit.metadata._mmr_score) ||
            isScoreAvailable(hit.metadata._mmr_relevance)) && (
            <div className="mt-2 text-xs text-muted-foreground">
              <span className="font-medium">MMR: </span>
              score={formatScore(hit.metadata._mmr_score)},
              relevance={formatScore(
                hit.metadata._mmr_relevance ?? hit.metadata._relevance_score
              )},
              max_sim={formatScore(hit.metadata._mmr_max_sim)}
            </div>
          )}
        </div>
      )}

      {datasetId && traceId && queryFingerprint && (
        <KnowledgeFeedbackControl
          datasetId={datasetId}
          traceId={traceId}
          queryFingerprint={queryFingerprint}
          targetType="retrieval_hit"
          segmentId={hit.segment_id}
        />
      )}
    </div>
  );
}
