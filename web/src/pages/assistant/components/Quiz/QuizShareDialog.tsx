/** Owner management of public Quiz links; public answers remain server-side. */
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Copy, Link2, Loader2, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogTitle, DialogDescription } from "@/components/ui/dialog";
import { createQuizShare, getQuiz, listQuizShares, revokeQuizShare, type ShareQuizResponse, type QuizShareSummary } from "@/api/quiz";
import type { QuizData } from "../../types";

interface QuizShareDialogProps { quizId: string; open: boolean; onClose: () => void }

export function QuizShareDialog({ quizId, open, onClose }: QuizShareDialogProps) {
  const { t } = useTranslation();
  const returnFocus = useRef<HTMLElement | null>(null);
  const [loading, setLoading] = useState(false);
  const [share, setShare] = useState<ShareQuizResponse | null>(null);
  const [shares, setShares] = useState<QuizShareSummary[]>([]);
  const [preview, setPreview] = useState<QuizData | null>(null);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState("");
  const [requireName, setRequireName] = useState(true);
  const [expiresDays, setExpiresDays] = useState(7);
  const [audience, setAudience] = useState<"public" | "internal">("internal");
  const [revoking, setRevoking] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setPreview(null); setShare(null); setCopied(false); setError(""); setShares([]);
    void getQuiz(quizId).then((value) => { if (!cancelled) setPreview(value); })
      .catch(() => { if (!cancelled) setError(t("assistant.sharePreviewUnavailable")); });
    void listQuizShares(quizId).then((value) => { if (!cancelled) setShares(value); })
      .catch(() => { if (!cancelled) setError(t("assistant.quiz.shareListFailed")); });
    return () => { cancelled = true; };
  }, [quizId, open, t]);

  const handleGenerate = useCallback(async () => {
    if (loading || !preview) return;
    setLoading(true); setError("");
    try {
      const result = await createQuizShare(quizId, {
        require_name: requireName,
        ...(expiresDays ? { expires_hours: expiresDays * 24 } : {}),
        audience,
      });
      setShare(result);
      setShares(await listQuizShares(quizId));
    } catch (failure) {
      const detail = (failure as { response?: { data?: { detail?: unknown } } }).response?.data?.detail;
      setError(typeof detail === "string" ? detail : t("assistant.shareFailed"));
    }
    finally { setLoading(false); }
  }, [quizId, requireName, expiresDays, audience, loading, preview, t]);

  const handleRevoke = async (id: string) => {
    if (revoking) return;
    setRevoking(id); setError("");
    try {
      await revokeQuizShare(id);
      setShares((current) => current.map((item) => item.share_id === id ? { ...item, is_active: false } : item));
      if (share?.share_id === id) setShare(null);
    } catch { setError(t("assistant.shareRevokeFailed")); }
    finally { setRevoking(null); }
  };

  const handleCopy = async () => {
    if (!share) return;
    try { await navigator.clipboard.writeText(`${window.location.origin}/quiz/${share.share_code}`); setCopied(true); }
    catch { setError(t("assistant.quiz.copyFailed")); }
  };

  return (
    <Dialog open={open} onOpenChange={(value) => { if (!value) onClose(); }}>
      <DialogContent showCloseButton={false} className="max-w-md max-h-[90dvh] gap-0 p-0 sm:p-0 rounded-2xl"
        onOpenAutoFocus={() => { returnFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null; }}
        onCloseAutoFocus={(event) => { event.preventDefault(); returnFocus.current?.focus(); }}>
        <div className="flex items-center justify-between px-5 py-4 border-b border-border">
          <div className="flex items-center gap-2"><Link2 className="w-4 h-4 text-primary" /><DialogTitle className="text-sm">{t("assistant.quiz.shareQuiz")}</DialogTitle></div>
          <button type="button" onClick={onClose} className="p-1 rounded-lg hover:bg-muted" aria-label={t("common.close")}><X className="w-4 h-4" /></button>
        </div>
        <div className="p-5 space-y-4">
          <DialogDescription>{audience === "internal" ? t("assistant.shareInternalPreview") : t("assistant.shareVisitorPreview")}</DialogDescription>
          {!share ? <>
            {preview ? <div className="max-h-40 overflow-y-auto rounded-xl border p-3 space-y-2" aria-label={t("assistant.quiz.questionPreview")}>
              <p className="text-sm font-medium">{preview.title} · {preview.question_count}</p>
              {preview.questions.map((question) => <div key={question.id} className="text-xs break-words">
                <p>{question.question_num}. {question.question_text}</p>
                {question.options.map((option) => <p key={option.label}>{option.label}. {option.text}</p>)}
              </div>)}
            </div> : <p role="status">{t("assistant.shareChecking")}</p>}
            <label className="flex items-center justify-between gap-2 text-sm">{t("assistant.shareAudience", "Who can open this link")}
              <select aria-label={t("assistant.shareAudience", "Who can open this link")} value={audience} onChange={(event) => setAudience(event.target.value as "public" | "internal")} className="rounded-md bg-background border px-2 py-1">
                <option value="internal">{t("assistant.shareInternal", "Signed-in teammates with source access")}</option>
                <option value="public">{t("assistant.sharePublic", "Anyone with the link")}</option>
              </select>
            </label>
            <label className="flex items-center gap-3 text-sm"><input type="checkbox" checked={requireName} onChange={(event) => setRequireName(event.target.checked)} />{t("assistant.quiz.requireName")}</label>
            <label className="flex items-center justify-between gap-2 text-sm">{t("assistant.shareExpiry")}
              <select aria-label={t("assistant.shareExpiry")} value={expiresDays} onChange={(event) => setExpiresDays(Number(event.target.value))} className="rounded-md bg-background border px-2 py-1">
                <option value={7}>{t("assistant.shareSevenDays")}</option><option value={30}>{t("assistant.shareThirtyDays")}</option><option value={0}>{t("assistant.shareNever")}</option>
              </select>
            </label>
            <Button onClick={() => void handleGenerate()} disabled={loading || !preview} className="w-full gap-2">{loading ? <Loader2 className="w-4 h-4 animate-spin" /> : <Link2 className="w-4 h-4" />}{t("assistant.quiz.generateLink")}</Button>
          </> : <>
            <p className="text-xs">{share.audience === "internal"
              ? t("assistant.shareInternal", "Signed-in teammates with source access")
              : t(share.require_name ? "assistant.quiz.publicAnyoneName" : "assistant.quiz.publicAnyone")}</p>
            <a href={`/quiz/${share.share_code}`} target="_blank" rel="noreferrer" className="block text-sm break-all underline">{window.location.origin}/quiz/{share.share_code}</a>
            <p className="text-xs">{t("assistant.shareExpiry")}: {share.expires_at ? new Date(share.expires_at).toLocaleString() : t("assistant.shareNever")}</p>
            <Button onClick={() => void handleCopy()} variant="outline" className="w-full gap-2">{copied ? <Check className="w-4 h-4" /> : <Copy className="w-4 h-4" />}{t(copied ? "assistant.quiz.copied" : "assistant.quiz.copyLink")}</Button>
          </>}
          {error && <p role="alert" className="text-xs text-red-500">{error}</p>}
          {shares.length > 0 && <section aria-label={t("assistant.activeShareLinks")} className="border-t pt-3 space-y-2">
            <h4 className="text-xs font-medium">{t("assistant.activeShareLinks")}</h4>
            {shares.map((item) => <div key={item.share_id} className="flex items-center gap-2 text-xs">
              <a href={`/quiz/${item.share_code}`} target="_blank" rel="noreferrer" className="min-w-0 truncate underline">{item.share_code}</a>
              <span>{item.audience === "internal" ? t("assistant.shareInternalShort", "Internal") : t("assistant.sharePublicShort", "Public")}</span>
              <span>{t(!item.is_active ? "assistant.shareRevoked" : item.expired ? "assistant.quiz.linkExpired" : "assistant.quiz.linkActive")}</span>
              {item.is_active && <Button variant="ghost" size="sm" className="ml-auto" disabled={revoking !== null} onClick={() => void handleRevoke(item.share_id)}>{t("assistant.revokeShare")}</Button>}
            </div>)}
          </section>}
          <p className="text-xs text-muted-foreground">{t("assistant.shareRevocationLimit")}</p>
        </div>
      </DialogContent>
    </Dialog>
  );
}
