import { useState } from "react";
import { Loader2, ThumbsDown, ThumbsUp } from "lucide-react";
import { useTranslation } from "react-i18next";

import { putQueryFeedback } from "@/api/knowledge";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { toast } from "@/hooks/use-toast";
import type {
  QueryFeedbackRating,
  QueryFeedbackReason,
  QueryFeedbackTarget,
} from "@/types/knowledge";

interface KnowledgeFeedbackControlProps {
  datasetId: string;
  traceId: string;
  queryFingerprint: string;
  targetType: QueryFeedbackTarget;
  segmentId?: string;
  compact?: boolean;
}

const POSITIVE_REASONS: QueryFeedbackReason[] = [
  "relevant",
  "helpful",
  "well_cited",
  "other",
];
const NEGATIVE_REASONS: QueryFeedbackReason[] = [
  "irrelevant",
  "incorrect",
  "missing_context",
  "bad_citation",
  "stale",
  "unsafe",
  "other",
];

export function KnowledgeFeedbackControl({
  datasetId,
  traceId,
  queryFingerprint,
  targetType,
  segmentId,
  compact = false,
}: KnowledgeFeedbackControlProps) {
  const { t } = useTranslation();
  const [rating, setRating] = useState<QueryFeedbackRating | null>(null);
  const [reason, setReason] = useState<QueryFeedbackReason | null>(null);
  const [comment, setComment] = useState("");
  const [saving, setSaving] = useState(false);
  const [savedRating, setSavedRating] = useState<QueryFeedbackRating | null>(null);

  function chooseRating(nextRating: QueryFeedbackRating) {
    setRating(nextRating);
    if (nextRating === "positive") {
      setReason(targetType === "retrieval_hit" ? "relevant" : "helpful");
      return;
    }
    setReason(targetType === "retrieval_hit" ? "irrelevant" : "incorrect");
  }

  async function submitFeedback() {
    if (!rating || !reason || saving) return;
    setSaving(true);
    try {
      await putQueryFeedback(datasetId, {
        trace_id: traceId,
        query_fingerprint: queryFingerprint,
        target_type: targetType,
        segment_id: targetType === "retrieval_hit" ? segmentId : undefined,
        rating,
        reason_code: reason,
        comment: comment.trim() || undefined,
      });
      setSavedRating(rating);
      setRating(null);
      setComment("");
      toast.success(t("knowledge.feedback.saved"));
    } catch (error) {
      toast.error(
        t("knowledge.feedback.saveFailed"),
        error instanceof Error ? error.message : String(error)
      );
    } finally {
      setSaving(false);
    }
  }

  const reasons = rating === "positive" ? POSITIVE_REASONS : NEGATIVE_REASONS;

  return (
    <div className={compact ? "space-y-2" : "mt-3 space-y-2 border-t border-border/60 pt-3"}>
      <div className="flex items-center gap-1">
        <span className="mr-1 text-xs text-muted-foreground">
          {t("knowledge.feedback.prompt")}
        </span>
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="h-7 w-7"
          aria-label={t("knowledge.feedback.positive")}
          aria-pressed={savedRating === "positive"}
          data-testid={`feedback-positive-${targetType}`}
          onClick={() => chooseRating("positive")}
        >
          <ThumbsUp className={savedRating === "positive" ? "h-3.5 w-3.5 text-emerald-600" : "h-3.5 w-3.5"} />
        </Button>
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="h-7 w-7"
          aria-label={t("knowledge.feedback.negative")}
          aria-pressed={savedRating === "negative"}
          data-testid={`feedback-negative-${targetType}`}
          onClick={() => chooseRating("negative")}
        >
          <ThumbsDown className={savedRating === "negative" ? "h-3.5 w-3.5 text-rose-600" : "h-3.5 w-3.5"} />
        </Button>
      </div>

      {rating && reason && (
        <div className="space-y-2 rounded-lg border border-border/60 bg-muted/30 p-2">
          <Select value={reason} onValueChange={(value) => setReason(value as QueryFeedbackReason)}>
            <SelectTrigger className="h-8 text-xs" aria-label={t("knowledge.feedback.reason")}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {reasons.map((item) => (
                <SelectItem key={item} value={item}>
                  {t(`knowledge.feedback.reasons.${item}`)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Textarea
            rows={2}
            maxLength={2000}
            value={comment}
            onChange={(event) => setComment(event.target.value)}
            placeholder={t("knowledge.feedback.commentPlaceholder")}
            className="min-h-14 text-xs"
          />
          <div className="flex justify-end gap-2">
            <Button type="button" size="sm" variant="ghost" onClick={() => setRating(null)}>
              {t("common.cancel")}
            </Button>
            <Button
              type="button"
              size="sm"
              onClick={submitFeedback}
              disabled={saving}
              data-testid={`feedback-submit-${targetType}`}
            >
              {saving && <Loader2 className="mr-1 h-3 w-3 animate-spin" />}
              {t("knowledge.feedback.submit")}
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}
