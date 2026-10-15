import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Alert,
  Button,
  Input,
  Select,
  Spin,
  Tag,
  Typography,
} from "antd";
import {
  Bot,
  BookOpen,
  ExternalLink,
  MessageSquarePlus,
  RotateCcw,
  Send,
  Trash2,
  User,
  Wrench,
} from "lucide-react";
import { Link } from "react-router-dom";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";

import {
  agentErrorDetail,
  createDraftPreviewSession,
  createVersionPreviewSession,
  streamAgentPreview,
} from "@/api/agents";
import { cancelTask, getArtifactDownloadUrl, getAssistantRunStatus, getSessionArtifacts, getSessionHistory, type ArtifactInfo } from "@/api/assistant";
import { decideAgentRuntimeApproval, getAgentRuntimeApproval, getAgentRuntimeThread, getAgentRuntimeV2RunSnapshot, type AgentApprovalPreview } from "@/api/agentThreads";
import { api } from "@/lib/api";
import { artifactAccessErrorKey, downloadAssistantArtifact } from "@/lib/authenticatedDownload";
import type {
  AgentRuntimeSession,
  AgentSpec,
  AgentStreamEvent,
  AgentVersion,
} from "@/types/agents";
import {
  agentPreviewEventData,
  agentPreviewEventText,
  agentPreviewToolActivityId,
} from "./agentPreviewEvents";
import { downloadPreviewArtifact, latestPreviewRunId, pollPreviewOutcome, previewArtifactCanDownload, previewCanStartCurrentDraft, previewHistoryMessages, previewLocator, previewLocatorKey, previewRunIsTerminal, previewSessionActionsBlocked, readPreviewLocator } from "./agentPreviewRecovery";

const { Paragraph, Text, Title } = Typography;

interface PreviewMessage {
  id: string;
  role: "user" | "assistant";
  content: string;
}

interface PreviewActivity {
  id: string;
  kind: "tool" | "knowledge";
  title: string;
  detail: string;
  status?: string;
}

interface PreviewHistory {
  session: AgentRuntimeSession | null;
  spec: AgentSpec | null;
  messages: PreviewMessage[];
  activities: PreviewActivity[];
  artifacts: ArtifactInfo[];
  error: string | null;
  runId: string | null;
  runUnsettled: boolean;
  cancelRequested: boolean;
}

interface PendingApproval {
  threadId: string;
  approvalId: string;
  preview: AgentApprovalPreview | null;
}

interface PreviewPin {
  session_id: string;
  agent_id: string;
  agent_version_id: string | null;
  draft_revision: number | null;
  channel: "preview";
}

interface RecoveredRun {
  status: string;
  approval: PendingApproval | null;
}

async function readPreviewRun(sessionId: string, runId: string): Promise<RecoveredRun> {
  const { run } = await getAssistantRunStatus(runId);
  if (run.run_id !== runId || run.session_id !== sessionId) {
    throw new Error("Preview run does not belong to this session");
  }
  const status = String(run.status || "unknown");
  if (["succeeded", "completed", "failed", "cancelled"].includes(status)) {
    return { status, approval: null };
  }
  const threadId = run.harness_thread_id;
  if (!threadId) return { status, approval: null };
  const thread = await getAgentRuntimeThread(threadId);
  if (thread.session_id !== sessionId) {
    throw new Error("Preview Runtime thread does not belong to this session");
  }
  const snapshot = await getAgentRuntimeV2RunSnapshot(threadId, runId);
  if (snapshot.terminalStatus) return { status: snapshot.terminalStatus, approval: null };
  const pending = snapshot.pendingApproval;
  if (!pending) return { status, approval: null };
  if (pending.threadId && pending.threadId !== threadId) {
    throw new Error("Preview approval does not belong to this Runtime thread");
  }
  const approval = await getAgentRuntimeApproval(threadId, pending.approvalId);
  const approvalRunId = (approval.approval as { run_id?: string }).run_id;
  if (approvalRunId && approvalRunId !== runId) {
    throw new Error("Preview approval does not belong to this run");
  }
  return {
    status,
    approval: approval.approval.status === "pending"
      ? { threadId, approvalId: pending.approvalId, preview: approval.preview }
      : null,
  };
}

interface AgentPreviewPanelProps {
  agentId: string;
  agentName: string;
  draftRevision: number;
  versions: AgentVersion[];
  savedSpec: AgentSpec;
  dirty: boolean;
}

function eventType(event: AgentStreamEvent): string {
  return String(event.event_type || event.event || "");
}

function previewErrorMessage(error: unknown, t: TFunction): string {
  const detail = agentErrorDetail(error);
  const code = detail.code || "";
  if (code.includes("MODEL")) return t("agents.preview.errors.model", { message: detail.message });
  if (code.includes("SKILL") || code.includes("CAPABILITY") || code.includes("MCP")) {
    return t("agents.preview.errors.capability", { message: detail.message });
  }
  if (code.includes("KNOWLEDGE") || code.includes("DATASET")) {
    return t("agents.preview.errors.knowledge", { message: detail.message });
  }
  if (code.includes("REVISION") || code.includes("VERSION")) {
    return t("agents.preview.errors.configuration", { message: detail.message });
  }
  if (code.includes("FORBIDDEN") || code.includes("PERMISSION") || code.includes("AUTH")) {
    return t("agents.preview.errors.permission", { message: detail.message });
  }
  if (code.includes("PROVIDER")) {
    return t("agents.preview.errors.provider", { message: detail.message });
  }
  if (code.includes("RUNTIME")) {
    return t("agents.preview.errors.runtime", { message: detail.message });
  }
  return detail.message;
}

export function AgentPreviewPanel({
  agentId,
  agentName,
  draftRevision,
  versions,
  savedSpec,
  dirty,
}: AgentPreviewPanelProps) {
  const { t } = useTranslation();
  const [restoredLocator] = useState(() => {
    try {
      return readPreviewLocator(window.sessionStorage.getItem(previewLocatorKey(agentId)), agentId);
    } catch {
      return null;
    }
  });
  const [target, setTarget] = useState(() => restoredLocator?.agent_version_id
    ?? (restoredLocator?.draft_revision && restoredLocator.draft_revision !== draftRevision
      ? `draft:${restoredLocator.draft_revision}` : "draft"));
  const [session, setSession] = useState<AgentRuntimeSession | null>(null);
  const [sessionSpec, setSessionSpec] = useState<AgentSpec | null>(null);
  const [messages, setMessages] = useState<PreviewMessage[]>([]);
  const [activities, setActivities] = useState<PreviewActivity[]>([]);
  const [artifacts, setArtifacts] = useState<ArtifactInfo[]>([]);
  const [artifactError, setArtifactError] = useState<string | null>(null);
  const [input, setInput] = useState("");
  const [starting, setStarting] = useState(false);
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [history, setHistory] = useState<Record<string, PreviewHistory>>({});
  const [restoring, setRestoring] = useState(Boolean(restoredLocator));
  const [refreshingRun, setRefreshingRun] = useState(false);
  const [runId, setRunId] = useState<string | null>(null);
  const [runUnsettled, setRunUnsettled] = useState(false);
  const [approval, setApproval] = useState<PendingApproval | null>(null);
  const [decidingApproval, setDecidingApproval] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [cancelRequested, setCancelRequested] = useState(false);
  const abortRef = useRef<AbortController | null>(null);
  const sessionIdRef = useRef<string | null>(null);
  sessionIdRef.current = session?.session_id ?? null;
  const selectedVersion = useMemo(
    () => versions.find((version) => version.agent_version_id === target),
    [target, versions],
  );
  const effectiveSpec = session ? sessionSpec : selectedVersion?.spec ?? (target === "draft" ? savedSpec : null);
  const memoryMode = String(effectiveSpec?.memory.mode || "session");
  const memoryModeLabel = t(`agents.studio.memory.${memoryMode === "user" ? "user" : memoryMode === "off" ? "off" : "session"}`);
  const targetLabel = selectedVersion
    ? t("agents.common.versionLabel", { version: selectedVersion.version_number })
    : t("agents.common.draftLabel", { revision: target.startsWith("draft:") ? Number(target.slice(6)) : draftRevision });
  const pinnedRevision = session?.draft_revision;
  const staleDraft = Boolean(session && pinnedRevision && pinnedRevision !== draftRevision);
  const effectiveNative = (session as AgentRuntimeSession & { effective_capabilities?: Array<{ name: string; risk: string; requires_confirmation: boolean }> } | null)?.effective_capabilities;
  const hasNativeBindings = Boolean(effectiveSpec?.capabilities.some((binding) => binding.type === "native" || binding.type === "model_native"));
  const sessionActionsBlocked = previewSessionActionsBlocked({
    restoring, starting, sending, runUnsettled, decidingApproval, cancelling, refreshingRun,
  });
  const canStartCurrentDraft = previewCanStartCurrentDraft({
    target, pinnedDraftRevision: session?.draft_revision ?? null, currentDraftRevision: draftRevision,
    restoring, starting, sending, decidingApproval, cancelling, refreshingRun,
    approvalPending: Boolean(approval),
  });
  const previousDrafts = Array.from(new Set([
    ...Object.keys(history).filter((key) => key.startsWith("draft:")),
    ...(target.startsWith("draft:") ? [target] : []),
  ])).sort((left, right) => Number(right.slice(6)) - Number(left.slice(6)));

  const refreshArtifactFacts = useCallback(async (sessionId: string) => {
    try {
      const facts = await getSessionArtifacts(sessionId);
      if (sessionIdRef.current !== sessionId) return;
      setArtifacts(facts.filter((artifact) => artifact.source !== "user"));
      setArtifactError(null);
    } catch {
      if (sessionIdRef.current === sessionId) {
        setArtifactError(t("agents.preview.artifactsUnavailable", "Generated files could not be verified. Refresh run status before relying on the output."));
      }
    }
  }, [t]);

  useEffect(() => {
    setHistory((current) => {
      const oldDraft = current.draft;
      if (!oldDraft?.session?.draft_revision || oldDraft.session.draft_revision === draftRevision) return current;
      const next = { ...current, [`draft:${oldDraft.session.draft_revision}`]: oldDraft };
      delete next.draft;
      return next;
    });
    if (target === "draft" && session?.draft_revision && session.draft_revision !== draftRevision) {
      // Keep a dispatched run and its r1 transcript visible after saving r2.
      setTarget(`draft:${session.draft_revision}`);
    }
  }, [draftRevision, session?.draft_revision, target]);

  useEffect(() => {
    if (!restoredLocator) return;
    let active = true;
    void (async () => {
      try {
        const { data: pin } = await api.get<PreviewPin>(
          `/api/v1/agents/${encodeURIComponent(agentId)}/preview/sessions/${encodeURIComponent(restoredLocator.session_id)}`,
        );
        if (pin.session_id !== restoredLocator.session_id || pin.agent_id !== agentId || pin.channel !== "preview") {
          throw new Error("Preview session pin mismatch");
        }
        const result = await getSessionHistory(pin.session_id);
        if (!active) return;
        setSession({ ...pin, publication_id: null, runtime_fingerprint: "", request_id: "" });
        setTarget(pin.agent_version_id ?? (pin.draft_revision === draftRevision ? "draft" : `draft:${pin.draft_revision}`));
        setSessionSpec(pin.agent_version_id
          ? versions.find((version) => version.agent_version_id === pin.agent_version_id)?.spec ?? null
          : pin.draft_revision === draftRevision ? savedSpec : null);
        setMessages(previewHistoryMessages(result.messages));
        try {
          const facts = await getSessionArtifacts(pin.session_id);
          if (active) setArtifacts(facts.filter((artifact) => artifact.source !== "user"));
        } catch {
          if (active) setArtifactError(t("agents.preview.artifactsUnavailable", "Generated files could not be verified. Refresh run status before relying on the output."));
        }
        if (!active) return;
        const latestRunId = latestPreviewRunId(result.messages);
        if (!latestRunId) return;
        setRunId(latestRunId);
        try {
          const recovered = await readPreviewRun(pin.session_id, latestRunId);
          if (!active) return;
          setApproval(recovered.approval);
          setRunUnsettled(!previewRunIsTerminal(recovered.status));
          if (["failed", "cancelled", "timeout"].includes(recovered.status)) {
            setError(t("agents.preview.runNotSuccessful", { status: recovered.status, defaultValue: "Preview ended with status {{status}}. Check the trace before publishing." }));
          }
        } catch {
          if (!active) return;
          setRunUnsettled(true);
          setError(t("agents.preview.statusUnavailable", "The saved run status could not be verified. Refresh its status or start a new Preview."));
        }
      } catch {
        if (!active) return;
        try { window.sessionStorage.removeItem(previewLocatorKey(agentId)); } catch { /* unavailable storage */ }
        setError(t("agents.preview.restoreFailed", "The saved Preview session is no longer available. Start a new isolated Preview."));
      } finally {
        if (active) setRestoring(false);
      }
    })();
    return () => { active = false; };
    // Restore the locator once. A later draft save must not reload or replace its pinned history.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agentId, restoredLocator]);

  useEffect(() => {
    if (restoring) return;
    try {
      const key = previewLocatorKey(agentId);
      if (session) window.sessionStorage.setItem(key, JSON.stringify(previewLocator(session)));
      else window.sessionStorage.removeItem(key);
    } catch { /* Preview still works when browser storage is disabled. */ }
  }, [agentId, restoring, session]);

  useEffect(() => () => abortRef.current?.abort(), []);

  useEffect(() => {
    if (!session || restoring || sending || !runUnsettled || approval || refreshingRun || decidingApproval || cancelling) return;
    let active = true;
    const sessionId = session.session_id;
    void pollPreviewOutcome<PendingApproval>({
      runId,
      readHistory: async () => (await getSessionHistory(sessionId)).messages,
      readRun: (id) => readPreviewRun(sessionId, id),
      isActive: () => active && sessionIdRef.current === sessionId,
      wait: () => new Promise((resolve) => window.setTimeout(resolve, 1500)),
      onRunId: (id) => setRunId(id),
      attempts: 6,
    }).then((result) => {
      if (!active || sessionIdRef.current !== sessionId) return;
      if (result.kind === "approval") {
        setApproval(result.approval);
      } else if (result.kind === "terminal") {
        setMessages(previewHistoryMessages(result.messages));
        void refreshArtifactFacts(sessionId);
        setRunUnsettled(false);
        setCancelRequested(false);
        setError(["failed", "cancelled", "timeout"].includes(result.status)
          ? t("agents.preview.runNotSuccessful", { status: result.status, defaultValue: "Preview ended with status {{status}}. Check the trace before publishing." })
          : null);
      } else if (result.kind === "unavailable") {
        setError(t("agents.preview.statusUnavailable", "The saved run status could not be verified. Refresh its status or start a new Preview."));
      }
    });
    return () => { active = false; };
  }, [session, restoring, sending, runUnsettled, approval, refreshingRun, decidingApproval, cancelling, runId, refreshArtifactFacts, t]);

  const startSession = async () => {
    if (sessionActionsBlocked && !canStartCurrentDraft) return;
    setStarting(true);
    setError(null);
    abortRef.current?.abort();
    if (target.startsWith("draft:")) {
      setHistory((current) => ({ ...current, [target]: { session, spec: sessionSpec, messages, activities, artifacts, error, runId, runUnsettled, cancelRequested } }));
      setTarget("draft");
    }
    setMessages([]);
    setActivities([]);
    setArtifacts([]);
    setArtifactError(null);
    setSession(null);
    setRunId(null);
    setRunUnsettled(false);
    setApproval(null);
    setCancelRequested(false);
    try {
      const created = selectedVersion
        ? await createVersionPreviewSession(agentId, selectedVersion.agent_version_id)
        : await createDraftPreviewSession(agentId, draftRevision);
      setSession(created);
      setSessionSpec(selectedVersion?.spec ?? savedSpec);
    } catch (sessionError) {
      setError(previewErrorMessage(sessionError, t));
    } finally {
      setStarting(false);
    }
  };

  const sendMessage = async () => {
    const text = input.trim();
    if (!text || sending || starting || restoring || staleDraft || runUnsettled || target.startsWith("draft:")) return;
    setSending(true);
    setError(null);
    setRunId(null);
    setRunUnsettled(false);
    setApproval(null);
    setCancelRequested(false);
    let activeSession = session;
    let assistantId: string | null = null;
    let terminal = false;
    let awaitingApproval = false;
    try {
      if (!activeSession) {
        activeSession = selectedVersion
          ? await createVersionPreviewSession(agentId, selectedVersion.agent_version_id)
          : await createDraftPreviewSession(agentId, draftRevision);
        setSession(activeSession);
        setSessionSpec(selectedVersion?.spec ?? savedSpec);
      }
      const userMessage: PreviewMessage = { id: crypto.randomUUID(), role: "user", content: text };
      assistantId = crypto.randomUUID();
      const assistantMessageId = assistantId;
      setMessages((current) => [...current, userMessage, { id: assistantMessageId, role: "assistant", content: "" }]);
      setInput("");
      const controller = new AbortController();
      abortRef.current = controller;
      for await (const event of streamAgentPreview({
        agentId,
        draftRevision: activeSession.draft_revision ?? undefined,
        versionId: selectedVersion?.agent_version_id,
        sessionId: activeSession.session_id,
        message: text,
        signal: controller.signal,
      })) {
        const type = eventType(event);
        const data = agentPreviewEventData(event);
        if (type === "run_started") {
          const id = data.run_id ?? data.task_id ?? event.run_id;
          if (typeof id === "string" && id) setRunId(id);
        }
        if (type === "approval_required") {
          awaitingApproval = true;
          setRunUnsettled(true);
          const threadId = data.thread_id ?? event.thread_id;
          const approvalId = data.approval_id ?? event.approval_id;
          if (typeof threadId === "string" && typeof approvalId === "string") {
            setApproval({ threadId, approvalId, preview: null });
            void getAgentRuntimeApproval(threadId, approvalId).then((result) => {
              setApproval((current) => current?.approvalId === approvalId
                ? { ...current, preview: result.preview } : current);
            }).catch(() => undefined);
          }
        } else if (type === "approval_result") {
          awaitingApproval = false;
          setApproval(null);
        } else if (type === "run_finished") {
          terminal = true;
          awaitingApproval = false;
          setRunUnsettled(false);
          setApproval(null);
          const status = String(data.status || event.status || "");
          if (status && !["completed", "succeeded", "success"].includes(status)) {
            setError(t("agents.preview.runNotSuccessful", { status, defaultValue: "Preview ended with status {{status}}. Check the trace before publishing." }));
          }
        } else if (type === "cancelled") {
          terminal = true;
          awaitingApproval = false;
          setRunUnsettled(false);
          setApproval(null);
          setError(t("agents.preview.cancelled", "Preview run was cancelled."));
        }
        if (type === "text_delta" || type === "text_message_content") {
          const delta = agentPreviewEventText(event);
          setMessages((current) => current.map((message) => message.id === assistantId ? { ...message, content: `${message.content}${delta}` } : message));
        } else if (type === "artifact_created") {
          // The event is a hint. Only the owner's artifact listing supplies a file fact.
          void refreshArtifactFacts(activeSession.session_id);
        } else if (type.includes("tool_call") || type === "tool_result") {
          const name = String(event.tool_name || data.tool_name || data.name || t("agents.preview.toolDefault"));
          const activityId = agentPreviewToolActivityId(event);
          const nextActivity: PreviewActivity = {
            id: activityId || crypto.randomUUID(),
            kind: "tool",
            title: t("agents.preview.toolTitle", { name }),
            detail: t("agents.preview.toolDetail"),
            status: String(event.status || data.status || t("agents.preview.toolStatus")),
          };
          setActivities((current) => {
            if (!activityId) return [...current, nextActivity];
            const existing = current.findIndex((activity) => activity.id === activityId);
            if (existing < 0) return [...current, nextActivity];
            return current.map((activity, index) => index === existing ? nextActivity : activity);
          });
        } else if (type.includes("context") || type.includes("knowledge") || type.includes("citation")) {
          const dataset = String(event.dataset_name || data.dataset_name || data.dataset_id || t("agents.preview.knowledgeDefault"));
          const rawCitationCount = event.citation_count ?? data.citation_count;
          const citationCount = typeof rawCitationCount === "number" && Number.isFinite(rawCitationCount)
            ? Math.max(0, Math.floor(rawCitationCount))
            : 0;
          setActivities((current) => [...current, {
            id: crypto.randomUUID(),
            kind: "knowledge",
            title: t("agents.preview.knowledgeTitle", { dataset }),
            detail: citationCount
              ? t(citationCount === 1 ? "agents.preview.citationOne" : "agents.preview.citationOther", { count: citationCount })
              : t("agents.preview.knowledgeDetail"),
          }]);
        } else if (type === "error" || type === "run_error") {
          terminal = true;
          setRunUnsettled(false);
          throw new Error(agentPreviewEventText(event) || t("agents.preview.runtimeError"));
        }
      }
      if (!terminal && !awaitingApproval && !controller.signal.aborted) {
        setRunUnsettled(true);
        setError(t("agents.preview.interrupted", "Preview connection ended before the run completed. Check its trace before retrying."));
      }
      if (!awaitingApproval) {
        setMessages((current) => current.map((message) => message.id === assistantId && !message.content ? { ...message, content: t("agents.preview.emptyResponse") } : message));
      }
      if (terminal) void refreshArtifactFacts(activeSession.session_id);
    } catch (streamError) {
      const aborted = streamError instanceof DOMException && streamError.name === "AbortError";
      if (!aborted) {
        if (!terminal) setRunUnsettled(true);
        setError(previewErrorMessage(streamError, t));
      }
      // The stream failed or was aborted before the emptyResponse patch ran, so
      // the placeholder assistant bubble would otherwise show a permanent
      // "Generating…" cursor. Replace its empty content with the empty-response
      // label (or leave any partially streamed text intact).
      if (assistantId) {
        const failedId = assistantId;
        setMessages((current) => current.map((message) => message.id === failedId && !message.content ? {
          ...message,
          content: aborted ? t("agents.preview.cancelled", "Preview run was cancelled.") : t("agents.preview.emptyResponse"),
        } : message));
      }
    } finally {
      setSending(false);
      abortRef.current = null;
    }
  };

  const decideApproval = async (approved: boolean) => {
    if (!approval || decidingApproval) return;
    setDecidingApproval(true);
    try {
      await decideAgentRuntimeApproval(approval.threadId, approval.approvalId, approved);
      setApproval(null);
      if (!approved) setError(t("agents.preview.approvalRejected", "Tool approval was rejected. Inspect the run before retrying."));
    } catch (cause) {
      setError(previewErrorMessage(cause, t));
    } finally {
      setDecidingApproval(false);
    }
  };

  const refreshRunStatus = async () => {
    if (!session || refreshingRun) return;
    const sessionId = session.session_id;
    setRefreshingRun(true);
    try {
      const currentRunId = runId ?? latestPreviewRunId((await getSessionHistory(sessionId)).messages);
      if (!currentRunId) throw new Error("Preview run identity is unavailable");
      setRunId(currentRunId);
      const recovered = await readPreviewRun(sessionId, currentRunId);
      if (sessionIdRef.current !== sessionId) return;
      setApproval(recovered.approval);
      const terminal = previewRunIsTerminal(recovered.status);
      if (terminal) {
        const history = await getSessionHistory(sessionId);
        if (sessionIdRef.current !== sessionId) return;
        setMessages(previewHistoryMessages(history.messages));
        void refreshArtifactFacts(sessionId);
      }
      setRunUnsettled(!terminal);
      if (terminal) setCancelRequested(false);
      setError(["failed", "cancelled", "timeout"].includes(recovered.status)
        ? t("agents.preview.runNotSuccessful", { status: recovered.status, defaultValue: "Preview ended with status {{status}}. Check the trace before publishing." })
        : null);
    } catch {
      if (sessionIdRef.current === sessionId) {
        setRunUnsettled(true);
        setError(t("agents.preview.statusUnavailable", "The saved run status could not be verified. Refresh its status or start a new Preview."));
      }
    } finally {
      setRefreshingRun(false);
    }
  };

  const cancelRun = async () => {
    if (!runId || cancelling || cancelRequested) return;
    setCancelling(true);
    try {
      await cancelTask(runId, "user_requested_stop");
      abortRef.current?.abort();
      setRunUnsettled(true);
      setApproval(null);
      setCancelRequested(true);
      setError(t("agents.preview.cancelRequested", "Cancellation was requested. Refresh the run status to confirm its outcome."));
    } catch (cause) {
      setError(previewErrorMessage(cause, t));
    } finally {
      setCancelling(false);
    }
  };

  const clearSession = () => {
    if (sessionActionsBlocked) return;
    abortRef.current?.abort();
    setSession(null);
    setMessages([]);
    setActivities([]);
    setArtifacts([]);
    setArtifactError(null);
    setError(null);
    setSessionSpec(null);
    setRunId(null);
    setRunUnsettled(false);
    setApproval(null);
    setCancelRequested(false);
    setHistory((current) => {
      const next = { ...current };
      delete next[target];
      return next;
    });
  };

  const switchTarget = (nextTarget: string) => {
    if (sessionActionsBlocked) return;
    if (nextTarget === target) return;
    if (!window.confirm(t("agents.preview.switchConfirm"))) return;
    abortRef.current?.abort();
    const currentHistory: PreviewHistory = { session, spec: sessionSpec, messages, activities, artifacts, error, runId, runUnsettled, cancelRequested };
    const nextHistory = history[nextTarget];
    setHistory((current) => ({ ...current, [target]: currentHistory }));
    setTarget(nextTarget);
    setSession(nextHistory?.session ?? null);
    setSessionSpec(nextHistory?.spec ?? null);
    setMessages(nextHistory?.messages ?? []);
    setActivities(nextHistory?.activities ?? []);
    setArtifacts(nextHistory?.artifacts ?? []);
    setArtifactError(null);
    setError(nextHistory?.error ?? null);
    // The older draft may still be executing after starting an isolated current
    // draft. Restore its identity so the existing read-only poll recovers its
    // approval/outcome; never treat a cached transcript as proof of completion.
    setRunId(nextHistory?.runId ?? null);
    setRunUnsettled(nextHistory?.runUnsettled ?? false);
    setApproval(null);
    setCancelRequested(nextHistory?.cancelRequested ?? false);
    setSending(false);
  };

  return (
    <section
      className="agent-preview"
      aria-labelledby="agent-preview-title"
      aria-busy={sending}
      data-testid="agent-preview-panel"
    >
      <header className="agent-preview-header">
        <div>
          <Title id="agent-preview-title" level={3}>{t("agents.common.preview")}</Title>
          <Text type="secondary">{t("agents.preview.subtitle")}</Text>
        </div>
        <Button icon={<MessageSquarePlus size={16} />} onClick={() => void startSession()} loading={starting} disabled={sessionActionsBlocked && !canStartCurrentDraft}>
          {target.startsWith("draft:") ? t("agents.preview.currentDraft", "Preview current draft") : t("agents.preview.newSession")}
        </Button>
      </header>

      <Select
        className="agent-preview-target"
        value={target}
        disabled={sessionActionsBlocked}
        aria-label={t("agents.preview.targetLabel")}
        onChange={switchTarget}
        options={[
          { value: "draft", label: t("agents.common.draftLabel", { revision: draftRevision }) },
          ...previousDrafts.map((key) => ({ value: key, label: t("agents.preview.priorDraft", { revision: Number(key.slice(6)), defaultValue: "Earlier draft r{{revision}} · view only" }) })),
          ...versions.map((version) => ({ value: version.agent_version_id, label: t("agents.common.versionLabel", { version: version.version_number }) })),
        ]}
      />

      {staleDraft && <Alert className="agent-preview-notice" type="warning" showIcon title={t("agents.preview.staleDraft", { old: pinnedRevision, current: draftRevision, defaultValue: "This session is pinned to saved draft r{{old}}. Current draft is r{{current}}. Review the old run here; start a new Preview for another turn." })} />}
      <Alert
        className="agent-preview-notice"
        type={dirty ? "warning" : "info"}
        showIcon
        title={dirty
          ? t("agents.preview.dirtyNotice", { target: targetLabel })
          : t("agents.preview.savedNotice", { target: targetLabel })}
      />

      {effectiveSpec ? <div className="agent-effective-summary" aria-label={t("agents.preview.summaryLabel")}>
        <span>{t("agents.preview.summaryModel", { model: effectiveSpec.model.model_id || t("agents.common.serverDefault") })}</span>
        <span>{t("agents.preview.summaryCapabilities", { count: effectiveSpec.capabilities.length })}</span>
        <span>{t("agents.preview.summaryKnowledge", { count: effectiveSpec.knowledge.length })}</span>
        <span>{t("agents.preview.summaryMemory", { mode: memoryModeLabel })}</span>
        {effectiveNative && (effectiveNative.length > 0 || !hasNativeBindings) && <span>{t("agents.preview.effectiveNative", { count: effectiveNative.length, defaultValue: "{{count}} platform tools authorized for this session" })}</span>}
      </div> : <div className="agent-effective-summary">{t("agents.preview.oldSummaryUnavailable", "Earlier draft configuration is not available in this page. Inspect the run trace for its recorded version and model.")}</div>}

      <div className="agent-preview-transcript" aria-live="polite">
        {!session && messages.length === 0 && !starting && !restoring && (
          <div className="agent-preview-empty">
            <Bot size={28} />
            <Title level={4}>{t("agents.preview.startTitle")}</Title>
            <Paragraph type="secondary">{t("agents.preview.startDescription", { target: targetLabel })}</Paragraph>
            <Button type="primary" disabled={sessionActionsBlocked} onClick={() => void startSession()}>{t("agents.preview.startButton")}</Button>
          </div>
        )}
        {(starting || restoring) && <div className="agent-preview-spinner"><Spin /><span>{restoring ? t("agents.preview.restoring", "Restoring this Preview's authorized history…") : t("agents.preview.resolving")}</span></div>}
        {session && <div className="agent-session-label"><RotateCcw size={14} /> {t("agents.preview.sessionLabel", { target: targetLabel })}</div>}
        {messages.map((message, index) => (
          <article key={message.id} className={`agent-preview-message agent-preview-message-${message.role}`}>
            <span className="agent-message-avatar" aria-hidden>{message.role === "user" ? <User size={15} /> : <Bot size={15} />}</span>
            <div><strong>{message.role === "user" ? t("agents.preview.you") : agentName}</strong><p>{message.content || (index === messages.length - 1 && (sending || runUnsettled) && !cancelRequested ? <span className="agent-stream-cursor">{t(approval ? "agents.preview.approvalTitle" : "agents.preview.generating")}</span> : t("agents.preview.emptyResponse"))}</p></div>
          </article>
        ))}
        {activities.map((activity) => (
          <article key={activity.id} className="agent-preview-activity">
            {activity.kind === "tool" ? <Wrench size={15} /> : <BookOpen size={15} />}
            <div><strong>{activity.title}</strong><p>{activity.detail}</p></div>
            {activity.status && <Tag color="green">{activity.status}</Tag>}
          </article>
        ))}
        {artifacts.length > 0 && <section className="agent-preview-artifacts" aria-label={t("agents.preview.artifacts", "Generated files")}>
          <strong>{t("agents.preview.artifacts", "Generated files")}</strong>
          <ul>{artifacts.map((artifact) => (
            <li key={artifact.artifact_id}>
              {previewArtifactCanDownload(artifact)
                ? <button type="button" onClick={() => {
                  void downloadPreviewArtifact(artifact, getArtifactDownloadUrl, downloadAssistantArtifact)
                    .catch((error) => setArtifactError(t(artifactAccessErrorKey(error))));
                }}>{artifact.filename || artifact.title}</button>
                : <span>{artifact.filename || artifact.title} · {t("agents.preview.artifactPending", "File is not ready for download")}</span>}
            </li>
          ))}</ul>
        </section>}
        {approval && <div className="agent-preview-approval" role="group" aria-label={t("agents.preview.approvalTitle", "Tool approval required")}>
          <strong>{t("agents.preview.approvalTitle", "Tool approval required")}</strong>
          <p>{approval.preview?.tool_name || t("agents.preview.toolDefault")}</p>
          {approval.preview?.reason && <p role="status">{approval.preview.reason}</p>}
          {approval.preview?.effect && <p>{approval.preview.effect}</p>}
          {approval.preview?.target && <p>{approval.preview.target}</p>}
          {approval.preview?.parameters?.map((parameter) => <p key={parameter.name}>{parameter.name}: {parameter.value}</p>)}
          <div>
            <Button disabled={!approval.preview?.can_approve || decidingApproval} loading={decidingApproval} onClick={() => void decideApproval(true)}>{t("agents.preview.approve", "Approve once")}</Button>
            <Button danger disabled={decidingApproval} onClick={() => void decideApproval(false)}>{t("agents.preview.reject", "Reject")}</Button>
          </div>
        </div>}
      </div>

      {artifactError && <Alert className="agent-preview-error" type="warning" showIcon title={artifactError} />}
      {error && <Alert className="agent-preview-error" type="error" showIcon title={t("agents.preview.failed")} description={error} action={<Button disabled={sessionActionsBlocked} onClick={() => void startSession()}>{t("agents.preview.newSession")}</Button>} />}
      {runUnsettled && !approval && !sending && <Alert className="agent-preview-notice" type="warning" showIcon title={t("agents.preview.outcomePending", "The prior run has no confirmed terminal outcome. Inspect its trace or stop the run before starting another Preview.")} />}

      <div className="agent-preview-composer">
        <Input.TextArea
          autoSize={{ minRows: 1, maxRows: 5 }}
          value={input}
          onChange={(event) => setInput(event.target.value)}
          onPressEnter={(event) => {
            if (!event.shiftKey) {
              event.preventDefault();
              void sendMessage();
            }
          }}
          placeholder={t("agents.preview.messagePlaceholder")}
          aria-label={t("agents.preview.messagePlaceholder")}
          disabled={starting || restoring || staleDraft || runUnsettled || target.startsWith("draft:") || Boolean(approval)}
        />
        <Button type="primary" icon={<Send size={16} />} aria-label={t("agents.preview.sendLabel")} disabled={!input.trim() || starting || restoring || staleDraft || runUnsettled || target.startsWith("draft:") || Boolean(approval)} loading={sending} onClick={() => void sendMessage()} />
      </div>
      <footer className="agent-preview-footer">
        <Button type="text" icon={<Trash2 size={14} />} onClick={clearSession} disabled={sessionActionsBlocked || (!session && messages.length === 0)}>{t("agents.preview.clear")}</Button>
        {runUnsettled && session && <Button size="small" loading={refreshingRun} onClick={() => void refreshRunStatus()}>{t("agents.preview.refreshStatus", "Refresh run status")}</Button>}
        {(sending || runUnsettled) && runId && <Button danger size="small" loading={cancelling} disabled={cancelRequested} onClick={() => void cancelRun()}>{t("agents.preview.stop", "Stop run")}</Button>}
        {session ? <Link to={`/eval?tab=traces&family=assistant&session_id=${encodeURIComponent(session.session_id)}`}>{t("agents.preview.openTrace")} <ExternalLink size={13} /></Link> : <Text type="secondary">{t("agents.preview.traceAfterRun")}</Text>}
      </footer>
    </section>
  );
}
