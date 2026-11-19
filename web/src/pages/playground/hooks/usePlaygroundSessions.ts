import type { RequestBody } from "@/api/contracts";
import { api } from "@/lib/api";
import { captureRequestSnapshot, persistedRequestSnapshot, restoreRequestSnapshot, serializeRequestSnapshot, type PlaygroundRequestCapture, type PlaygroundRequestSnapshot } from "../utils/requestSnapshot";
import { restoreTokenObservation } from "@/features/chat/stream/tokenObservation";
import { sdkRunReference, sdkUsageObservation } from "@/features/chat/stream/sdkRun";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  createSession,
  deleteSession,
  getSessionHistory,
  listSessions,
  updateSession,
  type SessionSummary,
} from "@/api/sessions";
import type { ChatMessage } from "@/components/ChatWindow";
import { useAppStore } from "@/store/useAppStore";
import { useAuthStore } from "@/store/useAuthStore";
import {
  trackChatHistoryEmptyState,
  trackChatHistoryRestored,
} from "@/features/chat/telemetry";
import {
  readReportedRouting,
  buildTextParts,
  convertToolCallsFromMetadata,
  dedupeHistory,
} from "../utils/langgraph";

export interface UsePlaygroundSessionsOptions {
  serviceId?: string;
  services: { service_id: string; service_type?: string; metadata?: Record<string, unknown> }[];
  onSessionCreationError?: (error: unknown) => void;
}

export interface UsePlaygroundSessionsReturn {
  requestSnapshot?: PlaygroundRequestSnapshot;
  requestSnapshotStatus: "none" | "local" | "saving" | "saved" | "failed" | "restored";
  onRequestSnapshot: (capture: PlaygroundRequestCapture, isCurrent: () => boolean) => void;
  copyRequestSnapshot: () => Promise<void>;
  sessions: SessionSummary[];
  sessionsLoading: boolean;
  historyLoading: boolean;
  historyRestoreState: "idle" | "loading" | "ready" | "failed";
  historyRestoreError: string | null;
  sessionEnabled: boolean;
  setSessionEnabled: (enabled: boolean) => void;
  messages: ChatMessage[];
  setMessages: React.Dispatch<React.SetStateAction<ChatMessage[]>>;
  refreshSessions: (silent?: boolean) => Promise<void>;
  handleSelectSession: (id: string) => Promise<void>;
  handleNewSession: () => Promise<string | null>;
  handleDeleteSession: (id: string) => Promise<void>;
  handleStopStreaming: () => void;
  /** Ref tracking the currently loading history session (for race-condition prevention) */
  loadingHistorySessionRef: React.MutableRefObject<string | null>;
  /** Ref tracking current request session ID */
  currentRequestSessionRef: React.MutableRefObject<string | null>;
  /** Request-scoped counter to invalidate stale requests */
  currentRequestIdRef: React.MutableRefObject<number>;
  /** Abort controller for cancelling in-flight requests */
  abortControllerRef: React.MutableRefObject<AbortController | null>;
  /** Thread ID map per session */
  sessionThreadIdRef: React.MutableRefObject<Record<string, string>>;
  /** Pending session init promise */
  pendingSessionInitRef: React.MutableRefObject<Promise<string | null> | null>;
  /** Messages ref for synchronous access */
  messagesRef: React.MutableRefObject<ChatMessage[]>;
  /** Whether user has started interacting */
  interactionStartedRef: React.MutableRefObject<boolean>;
  /** Invalidate any pending history load */
  invalidatePendingHistoryLoad: () => void;
  /** Pending thread creation promise (resolves to threadId or null) */
  pendingThreadRef: React.MutableRefObject<Promise<string | null> | null>;
  /** Loading state for streaming */
  loading: boolean;
  setLoading: React.Dispatch<React.SetStateAction<boolean>>;
}

export function usePlaygroundSessions({
  serviceId,
  services,
  onSessionCreationError,
}: UsePlaygroundSessionsOptions): UsePlaygroundSessionsReturn {
  const {
    activeSessionId,
    setActiveSessionId,
    playgroundSidebarOpen,
    setLocalTitles,
  } = useAppStore();

  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const messagesRef = useRef<ChatMessage[]>([]);
  const [loading, setLoading] = useState(false);
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [sessionsLoading, setSessionsLoading] = useState(false);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [historyRestoreState, setHistoryRestoreState] = useState<
    "idle" | "loading" | "ready" | "failed"
  >("idle");
  const [historyRestoreError, setHistoryRestoreError] = useState<string | null>(null);
  const [sessionEnabled, setSessionEnabledState] = useState(true);
  const memoryRevisionRef = useRef(0);
  const setSessionEnabled = useCallback((enabled: boolean) => {
    memoryRevisionRef.current += 1;
    setSessionEnabledState(enabled);
  }, []);

  const interactionStartedRef = useRef(false);
  const abortControllerRef = useRef<AbortController | null>(null);
  const currentRequestSessionRef = useRef<string | null>(null);
  const currentRequestIdRef = useRef(0);
  const loadingHistorySessionRef = useRef<string | null>(null);
  const sessionThreadIdRef = useRef<Record<string, string>>({});
  const pendingSessionInitRef = useRef<Promise<string | null> | null>(null);
  const pendingThreadRef = useRef<Promise<string | null> | null>(null);
  // Navigation owns session initialization; Send has a separate stream generation.
  const sessionNavigationIdRef = useRef(0);
  const selectedServiceRef = useRef(serviceId);
  selectedServiceRef.current = serviceId;
  const pendingSessionInitIsCurrentRef = useRef<(() => boolean) | null>(null);
  const invalidatePendingSessionInit = useCallback(() => {
    sessionNavigationIdRef.current += 1;
    pendingSessionInitRef.current = null;
    pendingSessionInitIsCurrentRef.current = null;
    pendingThreadRef.current = null;
    setSessionsLoading(false);
  }, []);

  const [diagnostic, setDiagnostic] = useState<{
    snapshot: PlaygroundRequestSnapshot;
    status: UsePlaygroundSessionsReturn["requestSnapshotStatus"];
    isCurrent: () => boolean;
  } | null>(null);
  const diagnosticWriteRef = useRef<Promise<void>>(Promise.resolve());
  // Navigation and authority own visibility, while a stopped attempt remains inspectable.
  const diagnosticOwner = useCallback(() => {
    const navigationId = sessionNavigationIdRef.current;
    const token = useAuthStore.getState().token;
    const authority = sdkAuthorityScope(useAuthStore.getState().user);
    return () => navigationId === sessionNavigationIdRef.current && selectedServiceRef.current === serviceId &&
      useAuthStore.getState().token === token && sdkAuthorityScope(useAuthStore.getState().user) === authority;
  }, [serviceId]);
  const onRequestSnapshot = useCallback((capture: PlaygroundRequestCapture, requestIsCurrent: () => boolean) => {
    if (!requestIsCurrent()) return;
    const snapshot = captureRequestSnapshot(capture);
    if (!snapshot) return;
    const isCurrent = diagnosticOwner();
    setDiagnostic({ snapshot, status: snapshot.session_id ? "saving" : "local", isCurrent });
    if (!snapshot.session_id) return;
    // Serialize client submissions; timeout does not prove an earlier server write stopped.
    const write = diagnosticWriteRef.current.catch(() => {}).then(async () => {
      if (!isCurrent() || !requestIsCurrent()) {
        setDiagnostic(current => current?.snapshot === snapshot && isCurrent() ? { ...current, status: "local" } : current);
        return;
      }
      try {
        // Keep the established API base URL and same-authority authentication behavior.
        await api.patch(`/api/v1/sessions/${encodeURIComponent(snapshot.session_id!)}`, ({
          metadata: { playground_request: persistedRequestSnapshot(snapshot) },
        } satisfies RequestBody<"PATCH /api/v1/sessions/{session_id}">), { timeout: 10_000 });
        setDiagnostic(current => current?.snapshot === snapshot && isCurrent() ? { ...current, status: "saved" } : current);
      } catch {
        setDiagnostic(current => current?.snapshot === snapshot && isCurrent() ? { ...current, status: "failed" } : current);
      }
    });
    diagnosticWriteRef.current = write;
  }, [diagnosticOwner]);
  const copyRequestSnapshot = useCallback(async () => {
    if (!diagnostic?.isCurrent()) throw new Error("Request snapshot unavailable");
    await navigator.clipboard.writeText(serializeRequestSnapshot(diagnostic.snapshot));
  }, [diagnostic]);

  // Keep messagesRef in sync
  useEffect(() => {
    messagesRef.current = messages;
  }, [messages]);

  // Recover from stale loading state
  useEffect(() => {
    if (!loading) return;
    const lastAssistant = [...messages]
      .reverse()
      .find((message) => message.role === "assistant");
    if (!lastAssistant) return;
    const assistantStillStreaming =
      lastAssistant.isStreaming === true || lastAssistant.status === "streaming";
    if (assistantStillStreaming) return;
    setLoading(false);
  }, [loading, messages]);

  const refreshSessions = useCallback(async (silent = false, ownerIsCurrent?: () => boolean) => {
    const navigationId = sessionNavigationIdRef.current;
    const token = useAuthStore.getState().token;
    const authorityScope = sdkAuthorityScope(useAuthStore.getState().user);
    const isCurrent = () => sessionNavigationIdRef.current === navigationId &&
      selectedServiceRef.current === serviceId && useAuthStore.getState().token === token &&
      sdkAuthorityScope(useAuthStore.getState().user) === authorityScope && (ownerIsCurrent?.() ?? true);
    if (!isCurrent()) return;
    if (!serviceId) {
      setSessions([]);
      return;
    }
    // Only show loading spinner on initial load, not on background refreshes
    if (!silent) setSessionsLoading(true);
    try {
      const data = await listSessions({ service_id: serviceId, limit: 100 });
      if (!isCurrent()) return;
      // In silent mode, skip setSessions if the session list hasn't materially
      // changed (same IDs in same order). This prevents sidebar re-renders
      // caused by new array references with only timestamp differences.
      if (silent) {
        setSessions((prev) => {
          if (!isCurrent()) return prev;
          const prevIds = prev.map((s) => s.session_id).join(",");
          const nextIds = data.map((s) => s.session_id).join(",");
          return prevIds === nextIds ? prev : data;
        });
      } else {
        setSessions(current => isCurrent() ? data : current);
      }
      const nextThreads = { ...sessionThreadIdRef.current };
      for (const s of data) {
        const threadId = s.metadata?.langgraph_thread_id as string | undefined;
        if (threadId) {
          nextThreads[s.session_id] = threadId;
        }
      }
      sessionThreadIdRef.current = nextThreads;
      setLocalTitles((prev) => {
        if (!isCurrent()) return prev;
        const updated = { ...prev };
        for (const s of data) {
          const serverTitle = s.metadata?.title as string | undefined;
          if (serverTitle && !updated[s.session_id]) {
            updated[s.session_id] = serverTitle;
          }
        }
        return updated;
      });
    } finally {
      if (!silent && isCurrent()) setSessionsLoading(false);
    }
  }, [serviceId, setLocalTitles]);

  const invalidatePendingHistoryLoad = useCallback(() => {
    loadingHistorySessionRef.current = null;
    setHistoryLoading(false);
    setHistoryRestoreState("idle");
    setHistoryRestoreError(null);
  }, []);

  const handleStopStreaming = useCallback(() => {
    // Stop owns the local terminal state before invalidating the transport.
    // An abort does not establish that upstream execution has stopped.
    setMessages(current => current.map(message => message.role === "assistant" && message.isStreaming
      ? { ...message, isStreaming: false, isThinking: false, status: "cancelled" as const } : message));
    setLoading(false);
    if (abortControllerRef.current) {
      abortControllerRef.current.abort();
      abortControllerRef.current = null;
    }
    currentRequestIdRef.current += 1;
  }, []);

  const handleSelectSession = useCallback(
    async (id: string) => {
      const token = useAuthStore.getState().token;
      const authorityScope = sdkAuthorityScope(useAuthStore.getState().user);
      interactionStartedRef.current = true;
      handleStopStreaming();
      invalidatePendingSessionInit();
      const navigationId = sessionNavigationIdRef.current;
      const navigationIsCurrent = () => sessionNavigationIdRef.current === navigationId &&
        selectedServiceRef.current === serviceId && useAuthStore.getState().token === token &&
        sdkAuthorityScope(useAuthStore.getState().user) === authorityScope;
      const historyIsCurrent = () => navigationIsCurrent() && loadingHistorySessionRef.current === id;
      loadingHistorySessionRef.current = id;
      setActiveSessionId(id);
      setHistoryLoading(true);
      setHistoryRestoreState("loading");
      setHistoryRestoreError(null);
      setMessages([]);
      setDiagnostic(null);
      // Item GET supplies full metadata; older servers may omit service_id.
      // Custom metadata is not guaranteed in list summaries. History stays independent.
      const memoryRevision = memoryRevisionRef.current;
      void api.get<SessionSummary>(`/api/v1/sessions/${encodeURIComponent(id)}`, { timeout: 10_000 }).then(({ data: session }) => {
        if (!historyIsCurrent() || session.session_id !== id ||
          (session.service_id !== undefined && session.service_id !== serviceId)) return;
        const snapshot = restoreRequestSnapshot(session.metadata?.playground_request, serviceId ?? "", id);
        if (!snapshot) return;
        setSessionEnabledState(current => historyIsCurrent() && memoryRevisionRef.current === memoryRevision ? snapshot.memory_enabled : current);
        const isCurrent = diagnosticOwner();
        setDiagnostic(current => historyIsCurrent() ? { snapshot, status: "restored", isCurrent } : current);
      }).catch(() => {});

      // Eagerly pre-create thread for this session if it doesn't have one yet
      const hasThread = !!sessionThreadIdRef.current[id];
      if (!hasThread) {
        const activeService = services.find((s) => s.service_id === serviceId);
        const isTransparentProxy =
          activeService?.service_type === "langgraph" ||
          activeService?.metadata?.adapter_type === "langgraph" ||
          activeService?.metadata?.proxy_mode === "transparent";

        if (isTransparentProxy && serviceId) {
          const token = useAuthStore.getState().token;
          pendingThreadRef.current = fetch(
            `/api/v1/proxy/${serviceId}/threads`,
            {
              method: "POST",
              headers: {
                "Content-Type": "application/json",
                ...(token ? { Authorization: `Bearer ${token}` } : {}),
              },
              body: JSON.stringify(({ metadata: { gateway_session_id: id } } satisfies RequestBody<"POST /api/v1/proxy/{service_name}/threads">)),
            }
          )
            .then((resp) => (resp.ok ? resp.json() : null))
            .then((data) => {
              // Send may replace history loading while still owning this selected thread.
              if (!navigationIsCurrent()) return null;
              if (data) {
                const tid = data.thread_id || data.id || data.threadId;
                if (tid) {
                  sessionThreadIdRef.current[id] = tid;
                  updateSession(id, { metadata: { langgraph_thread_id: tid } }).catch(() => {});
                  return tid as string;
                }
              }
              return null;
            })
            .catch(() => null);
        }
      }

      try {
        const timeoutMs = 10000;
        const historyPromise = getSessionHistory(id, { limit: 200 });
        const timeoutPromise = new Promise<never>((_, reject) =>
          setTimeout(() => reject(new Error("History load timeout")), timeoutMs)
        );
        const history = await Promise.race([historyPromise, timeoutPromise]);

        if (!historyIsCurrent()) return;

        const normalizedHistory = dedupeHistory(history);

        const nextMessages: ChatMessage[] = normalizedHistory.map((m, idx) => {
          const role = m.role === "user" ? "user" : "assistant";
          const content =
            typeof m.content === "string" ? m.content : JSON.stringify(m.content ?? "");
          const createdAt = m.timestamp || new Date().toISOString();
          const messageId = `${id}-${m.timestamp || "history"}-${role}-${idx}`;
          const parts = buildTextParts(messageId, content, createdAt);

          if (role === "assistant" && m.metadata) {
            const toolCalls = convertToolCallsFromMetadata(m.metadata.tool_calls);
            const sdkRun = m.metadata.source_access_revoked ? undefined : sdkRunReference(m.metadata, id);
            const observation = sdkRun ? sdkUsageObservation(m.metadata.stats ?? m.metadata.usage) : undefined;
            const stats = m.metadata.stats
              ? {
                  ...(!sdkRun ? { reportedRouting: readReportedRouting(m.metadata.stats.reported_routing) } : {}),
                  durationMs: m.metadata.stats.duration_ms,
                  firstTokenMs: m.metadata.stats.first_token_ms,
                  ...(observation ? {
                    inputTokens: observation.inputTokens,
                    outputTokens: observation.outputTokens,
                    totalTokens: observation.totalTokens,
                  } : restoreTokenObservation(m.metadata.stats)),
                }
              : sdkRun ? undefined : restoreTokenObservation(undefined);

            return {
              id: messageId,
              role,
              content,
              createdAt,
              parts,
              completionConfirmed: sdkRun ? m.metadata.sdk_stream_status === "completed" :
                m.metadata.gateway_stream_status === "completed",
              status: sdkRun && m.metadata.sdk_stream_status === "interrupted" ? "cancelled" as const :
                sdkRun && m.metadata.sdk_stream_status === "failed" ? "failed" as const :
                !sdkRun && m.metadata.gateway_stream_status === "interrupted" ? "cancelled" as const :
                !sdkRun && Object.prototype.hasOwnProperty.call(m.metadata, "gateway_stream_status") &&
                  m.metadata.gateway_stream_status !== "completed" ? "failed" as const : "completed" as const,
              toolCalls,
              stats,
              ...(sdkRun ? { sdkRun, sdkAuthorityScope: authorityScope } : {}),
            };
          }

          return {
            id: messageId,
            role,
            content,
            createdAt,
            parts,
            status: "completed" as const,
            ...(role === "assistant" ? { stats: restoreTokenObservation(undefined) } : {}),
          };
        });
        setMessages(current => historyIsCurrent() ? nextMessages : current);
        setHistoryRestoreState(current => historyIsCurrent() ? "ready" : current);
        trackChatHistoryRestored("playground", {
          sessionId: id,
          messageCount: nextMessages.length,
          restored: true,
        });
      } catch (err) {
        if (historyIsCurrent()) {
          const errorMessage = err instanceof Error ? err.message : String(err);
          setHistoryRestoreState("failed");
          setHistoryRestoreError(errorMessage);
          trackChatHistoryRestored("playground", {
            sessionId: id,
            messageCount: 0,
            restored: false,
            reason: errorMessage,
          });
        }
      } finally {
        if (historyIsCurrent()) {
          setHistoryLoading(false);
        }
      }
    },
    [setActiveSessionId, serviceId, services, handleStopStreaming, invalidatePendingSessionInit, diagnosticOwner]
  );

  const handleNewSession = useCallback(async () => {
    if (!serviceId) return null;
    interactionStartedRef.current = true;

    if (pendingSessionInitRef.current && pendingSessionInitIsCurrentRef.current?.()) {
      // Waiting Send needs the raw rejection; duplicate New callers use the
      // public handled-failure contract and must not emit another notification.
      return pendingSessionInitRef.current.catch(() => null);
    }

    handleStopStreaming();
    invalidatePendingSessionInit();
    invalidatePendingHistoryLoad();
    const navigationId = sessionNavigationIdRef.current;
    const initialRequestId = currentRequestIdRef.current;
    const token = useAuthStore.getState().token;
    const authorityScope = sdkAuthorityScope(useAuthStore.getState().user);
    const isCurrent = () => sessionNavigationIdRef.current === navigationId &&
      selectedServiceRef.current === serviceId && useAuthStore.getState().token === token &&
      sdkAuthorityScope(useAuthStore.getState().user) === authorityScope;

    // Enter the new view before awaiting the server. A pending Send must keep its
    // own input/response when initialization resolves, and must never use the old session.
    setActiveSessionId(undefined);
    currentRequestSessionRef.current = null;
    setMessages([]);
    pendingSessionInitIsCurrentRef.current = isCurrent;

    const pendingSession = (async () => {
      const created = await createSession({ service_id: serviceId });
      if (!isCurrent()) return null;
      currentRequestSessionRef.current = created.session_id;
      setActiveSessionId(created.session_id);
      // Do not mark a history load or clear messages: Send may already be waiting.
      setSessions((prev) => {
        if (!isCurrent() || prev.some((s) => s.session_id === created.session_id)) return prev;
        return [{ session_id: created.session_id, service_id: serviceId, created_at: new Date().toISOString(), updated_at: new Date().toISOString(), metadata: {} } as SessionSummary, ...prev];
      });

      const activeService = services.find((s) => s.service_id === serviceId);
      const isTransparentProxy =
        activeService?.service_type === "langgraph" ||
        activeService?.metadata?.adapter_type === "langgraph" ||
        activeService?.metadata?.proxy_mode === "transparent";
      if (isTransparentProxy) {
        const threadPromise = fetch(`/api/v1/proxy/${serviceId}/threads`, {
          method: "POST",
          headers: {
            "Content-Type": "application/json",
            ...(token ? { Authorization: `Bearer ${token}` } : {}),
          },
          body: JSON.stringify(({ metadata: { gateway_session_id: created.session_id } } satisfies RequestBody<"POST /api/v1/proxy/{service_name}/threads">)),
        })
          .then((resp) => (resp.ok ? resp.json() : null))
          .then((data) => {
            if (!isCurrent()) return null;
            const tid = data?.thread_id || data?.id || data?.threadId;
            if (tid) {
              sessionThreadIdRef.current[created.session_id] = tid;
              updateSession(created.session_id, { metadata: { langgraph_thread_id: tid } }).catch(() => {});
              return tid as string;
            }
            return null;
          })
          .catch(() => null);
        pendingThreadRef.current = threadPromise;
      }

      // The session is usable now. Sidebar refresh failure must not fail creation
      // or hold a waiting Send; a superseded refresh cannot publish this view's data.
      refreshSessions(true, isCurrent).catch(() => {});
      return created.session_id;
    })();

    pendingSessionInitRef.current = pendingSession;
    try {
      return await pendingSession;
    } catch (error) {
      if (!isCurrent()) return null;
      // Keep the new target detached from the previous conversation. The page
      // reports failure unless a waiting Send already owns the visible error.
      if (currentRequestIdRef.current === initialRequestId) onSessionCreationError?.(error);
      return null;
    } finally {
      if (pendingSessionInitRef.current === pendingSession) {
        pendingSessionInitRef.current = null;
        pendingSessionInitIsCurrentRef.current = null;
      }
    }
  }, [serviceId, services, refreshSessions, setActiveSessionId, invalidatePendingHistoryLoad, handleStopStreaming, invalidatePendingSessionInit, onSessionCreationError]);

  const handleDeleteSession = useCallback(
    async (id: string) => {
      if (loadingHistorySessionRef.current === id) {
        invalidatePendingHistoryLoad();
      }
      if (activeSessionId === id) {
        handleStopStreaming();
        invalidatePendingSessionInit();
        setActiveSessionId(undefined);
        setMessages([]);
        setHistoryRestoreState("idle");
        setHistoryRestoreError(null);
      }
      // Optimistic: remove from list immediately
      setSessions((prev) => prev.filter((s) => s.session_id !== id));
      setLocalTitles((prev) => {
        const next = { ...prev };
        delete next[id];
        return next;
      });
      // Server delete + silent background sync
      await deleteSession(id);
      await refreshSessions(true);
    },
    [activeSessionId, refreshSessions, setActiveSessionId, setLocalTitles, invalidatePendingHistoryLoad, handleStopStreaming, invalidatePendingSessionInit]
  );

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      invalidatePendingSessionInit();
      currentRequestIdRef.current += 1;
      if (abortControllerRef.current) {
        abortControllerRef.current.abort();
        abortControllerRef.current = null;
      }
    };
  }, [invalidatePendingSessionInit]);

  // Pick an initial default only. A removed selection needs an explicit user choice;
  // silently replacing it could change the target and discard the current conversation.
  useEffect(() => {
    if (services.length === 0 || serviceId) return;

    const { setSelectedServiceId } = useAppStore.getState();
    const fallbackService =
      services.find((service) => service.service_id === "assistant") ||
      services.find((service) => service.service_type === "assistant") ||
      services[0];

    if (fallbackService && fallbackService.service_id !== serviceId) {
      setSelectedServiceId(fallbackService.service_id);
    }
  }, [serviceId, services]);

  // Initial mount: restore persisted state
  useEffect(() => {
    let cancelled = false;
    let navigationId = sessionNavigationIdRef.current;
    const token = useAuthStore.getState().token;
    const authorityScope = sdkAuthorityScope(useAuthStore.getState().user);
    const isCurrent = () => !cancelled && sessionNavigationIdRef.current === navigationId &&
      selectedServiceRef.current === serviceId && useAuthStore.getState().token === token &&
      sdkAuthorityScope(useAuthStore.getState().user) === authorityScope;

    const init = async () => {
      const urlParams = new URLSearchParams(window.location.search);
      if (urlParams.get("reset") === "true") {
        setActiveSessionId(undefined);
        setMessages([]);
        setHistoryRestoreState("idle");
        setHistoryRestoreError(null);
        const newUrl = window.location.pathname;
        window.history.replaceState({}, "", newUrl);
        if (serviceId) {
          await refreshSessions();
        }
        return;
      }

      if (serviceId) {
        const data = await listSessions({ service_id: serviceId, limit: 100 });
        if (!isCurrent()) return;
        setSessions(current => isCurrent() ? data : current);

        const latestStore = useAppStore.getState();
        const activeSessionChanged = latestStore.activeSessionId !== activeSessionId;
        const serviceChanged = latestStore.selectedServiceId !== serviceId;
        if (
          interactionStartedRef.current ||
          activeSessionChanged ||
          serviceChanged ||
          messagesRef.current.length > 0
        ) {
          return;
        }

        const sessionBelongsToCurrentService =
          activeSessionId && data.some((s) => s.session_id === activeSessionId);

        if (sessionBelongsToCurrentService) {
          const restoration = handleSelectSession(activeSessionId!);
          // Automatic restore owns this one navigation step. Keep its queued
          // sidebar update valid, while later user navigation still invalidates it.
          navigationId += 1;
          await restoration;
        } else if (activeSessionId) {
          setActiveSessionId(undefined);
          setMessages([]);
          setHistoryRestoreState("idle");
          setHistoryRestoreError(null);
        }
      }
    };
    void init().catch(() => {
      if (isCurrent() && activeSessionId) setHistoryRestoreState("failed");
    });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Service switch: clear and refresh
  const prevServiceId = useRef(serviceId);
  useEffect(() => {
    if (serviceId !== prevServiceId.current) {
      handleStopStreaming();
      invalidatePendingSessionInit();
      currentRequestSessionRef.current = null;
      invalidatePendingHistoryLoad();

      setSessions([]);
      setMessages([]);
      setActiveSessionId(undefined);
      setHistoryRestoreState("idle");
      setHistoryRestoreError(null);
      void refreshSessions().catch(() => {});
    }
    prevServiceId.current = serviceId;
  }, [serviceId, refreshSessions, setActiveSessionId, invalidatePendingHistoryLoad, handleStopStreaming, invalidatePendingSessionInit]);

  // Track empty state telemetry
  useEffect(() => {
    if (historyLoading || messages.length > 0) return;
    if (historyRestoreState === "loading" || historyRestoreState === "failed") return;

    if (!serviceId) {
      trackChatHistoryEmptyState("playground", {
        state: "service_unselected",
        sessionCount: sessions.length,
        activeSessionId,
      });
      return;
    }

    if (!playgroundSidebarOpen && sessions.length > 0) {
      trackChatHistoryEmptyState("playground", {
        state: "history_hidden",
        sessionCount: sessions.length,
        activeSessionId,
      });
      return;
    }

    if (activeSessionId) {
      trackChatHistoryEmptyState("playground", {
        state: "selected_session_empty",
        sessionCount: sessions.length,
        activeSessionId,
      });
      return;
    }

    trackChatHistoryEmptyState("playground", {
      state: "no_sessions",
      sessionCount: sessions.length,
      activeSessionId,
    });
  }, [
    activeSessionId,
    historyLoading,
    historyRestoreState,
    messages.length,
    playgroundSidebarOpen,
    serviceId,
    sessions.length,
  ]);

  return {
    requestSnapshot: diagnostic?.isCurrent() ? diagnostic.snapshot : undefined,
    requestSnapshotStatus: diagnostic?.isCurrent() ? diagnostic.status : "none",
    onRequestSnapshot,
    copyRequestSnapshot,
    sessions,
    sessionsLoading,
    historyLoading,
    historyRestoreState,
    historyRestoreError,
    sessionEnabled,
    setSessionEnabled,
    messages,
    setMessages,
    refreshSessions,
    handleSelectSession,
    handleNewSession,
    handleDeleteSession,
    handleStopStreaming,
    loadingHistorySessionRef,
    currentRequestSessionRef,
    currentRequestIdRef,
    abortControllerRef,
    sessionThreadIdRef,
    pendingSessionInitRef,
    pendingThreadRef,
    messagesRef,
    interactionStartedRef,
    invalidatePendingHistoryLoad,
    loading,
    setLoading,
  };
}
