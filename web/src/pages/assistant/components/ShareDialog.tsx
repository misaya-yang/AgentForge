import { useState, useCallback, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { Share2, Copy, Check, X, ExternalLink } from "lucide-react";
import {
  createConversationShare,
  listConversationShares,
  previewConversationShare,
  revokeConversationShare,
  type ConversationSharePreview,
  type ExistingConversationShare,
  type ShareInfo,
} from "@/api/assistant";
import { toast } from "@/hooks/use-toast";

import { Dialog, DialogContent, DialogTitle, DialogDescription } from "@/components/ui/dialog";

interface ShareDialogProps {
  sessionId: string;
  messageCount: number;
  artifactCount: number;
  isOpen: boolean;
  onClose: () => void;
}

export function ShareDialog({ sessionId, messageCount, artifactCount, isOpen, onClose }: ShareDialogProps) {
  const { t } = useTranslation();
  const returnFocus = useRef<HTMLElement | null>(null);
  const [isCreating, setIsCreating] = useState(false);
  const [shareInfo, setShareInfo] = useState<ShareInfo | null>(null);
  const [copied, setCopied] = useState(false);
  const [includeArtifacts, setIncludeArtifacts] = useState(true);
  const [expiresDays, setExpiresDays] = useState<number | undefined>(7);
  const [audience, setAudience] = useState<"public" | "internal">("internal");
  const [preview, setPreview] = useState<ConversationSharePreview | null>(null);
  const [previewError, setPreviewError] = useState("");
  const [loadingPreview, setLoadingPreview] = useState(false);
  const [existingShares, setExistingShares] = useState<ExistingConversationShare[]>([]);

  useEffect(() => {
    if (!isOpen) return;
    let cancelled = false;
    setPreview(null);
    setPreviewError("");
    setLoadingPreview(true);
    void previewConversationShare(sessionId, {
      include_artifacts: includeArtifacts,
      expires_days: expiresDays,
      audience,
    }).then((value) => {
      if (!cancelled) setPreview(value);
    }).catch((error: { response?: { data?: { detail?: string } } }) => {
      if (!cancelled) setPreviewError(
        typeof error.response?.data?.detail === "string"
          ? error.response.data.detail
          : t("assistant.sharePreviewUnavailable", "Unable to verify what this link would expose."),
      );
    }).finally(() => {
      if (!cancelled) setLoadingPreview(false);
    });
    return () => { cancelled = true; };
  }, [sessionId, includeArtifacts, expiresDays, audience, isOpen, t]);

  useEffect(() => {
    if (!isOpen) return;
    void listConversationShares(sessionId).then(setExistingShares).catch(() => setExistingShares([]));
  }, [isOpen, sessionId]);

  const handleCreate = useCallback(async () => {
    if (!preview) return;
    setIsCreating(true);
    try {
      const info = await createConversationShare(sessionId, {
        include_artifacts: includeArtifacts,
        expires_days: expiresDays,
        preview_hash: preview.preview_hash,
        audience,
      });
      setShareInfo(info);
      setExistingShares(await listConversationShares(sessionId));
      toast.success(t("assistant.shareCreated", "Share link created!"));
    } catch (err) {
      toast.error(t("assistant.shareFailed", "Failed to create share link"));
      console.error("Share creation failed:", err);
    } finally {
      setIsCreating(false);
    }
  }, [sessionId, includeArtifacts, expiresDays, audience, preview, t]);

  const handleRevoke = useCallback(async (shareCode: string) => {
    try {
      await revokeConversationShare(shareCode);
      setExistingShares((current) => current.map((share) =>
        share.share_code === shareCode ? { ...share, is_active: false } : share,
      ));
      if (shareInfo?.share_code === shareCode) setShareInfo(null);
      toast.success(t("assistant.shareRevoked", "Link revoked"));
    } catch {
      toast.error(t("assistant.shareRevokeFailed", "Could not revoke this link"));
    }
  }, [shareInfo?.share_code, t]);

  const handleCopy = useCallback(async () => {
    if (!shareInfo) return;
    const fullUrl = `${window.location.origin}${shareInfo.share_url}`;
    try {
      await navigator.clipboard.writeText(fullUrl);
      setCopied(true);
      toast.success(t("assistant.linkCopied", "Link copied to clipboard"));
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // Fallback for HTTP
      const ta = document.createElement("textarea");
      ta.value = fullUrl;
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      document.body.removeChild(ta);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  }, [shareInfo, t]);

  const handleClose = useCallback(() => {
    setShareInfo(null);
    setCopied(false);
    onClose();
  }, [onClose]);

  if (!isOpen) return null;

  return (
    <Dialog open={isOpen} onOpenChange={(open) => { if (!open) handleClose(); }}>
      <DialogContent showCloseButton={false}
        className="max-w-md max-h-[90dvh] gap-0 p-0 sm:p-0 bg-white dark:bg-slate-800 rounded-2xl"
        onOpenAutoFocus={() => { returnFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null; }}
        onCloseAutoFocus={(event) => { event.preventDefault(); returnFocus.current?.focus(); }}>
        <DialogDescription className="sr-only">{audience === "internal" ? t("assistant.shareInternalPreview") : t("assistant.shareVisitorPreview")}</DialogDescription>
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-slate-200 dark:border-slate-700">
          <div className="flex items-center gap-2">
            <Share2 className="w-5 h-5 text-primary" />
            <DialogTitle className="text-lg font-semibold">{t("assistant.shareConversation", "Share Conversation")}</DialogTitle>
          </div>
          <button type="button" onClick={handleClose} className="p-1 rounded-lg hover:bg-slate-100 dark:hover:bg-slate-700" aria-label={t("common.close", "Close")}>
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Body */}
        <div className="px-6 py-5 space-y-4">
          {!shareInfo ? (
            <>
              {/* Preview */}
              <div className="p-4 rounded-xl bg-slate-50 dark:bg-slate-700/50 space-y-2">
                <p className="text-xs font-medium">{audience === "internal" ? t("assistant.shareInternalPreview") : t("assistant.shareVisitorPreview")}</p>
                <div className="flex justify-between text-sm">
                  <span className="text-muted-foreground">{t("assistant.messages", "Messages")}</span>
                  <span className="font-medium">{preview?.message_count ?? messageCount}</span>
                </div>
                {(preview?.artifact_count || artifactCount) > 0 && (
                  <div className="flex justify-between text-sm">
                    <span className="text-muted-foreground">{t("assistant.artifacts", "Artifacts")}</span>
                    <span className="font-medium">{preview?.artifact_count ?? artifactCount}</span>
                  </div>
                )}
                {loadingPreview && <p role="status" className="text-xs">{t("assistant.shareChecking", "Checking share contents…")}</p>}
                {previewError && <p role="alert" className="text-xs text-red-600">{previewError}</p>}
                {preview && (
                  <div className="max-h-40 space-y-2 overflow-y-auto border-t border-slate-200 pt-2 dark:border-slate-600">
                    {preview.messages.map((message, index) => (
                      <p key={index} className="text-xs break-words">
                        <strong>{message.role === "user" ? t("assistant.you", "You") : t("assistant.assistant", "Assistant")}:</strong>{" "}
                        <span className="whitespace-pre-wrap">{message.content}</span>
                        {message.quiz_data ? ` · ${t("assistant.sharedQuiz", "Interactive quiz")}` : ""}
                      </p>
                    ))}
                    {preview.artifacts.map((artifact) => (
                      <p key={artifact.artifact_id} className="text-xs break-words">{t("assistant.sharedFile", "File")}: {artifact.filename || artifact.title}</p>
                    ))}
                  </div>
                )}
              </div>

              {/* Include artifacts toggle */}
              <label className="flex items-center justify-between gap-2 text-sm">
                <span>{t("assistant.shareAudience", "Who can open this link")}</span>
                <select
                  value={audience}
                  onChange={(event) => { setAudience(event.target.value as "public" | "internal"); setPreview(null); }}
                  className="rounded border border-slate-300 bg-transparent px-2 py-1 dark:border-slate-600"
                >
                  <option value="internal">{t("assistant.shareInternal", "Signed-in teammates with source access")}</option>
                  <option value="public">{t("assistant.sharePublic", "Anyone with the link")}</option>
                </select>
              </label>

              <label className="flex items-center gap-3 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={includeArtifacts}
                    onChange={(e) => setIncludeArtifacts(e.target.checked)}
                    className="w-4 h-4 rounded border-slate-300"
                  />
                  <span className="text-sm">
                    {t("assistant.includeArtifacts", "Include generated files & images")}
                    <span className="text-muted-foreground ml-1">({artifactCount})</span>
                  </span>
              </label>

              <label className="flex items-center justify-between gap-2 text-sm">
                <span>{t("assistant.shareExpiry", "Link expires")}</span>
                <select
                  value={expiresDays ?? "never"}
                  onChange={(event) => setExpiresDays(event.target.value === "never" ? undefined : Number(event.target.value))}
                  className="rounded border border-slate-300 bg-transparent px-2 py-1 dark:border-slate-600"
                >
                  <option value="7">{t("assistant.shareSevenDays", "7 days")}</option>
                  <option value="30">{t("assistant.shareThirtyDays", "30 days")}</option>
                  <option value="never">{t("assistant.shareNever", "No expiry")}</option>
                </select>
              </label>

              <p className="text-xs text-muted-foreground">
                {audience === "internal"
                  ? t("assistant.shareInternalNote", "Only active teammates who can still access every source can open this link. You can revoke it at any time.")
                  : t("assistant.shareNote", "Anyone with the link can view this conversation. You can revoke the link at any time.")}
              </p>

              <button
                type="button"
                onClick={handleCreate}
                disabled={isCreating || loadingPreview || !preview}
                className="w-full py-2.5 px-4 rounded-xl bg-primary text-white font-medium hover:bg-primary/90 disabled:opacity-50 transition-colors flex items-center justify-center gap-2"
              >
                {isCreating ? (
                  <div className="w-4 h-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
                ) : (
                  <Share2 className="w-4 h-4" />
                )}
                {isCreating ? t("assistant.creating", "Creating...") : t("assistant.createShareLink", "Create Share Link")}
              </button>
            </>
          ) : (
            <>
              {/* Success state */}
              <div className="text-center space-y-3">
                <div className="w-12 h-12 rounded-full bg-green-100 dark:bg-green-900/30 flex items-center justify-center mx-auto">
                  <Check className="w-6 h-6 text-green-600" />
                </div>
                <p className="font-medium">{t("assistant.shareLinkReady", "Share link is ready!")}</p>
                <p className="text-xs text-muted-foreground">{shareInfo.audience === "internal"
                  ? t("assistant.shareInternal", "Signed-in teammates with source access")
                  : t("assistant.sharePublic", "Anyone with the link")}</p>
                <p className="text-sm text-muted-foreground">
                  {shareInfo.message_count} {t("assistant.messages", "messages")}
                  {shareInfo.artifact_count > 0 && ` · ${shareInfo.artifact_count} ${t("assistant.artifacts", "artifacts")}`}
                </p>
              </div>

              {/* URL display */}
              <div className="flex items-center gap-2 p-3 rounded-xl bg-slate-50 dark:bg-slate-700/50 border border-slate-200 dark:border-slate-600">
                <input
                  readOnly
                  value={`${window.location.origin}${shareInfo.share_url}`}
                  className="flex-1 bg-transparent text-sm font-mono truncate outline-hidden"
                />
                <button
                  type="button"
                  onClick={handleCopy}
                  className="shrink-0 p-2 rounded-lg hover:bg-slate-200 dark:hover:bg-slate-600 transition-colors"
                  aria-label={copied ? t("assistant.linkCopied", "Link copied") : t("common.copy", "Copy")}
                >
                  {copied ? <Check className="w-4 h-4 text-green-500" /> : <Copy className="w-4 h-4" />}
                </button>
                <a
                  href={shareInfo.share_url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="shrink-0 p-2 rounded-lg hover:bg-slate-200 dark:hover:bg-slate-600 transition-colors"
                  aria-label={t("common.openInNewTab", "Open share link")}
                >
                  <ExternalLink className="w-4 h-4" />
                </a>
              </div>
            </>
          )}

          {existingShares.some((share) => share.is_active) && (
            <div className="border-t border-slate-200 pt-3 dark:border-slate-700">
              <p className="mb-2 text-xs font-medium">{t("assistant.activeShareLinks", "Active links for this conversation")}</p>
              {existingShares.filter((share) => share.is_active).map((share) => (
                <div key={share.share_code} className="flex items-center justify-between gap-2 py-1 text-xs">
                  <a href={`/share/${share.share_code}`} target="_blank" rel="noreferrer" className="truncate underline">{share.share_code}</a>
                  <span className="shrink-0 text-muted-foreground">{share.audience === "internal" ? t("assistant.shareInternalShort", "Internal") : t("assistant.sharePublicShort", "Public")}</span>
                  <button type="button" onClick={() => void handleRevoke(share.share_code)} className="shrink-0 text-red-600">
                    {t("assistant.revokeShare", "Revoke")}
                  </button>
                </div>
              ))}
              <p className="mt-2 text-xs text-muted-foreground">{t("assistant.shareRevocationLimit", "Revocation stops future access. It cannot remove copies already downloaded.")}</p>
            </div>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
