import { useEffect, useMemo, useRef, useState } from "react";
import { Link, useParams } from "react-router-dom";
import { Bot, Clock3, Database, MessageCirclePlus, Paperclip, Send, ShieldCheck, ThumbsDown, ThumbsUp, Wrench, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  createPublicAgentSession,
  getPublicAgent,
  streamPublicAgent,
  submitPublicAgentFeedback,
  uploadPublicAgentAttachment,
  type PublicAgentAttachment,
  type PublicAgentConfig,
} from "@/services/agentRuntime";
import type { AgentStreamEvent } from "@/types/agents";
import "./agent-public.css";

interface ChatMessage {
  id: string;
  role: "user" | "assistant";
  content: string;
  attachmentNames?: string[];
  citations?: AgentCitation[];
}

interface AgentCitation {
  datasetId?: string;
  datasetName?: string;
  count: number;
}

function eventText(event: AgentStreamEvent): string {
  if (typeof event.content === "string") return event.content;
  if (typeof event.data === "string") return event.data;
  if (event.data && typeof event.data.content === "string") return event.data.content;
  return "";
}

function eventCitation(event: AgentStreamEvent): AgentCitation | null {
  const data = event.data && typeof event.data === "object" ? event.data : {};
  const countValue = event.citation_count ?? data.citation_count;
  const count = typeof countValue === "number" ? countValue : Number(countValue || 0);
  if (!Number.isFinite(count) || count <= 0) return null;
  const datasetIdValue = event.dataset_id ?? data.dataset_id;
  const datasetNameValue = event.dataset_name ?? data.dataset_name;
  return {
    datasetId: typeof datasetIdValue === "string" ? datasetIdValue : undefined,
    datasetName: typeof datasetNameValue === "string" ? datasetNameValue : undefined,
    count,
  };
}

export function AgentHostedPage() {
  const { publicId = "" } = useParams();
  const { t } = useTranslation();
  const [config, setConfig] = useState<PublicAgentConfig | null>(null);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [draft, setDraft] = useState("");
  const [loading, setLoading] = useState(true);
  const [streaming, setStreaming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [feedback, setFeedback] = useState<-1 | 1 | null>(null);
  const [attachments, setAttachments] = useState<PublicAgentAttachment[]>([]);
  const [uploading, setUploading] = useState(false);
  const endRef = useRef<HTMLDivElement>(null);
  const fileRef = useRef<HTMLInputElement>(null);
  const abortRef = useRef<AbortController | null>(null);

  useEffect(() => {
    let active = true;
    setLoading(true);
    void getPublicAgent(publicId)
      .then((value) => {
        if (!active) return;
        setConfig(value);
        setError(null);
      })
      .catch(() => active && setError(t("agents.public.unavailable")))
      .finally(() => active && setLoading(false));
    return () => {
      active = false;
      abortRef.current?.abort();
    };
  }, [publicId, t]);

  useEffect(() => endRef.current?.scrollIntoView({ behavior: "smooth", block: "end" }), [messages]);

  const suggestions = useMemo(
    () => (Array.isArray(config?.identity.suggested_prompts)
      ? config.identity.suggested_prompts.slice(0, 4)
      : []),
    [config],
  );

  async function sendMessage(text = draft) {
    const message = text.trim();
    if (!message || streaming || !config) return;
    setDraft("");
    setError(null);
    setFeedback(null);
    const assistantId = crypto.randomUUID();
    const outgoingAttachments = attachments;
    setMessages((current) => [
      ...current,
      {
        id: crypto.randomUUID(),
        role: "user",
        content: message,
        attachmentNames: outgoingAttachments.map((item) => item.filename),
      },
      { id: assistantId, role: "assistant", content: "" },
    ]);
    setAttachments([]);
    setStreaming(true);
    const controller = new AbortController();
    abortRef.current = controller;
    try {
      const activeSession = sessionId ?? (await createPublicAgentSession(publicId)).session_id;
      if (!sessionId) setSessionId(activeSession);
      for await (const event of streamPublicAgent({
        publicId,
        sessionId: activeSession,
        message,
        attachments: outgoingAttachments,
        signal: controller.signal,
      })) {
        if (String(event.event_type ?? event.event ?? "").toLowerCase().includes("error")) {
          throw new Error(event.message || t("agents.public.streamFailed"));
        }
        const delta = eventText(event);
        const citation = eventCitation(event);
        if (delta || citation) {
          setMessages((current) => current.map((item) => (
            item.id === assistantId
              ? {
                ...item,
                content: `${item.content}${delta}`,
                citations: citation
                  ? [
                    ...(item.citations ?? []).filter((existingCitation) => (
                      (existingCitation.datasetId || existingCitation.datasetName)
                      !== (citation.datasetId || citation.datasetName)
                    )),
                    citation,
                  ]
                  : item.citations,
              }
              : item
          )));
        }
      }
      setMessages((current) => current.map((item) => (
        item.id === assistantId && !item.content
          ? { ...item, content: t("agents.public.emptyResponse") }
          : item
      )));
    } catch (cause) {
      if (!controller.signal.aborted) {
        setError(cause instanceof Error ? cause.message : t("agents.public.streamFailed"));
        setMessages((current) => current.filter((item) => item.id !== assistantId));
      }
    } finally {
      setStreaming(false);
      abortRef.current = null;
    }
  }

  async function rate(rating: -1 | 1) {
    if (!sessionId) return;
    try {
      await submitPublicAgentFeedback({ publicId, sessionId, rating });
      setFeedback(rating);
    } catch {
      // A failed rating (stale session, quota, network) previously rejected
      // unhandled and left the button unselected with no feedback to the user.
      setError(t("agents.public.feedbackFailed"));
    }
  }

  function newConversation() {
    abortRef.current?.abort();
    setSessionId(null);
    setMessages([]);
    setFeedback(null);
    setError(null);
    setAttachments([]);
  }

  async function selectAttachments(files: FileList | null) {
    if (!files?.length || !config?.attachments || uploading) return;
    setUploading(true);
    setError(null);
    try {
      const uploaded: PublicAgentAttachment[] = [];
      for (const file of Array.from(files).slice(0, 5)) {
        uploaded.push(await uploadPublicAgentAttachment({ publicId, file }));
      }
      setAttachments((current) => [...current, ...uploaded].slice(0, 5));
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("agents.public.uploadFailed"));
    } finally {
      setUploading(false);
      if (fileRef.current) fileRef.current.value = "";
    }
  }

  if (loading) {
    return <main className="agent-public-shell agent-public-state" aria-busy="true">{t("agents.public.loading")}</main>;
  }

  if (!config) {
    return (
      <main className="agent-public-shell agent-public-state" data-testid="agent-public-unavailable">
        <Bot aria-hidden="true" />
        <h1>{t("agents.public.unavailableTitle")}</h1>
        <p>{error ?? t("agents.public.unavailable")}</p>
        <Link to="/login">{t("agents.public.signIn")}</Link>
      </main>
    );
  }

  const welcome = config.identity.welcome_message || t("agents.public.defaultWelcome", { name: config.name });
  return (
    <main className="agent-public-shell" style={{ "--agent-accent": config.identity.theme_color || "#635bff" } as React.CSSProperties}>
      <header className="agent-public-header">
        <div className="agent-public-brand">
          {config.identity.icon_url
            ? <img src={config.identity.icon_url} alt="" />
            : <span aria-hidden="true"><Bot /></span>}
          <div><h1>{config.name}</h1><p>{config.description}</p></div>
        </div>
        <button type="button" onClick={newConversation} aria-label={t("agents.public.newChat")}>
          <MessageCirclePlus aria-hidden="true" />
          <span>{t("agents.public.newChat")}</span>
        </button>
        <ul className="agent-public-trust" aria-label={t("agents.public.trustSummary")}>
          <li className={config.release_gate_verified ? "is-verified" : ""}>
            <ShieldCheck aria-hidden="true" />
            {config.release_gate_verified
              ? t("agents.public.releaseVerified", { version: config.version_number })
              : t("agents.public.releaseUnverified", { version: config.version_number })}
          </li>
          <li><Wrench aria-hidden="true" />{t("agents.public.capabilityCount", { count: config.capability_count })}</li>
          <li><Database aria-hidden="true" />{t("agents.public.knowledgeCount", { count: config.knowledge_count })}</li>
          <li><ShieldCheck aria-hidden="true" />{t(`agents.public.authModes.${config.auth_mode}`)}</li>
          {config.published_at && <li><Clock3 aria-hidden="true" />{t("agents.public.updated", { date: new Date(config.published_at).toLocaleDateString() })}</li>}
        </ul>
      </header>

      <section className="agent-public-conversation" aria-label={t("agents.public.conversation")}>
        {messages.length === 0 ? (
          <div className="agent-public-welcome">
            <span aria-hidden="true"><Bot /></span>
            <h2>{welcome}</h2>
            {suggestions.length > 0 && (
              <div className="agent-public-suggestions">
                {suggestions.map((suggestion) => (
                  <button key={suggestion} type="button" onClick={() => void sendMessage(suggestion)}>{suggestion}</button>
                ))}
              </div>
            )}
          </div>
        ) : (
          <div className="agent-public-messages" aria-live="polite">
            {messages.map((message) => (
              <article key={message.id} className={`agent-public-message is-${message.role}`}>
                <span>{message.role === "user" ? t("agents.public.you") : config.name}</span>
                <p>{message.content || (streaming && message.role === "assistant" ? t("agents.public.generating") : "")}</p>
                {message.attachmentNames && message.attachmentNames.length > 0 && (
                  <ul className="agent-public-message-attachments" aria-label={t("agents.public.attachmentsLabel")}>
                    {message.attachmentNames.map((filename) => <li key={filename}>{filename}</li>)}
                  </ul>
                )}
                {message.citations && message.citations.length > 0 && (
                  <ul className="agent-public-citations" aria-label={t("agents.public.citationsLabel")}>
                    {message.citations.map((citation) => (
                      <li key={citation.datasetId || citation.datasetName || citation.count}>
                        {t("agents.public.citation", {
                          count: citation.count,
                          dataset: citation.datasetName || citation.datasetId || t("agents.public.knowledge"),
                        })}
                      </li>
                    ))}
                  </ul>
                )}
              </article>
            ))}
            {!streaming && sessionId && (
              <div className="agent-public-feedback" aria-label={t("agents.public.feedbackLabel")}>
                <button className={feedback === 1 ? "is-selected" : ""} type="button" onClick={() => void rate(1)} aria-label={t("agents.public.helpful")}><ThumbsUp /></button>
                <button className={feedback === -1 ? "is-selected" : ""} type="button" onClick={() => void rate(-1)} aria-label={t("agents.public.notHelpful")}><ThumbsDown /></button>
              </div>
            )}
            <div ref={endRef} />
          </div>
        )}
      </section>

      <footer className="agent-public-composer">
        {error && <p className="agent-public-error" role="alert">{error}</p>}
        {attachments.length > 0 && (
          <ul className="agent-public-attachment-tray" aria-label={t("agents.public.attachmentsLabel")}>
            {attachments.map((attachment) => (
              <li key={attachment.artifact_id}>
                <span>{attachment.filename}</span>
                <button
                  type="button"
                  onClick={() => setAttachments((current) => current.filter((item) => item.artifact_id !== attachment.artifact_id))}
                  aria-label={t("agents.public.removeAttachment", { filename: attachment.filename })}
                ><X /></button>
              </li>
            ))}
          </ul>
        )}
        <form onSubmit={(event) => { event.preventDefault(); void sendMessage(); }}>
          <input
            ref={fileRef}
            className="agent-public-file-input"
            type="file"
            multiple
            aria-label={t("agents.public.attach")}
            accept=".pdf,.docx,.doc,.md,.txt,.csv,.xlsx,.png,.jpg,.jpeg,.gif,.webp,.bmp"
            onChange={(event) => void selectAttachments(event.target.files)}
          />
          <button type="button" onClick={() => fileRef.current?.click()} disabled={!config.attachments || streaming || uploading} title={config.attachments ? t("agents.public.attach") : t("agents.public.attachDisabled")} aria-label={t("agents.public.attach")}><Paperclip /></button>
          <textarea rows={1} value={draft} disabled={streaming} placeholder={t("agents.public.placeholder")} onChange={(event) => setDraft(event.target.value)} onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey) {
              event.preventDefault();
              void sendMessage();
            }
          }} />
          <button className="agent-public-send" type="submit" disabled={streaming || uploading || !draft.trim()} aria-label={t("agents.public.send")}><Send /></button>
        </form>
        <small>{t("agents.public.disclaimer")}</small>
      </footer>
    </main>
  );
}

export default AgentHostedPage;
