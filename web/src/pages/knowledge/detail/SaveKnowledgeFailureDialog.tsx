import { useEffect, useRef, useState } from "react";
import { isAxiosError } from "axios";
import { useTranslation } from "react-i18next";
import { BookmarkPlus, Loader2 } from "lucide-react";

import { toast } from "@/hooks/use-toast";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { useAuthStore, usePermission } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { knowledgeSaveAuthority } from "./knowledgeSaveAuthority";
import { saveKnowledgeFailureToEvalDataset } from "./kbEvalDataset";
import {
  HIT_TEST_EVAL_SOURCE,
  QA_EVAL_SOURCE,
  sourceVersionsFromHits,
  type KnowledgeFailureHit,
} from "./evalCaseStore";

interface SaveKnowledgeFailureDialogProps {
  kbDatasetId?: string;
  query: string;
  observedHits: KnowledgeFailureHit[];
  source: typeof HIT_TEST_EVAL_SOURCE | typeof QA_EVAL_SOURCE;
  sourceTraceId?: string;
  queryFingerprint?: string;
  observedAnswer?: string;
  disabled?: boolean;
  testId?: string;
}

export function SaveKnowledgeFailureDialog(props: SaveKnowledgeFailureDialogProps) {
  const { t } = useTranslation();
  const canSaveEval = usePermission("console:eval:run");
  const token = useAuthStore(state => state.token);
  const authority = useAuthStore(state => sdkAuthorityScope(state.user));
  const viewKey = JSON.stringify([token, authority, props.kbDatasetId, props.query, props.source,
    props.sourceTraceId, props.queryFingerprint, props.observedAnswer, props.disabled,
    props.observedHits.map(hit => [hit.document_id, hit.segment_id, hit.source_version, hit.source_hash])]);
  const viewRef = useRef({ key: viewKey, retired: false });
  if (viewRef.current.key !== viewKey) {
    viewRef.current.retired = true;
    viewRef.current = { key: viewKey, retired: false };
  }
  const view = viewRef.current;
  const mounted = useRef(true);
  const pending = useRef<object | null>(null);
  const [formView, setFormView] = useState(view);
  const viewIsCurrent = () => mounted.current && !view.retired && viewRef.current === view
    && useAuthStore.getState().token === token
    && sdkAuthorityScope(useAuthStore.getState().user) === authority;
  const [open, setOpen] = useState(false);
  const [saving, setSaving] = useState(false);
  const [expectedAnswer, setExpectedAnswer] = useState("");
  const [failureReason, setFailureReason] = useState("");
  const sourceCount = props.kbDatasetId
    ? sourceVersionsFromHits(props.kbDatasetId, props.observedHits).length
    : 0;
  const incompleteSources = sourceCount !== props.observedHits.length;
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; pending.current = null; };
  }, []);
  useEffect(() => {
    pending.current = null;
    setOpen(false);
    setSaving(false);
    setExpectedAnswer("");
    setFailureReason("");
    setFormView(view);
  }, [view]);

  async function save() {
    if (!viewIsCurrent() || formView !== view || pending.current || props.disabled || !open || !canSaveEval || incompleteSources || !props.kbDatasetId || !props.query.trim() || !expectedAnswer.trim() || saving) return;
    const attempt = {};
    pending.current = attempt;
    const isAttemptCurrent = () => viewIsCurrent() && pending.current === attempt;
    const guard = knowledgeSaveAuthority(isAttemptCurrent);
    setSaving(true);
    try {
      guard.assertCurrent();
      const result = await saveKnowledgeFailureToEvalDataset({
        kbDatasetId: props.kbDatasetId,
        query: props.query,
        expectedAnswer,
        observedHits: props.observedHits,
        source: props.source,
        sourceTraceId: props.sourceTraceId,
        queryFingerprint: props.queryFingerprint,
        observedAnswer: props.observedAnswer,
        failureReason,
      }, guard.isCurrent);
      if (!guard.isCurrent()) return;
      toast({
        variant: "success",
        title: result.created
          ? t("knowledge.detail.failureSavedTitle")
          : t("knowledge.detail.failureUnchangedTitle"),
        description: (
          <span>
            {result.created
              ? t("knowledge.detail.failureRevisionSaved", { revision: result.revision })
              : t("knowledge.detail.failureUnchangedText", { revision: result.revision })}{" "}
            <a
              className="underline"
              href={`/eval?dataset_id=${encodeURIComponent(props.kbDatasetId)}&tab=assets&review=pending`}
            >
              {t("knowledge.detail.openFailureReview")}
            </a>
          </span>
        ),
      });
      setOpen(false);
      setExpectedAnswer("");
      setFailureReason("");
    } catch (error: unknown) {
      if (!guard.isCurrent()) return;
      toast.error(
        t("knowledge.detail.sendToEvalFailed"),
        isAxiosError(error) && error.response?.status === 409
          ? t("knowledge.detail.failureRevisionConflict")
          : error instanceof Error ? error.message : String(error)
      );
    } finally {
      if (guard.isCurrent()) setSaving(false);
      if (pending.current === attempt) pending.current = null;
      guard.close();
    }
  }

  return (
    <>
      <Button
        size="sm"
        variant="outline"
        className="h-7 px-2 text-xs"
        disabled={props.disabled || !canSaveEval || !props.kbDatasetId || !props.query.trim()}
        onClick={() => { if (viewIsCurrent() && formView === view && !props.disabled) setOpen(true); }}
        data-testid={props.testId}
      >
        <BookmarkPlus className="mr-1 h-3.5 w-3.5" aria-hidden="true" />
        {t("knowledge.detail.saveFailureForReview")}
      </Button>
      {!canSaveEval && (
        <span role="note" className="text-xs text-muted-foreground">
          {t("knowledge.detail.failureSavePermission")}
        </span>
      )}
      <Dialog open={formView === view && open} onOpenChange={(next) => viewIsCurrent() && !saving && setOpen(next)}>
        <DialogContent className="max-w-lg" data-testid="save-knowledge-failure-dialog">
          <DialogHeader>
            <DialogTitle>{t("knowledge.detail.saveFailureForReview")}</DialogTitle>
            <DialogDescription>{t("knowledge.detail.failureReviewHint")}</DialogDescription>
          </DialogHeader>
          <div className="space-y-3">
            <div className="rounded-md bg-muted px-3 py-2 text-sm whitespace-pre-wrap">{props.query}</div>
            <p className="text-xs text-muted-foreground">
              {t("knowledge.detail.failureSourceVersions", {
                count: sourceCount,
                total: props.observedHits.length,
              })}
            </p>
            {incompleteSources && (
              <p role="alert" className="text-xs text-destructive">
                {t("knowledge.detail.failureIncompleteSources")}
              </p>
            )}
            <div className="space-y-1.5">
              <Label htmlFor="kb-failure-expected">{t("knowledge.detail.failureExpectedAnswer")}</Label>
              <Textarea
                id="kb-failure-expected"
                value={formView === view ? expectedAnswer : ""}
                onChange={(event) => setExpectedAnswer(event.target.value)}
                rows={3}
                maxLength={4000}
                disabled={saving}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="kb-failure-reason">{t("knowledge.detail.failureReason")}</Label>
              <Textarea
                id="kb-failure-reason"
                value={formView === view ? failureReason : ""}
                onChange={(event) => setFailureReason(event.target.value)}
                rows={2}
                maxLength={1000}
                disabled={saving}
              />
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)} disabled={saving}>
              {t("common.cancel")}
            </Button>
            <Button onClick={save} disabled={saving || !canSaveEval || incompleteSources || !expectedAnswer.trim()} data-testid="save-knowledge-failure">
              {saving && <Loader2 className="mr-1 h-4 w-4 animate-spin" aria-hidden="true" />}
              {t("knowledge.detail.savePendingFailure")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
