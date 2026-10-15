/**
 * Share Page — read-only conversation snapshot with artifacts.
 *
 * Accessible at /share/:shareId (internal links require a current account).
 * Supports conversation shares from /api/v1/assistant/shares/:code.
 *
 * Phase 3 retheme: aligned to the single-accent (gold) palette shared with
 * the main /assistant surface. All hard-coded slate/gray/indigo/emerald/
 * blue/purple tones were swapped for --assistant-* tokens. Gold is not
 * used on this page at all — it shares the main app's neutral chrome so
 * an embedded <QuizCard scope="share"> carries the only gold moments.
 */
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { placeSharedArtifacts } from "@/pages/assistant/sharedArtifacts";
import { useParams } from "react-router-dom";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { ArrowRight, Download } from "lucide-react";
import { formatFileSize, getFormatLabel } from "@/lib/format";
import { QuizCard } from "@/pages/assistant/components/Quiz";
import type { QuizData } from "@/pages/assistant/types";
import { getAuthToken } from "@/lib/api";
import { useAuthStore } from "@/store/useAuthStore";

// ── Types ────────────────────────────────────────────────────────────

interface ShareMessage {
  role: "user" | "assistant";
  content: string;
  timestamp?: string;
  metadata?: Record<string, unknown>;
  /** Frozen quiz payload attached at share-creation time. Present only on
   *  assistant messages that produced a quiz via the generate_quiz tool. */
  quiz_data?: QuizData;
}

interface ShareArtifact {
  artifact_id: string;
  type: string;
  format: string;
  title: string;
  filename: string;
  size_bytes: number;
  mime_type?: string;
}

interface ConversationShareData {
  share_code: string;
  title: string;
  snapshot: {
    messages: ShareMessage[];
    artifacts: ShareArtifact[];
    model_id?: string;
    shared_at?: string;
  };
  message_count: number;
  artifact_count: number;
  view_count: number;
  created_at: string;
  expires_at: string | null;
  audience: "public" | "internal";
}

function shareRequestHeaders(): HeadersInit {
  const token = getAuthToken();
  return token ? { Authorization: `Bearer ${token}` } : {};
}

// ── Component ────────────────────────────────────────────────────────

export function SharePage() {
  const userId = useAuthStore((state) => state.user?.user_id);
  return <SharePageSession key={userId || "anonymous"} userId={userId} />;
}

function SharePageSession({ userId }: { userId?: string }) {
  const { t } = useTranslation();
  const { shareId } = useParams<{ shareId: string }>();
  const [convShare, setConvShare] = useState<ConversationShareData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    if (!shareId) {
      setError(t("assistant.sharedLinkUnavailable"));
      setLoading(false);
      return;
    }

    const controller = new AbortController();
    async function loadShare() {
      try {
        const resp = await fetch(`/api/v1/assistant/shares/${shareId}`, {
          signal: controller.signal,
          headers: shareRequestHeaders(),
        });
        if (!resp.ok) {
          throw new Error(
            resp.status === 401
              ? t("assistant.shareSignIn", "Sign in to open this internal link")
              : resp.status === 403
                ? t("assistant.shareSourceUnavailable", "You no longer have access to this link's sources")
              : resp.status === 404
              ? t("assistant.sharedLinkUnavailable")
              : resp.status === 410
                ? t("assistant.sharedLinkExpired")
              : t("assistant.sharedLinkLoadFailed"),
          );
        }
        const data = (await resp.json()) as ConversationShareData;
        setConvShare(data);
        setError(null);
      } catch (loadError) {
        if (loadError instanceof DOMException && loadError.name === "AbortError") return;
        setError(
          loadError instanceof Error
            ? loadError.message
            : t("assistant.sharedLinkLoadFailed"),
        );
      } finally {
        if (!controller.signal.aborted) setLoading(false);
      }
    }

    void loadShare();
    return () => controller.abort();
  }, [shareId, t]);

  if (loading) {
    return (
      <div className="assistant-v2 flex min-h-dvh items-center justify-center bg-[hsl(var(--assistant-canvas-bg))]">
        <div className="flex items-center gap-3 text-[13px] text-[hsl(var(--assistant-text-secondary))]" role="status">
          <div className="h-5 w-5 animate-spin rounded-full border-2 border-[hsl(var(--assistant-border))] border-t-[hsl(var(--assistant-accent))]" aria-hidden="true" />
          <span>{t("assistant.sharedLinkLoading")}</span>
        </div>
      </div>
    );
  }

  if (error || !convShare) {
    return (
      <div className="assistant-v2 flex min-h-dvh items-center justify-center bg-[hsl(var(--assistant-canvas-bg))]">
        <div className="max-w-md space-y-3 px-6 text-center" role="alert">
          <h1 className="text-[18px] font-semibold text-[hsl(var(--assistant-text-primary))]">
            {error || t("assistant.sharedLinkUnavailable")}
          </h1>
          <p className="text-[13px] text-[hsl(var(--assistant-text-secondary))]">
            {t("assistant.sharedLinkNextStep")}
          </p>
        </div>
      </div>
    );
  }

  // ── Conversation Share (new — with artifacts) ──────────────────────

  if (convShare) {
    const { snapshot, title, artifact_count, view_count, created_at } = convShare;
    const placement = placeSharedArtifacts(snapshot.artifacts, snapshot.messages);

    return (
      <div className="assistant-v2 min-h-dvh bg-[hsl(var(--assistant-canvas-bg))] text-[hsl(var(--assistant-text-primary))]">
        {/* Header */}
        <header className="sticky top-0 z-10 bg-[hsl(var(--assistant-canvas-bg)/0.85)] backdrop-blur-xs border-b border-[hsl(var(--assistant-border))]">
          <div className="mx-auto flex max-w-3xl items-center justify-between gap-3 px-4 py-3">
            <div className="flex min-w-0 items-center gap-2.5">
              <div className="w-7 h-7 rounded-full bg-[hsl(var(--assistant-surface-bg))] border border-[hsl(var(--assistant-border))] flex items-center justify-center">
                <svg
                  className="w-3.5 h-3.5 text-[hsl(var(--assistant-text-secondary))]"
                  fill="currentColor"
                  viewBox="0 0 20 20"
                  aria-hidden
                >
                  <path d="M10 2a8 8 0 100 16 8 8 0 000-16zm0 14a6 6 0 110-12 6 6 0 010 12z" />
                </svg>
              </div>
              <div className="min-w-0">
                <h1 className="truncate text-[13px] font-medium text-[hsl(var(--assistant-text-primary))]">
                  AI Assistant{snapshot.model_id ? ` · ${snapshot.model_id}` : ""}
                </h1>
                <p className="text-[11px] font-mono text-[hsl(var(--assistant-text-tertiary))] mt-0.5">
                  Shared · {snapshot.messages.length} messages
                  {convShare.audience === "internal" && ` · ${t("assistant.shareInternalShort", "Internal")}`}
                  {artifact_count > 0 && ` · ${artifact_count} files`}
                  {view_count > 0 && ` · ${view_count} views`}
                </p>
              </div>
            </div>
            <time className="shrink-0 text-[11px] font-mono text-[hsl(var(--assistant-text-tertiary))]">
              {new Date(created_at).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" })}
            </time>
          </div>
        </header>

        {/* Title */}
        {title && (
          <div className="max-w-3xl mx-auto px-4 pt-6 pb-2">
            <h2 className="text-[18px] font-semibold text-[hsl(var(--assistant-text-primary))] tracking-tight">
              {title}
            </h2>
          </div>
        )}

        {/* Messages */}
        <div className="max-w-3xl mx-auto px-4 py-4 space-y-5">
          {snapshot.messages.map((msg, i) => {
            const msgArtifacts = placement.byMessage[i];
            const isUser = msg.role === "user";
            return (
              <div
                key={i}
                className={`flex gap-3 ${isUser ? "flex-row-reverse" : "flex-row"}`}
              >
                {/* Avatar */}
                <div
                  className="w-7 h-7 rounded-full shrink-0 flex items-center justify-center text-[11px] font-medium bg-[hsl(var(--assistant-surface-bg))] border border-[hsl(var(--assistant-border))] text-[hsl(var(--assistant-text-secondary))]"
                  aria-hidden
                >
                  {isUser ? "U" : "AI"}
                </div>

                {/* Bubble / copy */}
                <div
                  className={`max-w-[80%] min-w-0 ${
                    isUser
                      ? "bg-[hsl(var(--assistant-user-bubble))] text-[hsl(var(--assistant-text-primary))] rounded-[14px] rounded-tr-sm px-3.5 py-2"
                      : "text-[hsl(var(--assistant-text-primary))]"
                  }`}
                >
                  {isUser ? (
                    <div className="text-[14px] leading-relaxed whitespace-pre-wrap wrap-break-word">
                      {msg.content}
                    </div>
                  ) : (
                    <div className="assistant-copy text-[14px] leading-relaxed prose prose-sm max-w-none [overflow-wrap:anywhere] [&_code]:[overflow-wrap:anywhere] [&_pre]:max-w-full [&_pre]:overflow-x-auto">
                      <ReactMarkdown remarkPlugins={[remarkGfm]}>{msg.content}</ReactMarkdown>
                    </div>
                  )}

                  {/* Inline artifacts */}
                  {msgArtifacts.length > 0 && (
                    <div className="mt-3 space-y-2">
                      {msgArtifacts.map((art) => (
                        <ArtifactCard key={art.artifact_id} artifact={art} shareCode={convShare.share_code} audience={convShare.audience} />
                      ))}
                    </div>
                  )}

                  {/* Inline interactive quiz — anonymous submit wired to
                       /assistant/shares/:code/quiz/:id/submit. State keyed by
                       shareCode so it never collides with the author's main-app
                       session state. */}
                  {msg.role === "assistant" && msg.quiz_data && (
                    <div className="mt-3">
                      <QuizCard
                        quizData={msg.quiz_data}
                        scope="share"
                        shareCode={convShare.share_code}
                        userScopeId={convShare.audience === "internal" ? userId || "unauthenticated" : undefined}
                      />
                    </div>
                  )}
                </div>
              </div>
            );
          })}
        </div>

        {placement.remaining.length > 0 && (
          <section className="max-w-3xl mx-auto px-4 space-y-2" aria-label={t("assistant.sharedFiles")}>
            <h2 className="text-sm font-medium">{t("assistant.sharedFiles")}</h2>
            {placement.remaining.map((artifact) => (
              <ArtifactCard key={artifact.artifact_id} artifact={artifact} shareCode={convShare.share_code} audience={convShare.audience} />
            ))}
          </section>
        )}
        {/* CTA + Footer */}
        <div className="max-w-3xl mx-auto px-4 py-6 flex justify-center">
          <a
            href="/assistant"
            className="act-btn act-hover inline-flex items-center gap-1.5 h-8 px-3 rounded-md text-[13px] font-medium text-[hsl(var(--assistant-text-secondary))] hover:text-[hsl(var(--assistant-text-primary))]"
          >
            Start a new conversation
            <ArrowRight className="w-[14px] h-[14px]" />
          </a>
        </div>
        <footer className="max-w-3xl mx-auto px-4 py-6 text-center">
          <p className="text-[11px] font-mono text-[hsl(var(--assistant-text-tertiary))]">
            Shared from AI Platform · AI-generated content
          </p>
          {convShare.expires_at && (
            <p className="text-[11px] font-mono text-[hsl(var(--assistant-text-tertiary))] mt-1">
              Expires {new Date(convShare.expires_at).toLocaleDateString()}
            </p>
          )}
        </footer>
      </div>
    );
  }

  return null;
}

// ── Inline Artifact Card ─────────────────────────────────────────────

function ArtifactCard({ artifact, shareCode, audience }: { artifact: ShareArtifact; shareCode: string; audience: "public" | "internal" }) {
  const { t } = useTranslation();
  const downloadUrl = `/api/v1/assistant/shares/${shareCode}/artifact/${artifact.artifact_id}`;
  const isImage = artifact.type === "image" || artifact.mime_type?.startsWith("image/");
  const label = getFormatLabel(artifact.format, artifact.mime_type);
  const [privateImageUrl, setPrivateImageUrl] = useState<string | null>(null);
  const [downloadError, setDownloadError] = useState(false);

  useEffect(() => {
    if (audience !== "internal" || !isImage) return;
    const controller = new AbortController();
    let objectUrl: string | null = null;
    void fetch(downloadUrl, { headers: shareRequestHeaders(), signal: controller.signal })
      .then((response) => {
        if (!response.ok) throw new Error("Artifact unavailable");
        return response.blob();
      })
      .then((blob) => {
        if (controller.signal.aborted) return;
        objectUrl = URL.createObjectURL(blob);
        setPrivateImageUrl(objectUrl);
      })
      .catch(() => { if (!controller.signal.aborted) setPrivateImageUrl(null); });
    return () => {
      controller.abort();
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [audience, downloadUrl, isImage]);

  const downloadInternal = async () => {
    setDownloadError(false);
    try {
      const response = await fetch(downloadUrl, { headers: shareRequestHeaders() });
      if (!response.ok) throw new Error("Artifact unavailable");
      const objectUrl = URL.createObjectURL(await response.blob());
      const link = document.createElement("a");
      link.href = objectUrl;
      link.download = artifact.filename;
      document.body.appendChild(link);
      link.click();
      link.remove();
      window.setTimeout(() => URL.revokeObjectURL(objectUrl), 1000);
    } catch {
      setDownloadError(true);
    }
  };

  return (
    <div className="rounded-[10px] border border-[hsl(var(--assistant-border))] overflow-hidden bg-[hsl(var(--assistant-surface-bg))]">
      {isImage && (audience === "public" || privateImageUrl) && (
        <a href={audience === "internal" ? privateImageUrl || "#" : downloadUrl} target="_blank" rel="noopener noreferrer" className="block">
          <img
            src={audience === "internal" ? privateImageUrl || undefined : downloadUrl}
            alt={artifact.title}
            className="w-full max-h-[400px] object-contain bg-[hsl(var(--assistant-surface-soft))]"
            loading="lazy"
          />
        </a>
      )}
      <div className="flex items-center gap-2.5 p-2.5">
        {/* Monospace uppercase format glyph (matches ArtifactsPanel treatment) */}
        <span
          className="shrink-0 px-1.5 py-[2px] text-[9px] font-mono font-semibold tracking-wider uppercase rounded bg-[hsl(var(--assistant-chip-bg))] text-[hsl(var(--assistant-text-tertiary))]"
          aria-hidden
        >
          {label.slice(0, 4)}
        </span>
        <div className="flex-1 min-w-0">
          <p className="text-[13px] font-medium truncate text-[hsl(var(--assistant-text-primary))]">
            {artifact.title || artifact.filename}
          </p>
          <p className="text-[11px] font-mono text-[hsl(var(--assistant-text-tertiary))]">
            {label} · {formatFileSize(artifact.size_bytes)}
          </p>
        </div>
        {audience === "internal" ? (
          <button type="button" onClick={() => void downloadInternal()}
            className="shrink-0 inline-flex items-center justify-center w-7 h-7 rounded-md text-[hsl(var(--assistant-text-secondary))] hover:bg-[hsl(var(--assistant-surface-soft))] hover:text-[hsl(var(--assistant-text-primary))] transition-colors duration-150"
            title={t("common.download")} aria-label={t("common.download")}>
            <Download className="w-[14px] h-[14px]" />
          </button>
        ) : (
          <a href={downloadUrl} download={artifact.filename}
            className="shrink-0 inline-flex items-center justify-center w-7 h-7 rounded-md text-[hsl(var(--assistant-text-secondary))] hover:bg-[hsl(var(--assistant-surface-soft))] hover:text-[hsl(var(--assistant-text-primary))] transition-colors duration-150"
            title={t("common.download")} aria-label={t("common.download")}>
            <Download className="w-[14px] h-[14px]" />
          </a>
        )}
      </div>
      {downloadError && <p role="alert" className="px-2.5 pb-2.5 text-xs text-[hsl(var(--assistant-text-secondary))]">{t("assistant.shareSourceUnavailable")}</p>}
    </div>
  );
}
