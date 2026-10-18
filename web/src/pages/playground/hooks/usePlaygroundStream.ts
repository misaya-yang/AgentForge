import type { RequestBody } from "@/api/contracts";
import type { PlaygroundRequestCapture } from "../utils/requestSnapshot";
import { observeTokenUsage, type TokenObservation } from "@/features/chat/stream/tokenObservation";
import { sdkRunReference, sdkUsageObservation, type SdkRunReference } from "@/features/chat/stream/sdkRun";
import { isSdkLifecycleChunk } from "@/features/chat/stream/sdkLifecycle";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { useCallback, useMemo, useRef } from "react";
import { isNativeAgentSdk } from "@/lib/sdkAgentConfig";
import { prepareDifyConversation } from "../utils/difyConversation";
import { createSession, getSession, updateSession, addSessionMessage } from "@/api/sessions";
import {
  sseFetch,
  sseFetchEvents,
  streamChunkToAGUIEvent,
  type AGUIEvent,
} from "@/lib/sse";
import { useAgentTimeline } from "@/hooks/useAgentTimeline";
import type { ArtifactData } from "@/components/agent/ArtifactCard";
import { getPlaygroundErrorMessage } from "@/lib/utils";
import type { ChatMessage, ToolCallWithResult } from "@/components/ChatWindow";
import type {
  ContentItem,
  StreamChunk,
  ServiceUiPreferences,
} from "@/types/gateway";
import { useAuthStore } from "@/store/useAuthStore";
import {
  applyUsageToTurnState,
  cancelStreamTurn,
  completeStreamTurn,
  createMessageId,
  createStreamReducerContext,
  createStreamTurnState,
  failStreamTurn,
  reduceLegacyStreamChunk,
  setStreamTurnContent,
  type StreamTurnState,
} from "@/features/chat/stream";
import {
  finishChatStreamTrace,
  markChatStreamFirstToken,
  startChatStreamTrace,
} from "@/features/chat/telemetry";
import type { TFunction } from "i18next";
import {
  readReportedRouting,
  buildTextParts,
  estimateTokens,
  extractMessagePayload,
  extractToolCallUpdates,
  extractToolResult,
  normalizeContentDelta,
  normalizeLangGraphEvent,
  type LangGraphStreamEvent,
  type ToolCallUpdate,
} from "../utils/langgraph";
// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------
export interface UsePlaygroundStreamOptions {
  serviceId?: string;
  activeService?: {
    service_id: string;
    name?: string;
    display_name?: string;
    service_type?: string;
    upstream_url?: string | null;
    graph_id?: string | null;
    assistant_id?: string | null;
    connector_config?: Record<string, unknown>;
    metadata?: Record<string, unknown>;
  };
  /** Observe only the actual dispatch attempt; diagnostics must not block inference. */
  onRequestSnapshot?: (capture: PlaygroundRequestCapture, isCurrent: () => boolean) => void;
  /** Live selection fence for custom Dify inputs across asynchronous preparation. */
  isSelectionCurrent?: (serviceId: string, sessionId: string | undefined) => boolean;
  /** Whether session persistence is enabled */
  sessionEnabled: boolean;
  /** Current messages array ref */
  messagesRef: React.MutableRefObject<ChatMessage[]>;
  /** Messages state setter */
  setMessages: React.Dispatch<React.SetStateAction<ChatMessage[]>>;
  /** Loading state setter */
  setLoading: React.Dispatch<React.SetStateAction<boolean>>;
  /** Active session ID from the store */
  activeSessionId?: string;
  /** Store setter for active session ID */
  setActiveSessionId: (id?: string) => void;
  /** Local titles cache */
  localTitles: Record<string, string>;
  /** Local titles setter */
  setLocalTitles: (
    titles:
      | Record<string, string>
      | ((prev: Record<string, string>) => Record<string, string>)
  ) => void;
  /** Sessions list (for thread ID lookup) */
  sessions: { session_id: string; metadata?: Record<string, unknown> | null }[];
  /** Messages array (for fallback content lookup) */
  messages: ChatMessage[];
  /** Ref-based abort controller */
  abortControllerRef: React.MutableRefObject<AbortController | null>;
  /** Request ID counter ref */
  currentRequestIdRef: React.MutableRefObject<number>;
  /** Current request session ref */
  currentRequestSessionRef: React.MutableRefObject<string | null>;
  /** Loading history session ref */
  loadingHistorySessionRef: React.MutableRefObject<string | null>;
  /** Session thread ID map */
  sessionThreadIdRef: React.MutableRefObject<Record<string, string>>;
  /** Pending session init promise */
  pendingSessionInitRef: React.MutableRefObject<Promise<string | null> | null>;
  /** Pending thread creation promise from session init */
  pendingThreadRef: React.MutableRefObject<Promise<string | null> | null>;
  /** Interaction flag */
  interactionStartedRef: React.MutableRefObject<boolean>;
  /** Stick-to-bottom ref */
  stickToBottomRef: React.MutableRefObject<boolean>;
  /** Invalidate pending history load */
  invalidatePendingHistoryLoad: () => void;
  /** Refresh sessions list */
  refreshSessions: (silent?: boolean) => Promise<void>;
  /** i18n translation function */
  t: TFunction;
  /** Current loading state (for uiStreamingActive derivation) */
  loading: boolean;
}
function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === "object"
    ? (value as Record<string, unknown>)
    : undefined;
}
function asSafeString(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value : null;
}
function readGatewayFailoverEvent(value: unknown): Record<string, unknown> | null {
  const data = asRecord(value);
  if (!data || data.type !== "gateway_model_failover") return null;
  return data;
}
// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

export function usePlaygroundStream(opts: UsePlaygroundStreamOptions) {
  const selectedServiceRef = useRef(opts.serviceId);
  selectedServiceRef.current = opts.serviceId;
  const {
    serviceId,
    activeService,
    sessionEnabled,
    setMessages,
    setLoading,
    activeSessionId,
    localTitles,
    setLocalTitles,
    sessions,
    messages,
    abortControllerRef,
    currentRequestIdRef,
    currentRequestSessionRef,
    loadingHistorySessionRef,
    sessionThreadIdRef,
    pendingSessionInitRef,
    pendingThreadRef,
    interactionStartedRef,
    stickToBottomRef,
    invalidatePendingHistoryLoad,
    refreshSessions,
    t,
    loading,
  } = opts;

  // AG-UI Timeline state management
  const {
    state: timelineState,
    processEvent: processTimelineEvent,
    reset: resetTimeline,
  } = useAgentTimeline();

  // Track artifacts collected during streaming
  const artifactsRef = useRef<ArtifactData[]>([]);

  // Derived: is the UI actively streaming?
  const uiStreamingActive = useMemo(() => {
    const lastAssistant = [...messages]
      .reverse()
      .find((message) => message.role === "assistant");
    if (!lastAssistant) return false;
    return (
      loading &&
      (lastAssistant.isStreaming === true || lastAssistant.status === "streaming")
    );
  }, [loading, messages]);

  // UI preference resolution
  const uiPreferences = (activeService?.metadata?.ui_preferences ||
    {}) as ServiceUiPreferences;
  const isLangGraphService =
    activeService?.service_type === "langgraph" ||
    activeService?.metadata?.adapter_type === "langgraph" ||
    activeService?.metadata?.proxy_mode === "transparent";
  const toolCallsMode: "full" | "collapsed" | "hidden" = (uiPreferences.tool_calls_mode as "full" | "collapsed" | "hidden") ?? "full";
  const toolCallsDefaultOpen = uiPreferences.tool_calls_default_open ?? true;
  const showTimeline =
    uiPreferences.hide_timeline == null
      ? !isLangGraphService
      : !uiPreferences.hide_timeline;
  const forceVisibleToolCalls =
    isLangGraphService && !showTimeline;
  const resolvedToolCallsMode: "full" | "collapsed" | "hidden" =
    toolCallsMode === "hidden"
      ? "hidden"
      : forceVisibleToolCalls
        ? "full"
        : toolCallsMode;
  const resolvedToolCallsDefaultOpen = forceVisibleToolCalls
    ? true
    : toolCallsDefaultOpen;

  const handleSend = useCallback(
    async (inputs: ContentItem[], difyInputs?: Record<string, unknown>) => {
      if (!serviceId) return;
      interactionStartedRef.current = true;
      if (loadingHistorySessionRef.current) {
        invalidatePendingHistoryLoad();
      }
      const token = useAuthStore.getState().token;
      const authorityScope = sdkAuthorityScope(useAuthStore.getState().user);
      const isDifyService = activeService?.metadata?.adapter_type === "dify_chat" &&
        activeService.service_type !== "langgraph" && activeService.metadata.proxy_mode !== "transparent";
      const isSdkService = isNativeAgentSdk(activeService?.metadata?.adapter_type);
      // Snapshot before any await; later editor changes cannot alter this dispatch.
      const customDifyInputs = isDifyService && difyInputs !== undefined
        ? JSON.parse(JSON.stringify(difyInputs)) as Record<string, unknown> : undefined;
      const noAutomaticReplay = isDifyService || isSdkService;
      const isTransparentProxyService =
        Boolean(activeService) &&
        (activeService?.service_type === "langgraph" ||
          activeService?.metadata?.adapter_type === "langgraph" ||
          activeService?.metadata?.proxy_mode === "transparent");
      const useTransparentProxy = isTransparentProxyService;
      const text = inputs.find((i) => i.type === "text")?.data || "";
      if (!text) return;
      const inputText = String(text);
      const estimatedInputTokens = estimateTokens(inputText);
      stickToBottomRef.current = true;

      if (abortControllerRef.current) {
        abortControllerRef.current.abort();
      }

      const abortController = new AbortController();
      abortControllerRef.current = abortController;
      const requestId = currentRequestIdRef.current + 1;
      currentRequestIdRef.current = requestId;

      setLoading(true);

      const startTime = performance.now();
      let firstTokenTime: number | null = null;
      const streamTrace = startChatStreamTrace("playground", {
        serviceId,
        transparentProxy: useTransparentProxy,
      });
      let reportedRouting: ReturnType<typeof readReportedRouting>;
      let streamTraceClosed = false;
      const closeStreamTrace = (
        outcome: "completed" | "cancelled" | "failed",
        payload?: Record<string, unknown>
      ) => {
        if (streamTraceClosed) return;
        streamTraceClosed = true;
        finishChatStreamTrace(streamTrace, outcome, { ...payload, ...(reportedRouting ? { reportedRouting } : {}) });
      };
      let toolCallIdCounter = 0;

      let assistantIndex = 0;
      resetTimeline();
      artifactsRef.current = [];

      setMessages((prev) => {
        const userMsgId = createMessageId("user");
        const assistantMsgId = createMessageId("assistant");
        const nowIso = new Date().toISOString();
        const next = [
          // Regenerate can supersede a run without first going through the Stop button.
          ...prev.map(message => message.role === "assistant" && message.isStreaming
            ? { ...message, status: "cancelled" as const, isStreaming: false, isThinking: false }
            : message),
          {
            id: userMsgId,
            role: "user" as const,
            content: inputText,
            createdAt: nowIso,
            parts: buildTextParts(userMsgId, inputText, nowIso),
            status: "completed" as const,
          },
          {
            id: assistantMsgId,
            role: "assistant" as const,
            content: "",
            createdAt: nowIso,
            parts: [],
            status: "streaming" as const,
            toolCalls: [],
            isThinking: true,
            isStreaming: true,
            timeline: undefined,
            artifacts: undefined,
            ...(!isSdkService ? { stats: { tokenUsageSource: "unknown" as const } } : {}),
          },
        ];
        assistantIndex = next.length - 1;
        return next;
      });

      // Declare stream-scoped variables before try so they're accessible in catch/finally
      let streamState = createStreamTurnState(startTime);
      let sdkRun: SdkRunReference | undefined;
      let sdkUsage = sdkUsageObservation(undefined);
      let tokenObservation: TokenObservation = { tokenUsageSource: "unknown" };
      const observedStats = () => ({
        ...tokenObservation,
        ...(reportedRouting ? { reportedRouting } : {}),
        textTokenEstimate: {
          inputTokens: estimatedInputTokens,
          outputTokens: estimateTokens(streamState.content),
          totalTokens: estimatedInputTokens + estimateTokens(streamState.content),
        },
      });
      let sdkSessionId: string | undefined;

      const mapToolCalls = (
        state: StreamTurnState
      ): ToolCallWithResult[] =>
        state.toolCalls.map((toolCall) => ({
          toolCall: {
            tool_call_id: toolCall.id,
            name: toolCall.name,
            arguments: toolCall.arguments,
            status: toolCall.status,
          },
          result: toolCall.result,
          argsText: toolCall.arguments,
          argsValid: toolCall.argsValid,
        }));

      let requestSessionId = activeSessionId;
      const isRequestCurrent = () => currentRequestIdRef.current === requestId &&
        selectedServiceRef.current === serviceId && useAuthStore.getState().token === token &&
        sdkAuthorityScope(useAuthStore.getState().user) === authorityScope &&
        (customDifyInputs === undefined || !opts.isSelectionCurrent || opts.isSelectionCurrent(serviceId, requestSessionId));
      const isRequestValid = () => !abortController.signal.aborted && isRequestCurrent();

      let rafId: number | null = null;

      const flushAssistant = (overrides?: Partial<ChatMessage>, force = false) => {
        // Force may finish an aborted turn, but must never cross an account/view boundary.
        if (!isRequestCurrent()) return;
        if (!force && !isRequestValid()) return;
        const toolCalls = mapToolCalls(streamState);
        const content = streamState.content;

        const currentTimeline =
          timelineState.steps.length > 0
            ? { ...timelineState }
            : undefined;
        const currentArtifacts =
          artifactsRef.current.length > 0
            ? [...artifactsRef.current]
            : undefined;

        setMessages((m) => {
          if (!isRequestCurrent()) return m;
          const next = [...m];
          if (next[assistantIndex]) {
            const messageId =
              next[assistantIndex].id ||
              createMessageId("assistant");
            const createdAt =
              next[assistantIndex].createdAt ||
              new Date().toISOString();
            const textParts = buildTextParts(
              messageId,
              content,
              createdAt
            );
            next[assistantIndex] = {
              ...next[assistantIndex],
              id: messageId,
              content,
              createdAt,
              parts: textParts,
              status:
                overrides?.status ??
                (streamState.status === "failed"
                  ? "failed"
                  : streamState.status === "cancelled"
                    ? "cancelled"
                    : overrides?.isStreaming === false
                      ? "completed"
                      : "streaming"),
              toolCalls,
              sdkToolActivity: streamState.sdkToolActivity,
              sdkAuthorityScope: authorityScope,
              sdkRun,
              isThinking: false,
              isStreaming:
                overrides?.isStreaming ??
                streamState.status === "streaming",
              timeline: currentTimeline,
              artifacts: currentArtifacts,
              ...(!isSdkService ? { stats: observedStats() } : {}),
              ...overrides,
            };
          }
          return next;
        });
      };

      try {
        let effectiveSessionId: string | undefined = undefined;

        if (pendingSessionInitRef.current) {
          // A failed creation must fail this send. Retrying creation or falling
          // through to the previously selected session would change its target.
          const pendingSessionId = await pendingSessionInitRef.current;
          if (pendingSessionId) requestSessionId = pendingSessionId;
          if (!isRequestValid()) return;
          if (!pendingSessionId) throw new Error("playground_session_initialization_interrupted");
          effectiveSessionId = pendingSessionId;
          currentRequestSessionRef.current = pendingSessionId;
        }

        // Cancellation or a view/account change while initializing must not dispatch a request.
        if (!isRequestValid()) return;

        if (sessionEnabled) {
          if (!effectiveSessionId && !activeSessionId) {
            const created = await createSession({ service_id: serviceId });
            if (!isRequestValid()) return;
            effectiveSessionId = created.session_id;
            currentRequestSessionRef.current = created.session_id;
            requestSessionId = created.session_id;
            opts.setActiveSessionId(created.session_id);
            refreshSessions(true).catch(() => {});
          } else if (!effectiveSessionId) {
            effectiveSessionId = activeSessionId;
            currentRequestSessionRef.current = activeSessionId ?? null;
          }
        }

        if (effectiveSessionId && !localTitles[effectiveSessionId]) {
          const titleText = inputText.trim().split("\n")[0].slice(0, 40);
          if (titleText) {
            setLocalTitles((prev) => ({
              ...prev,
              [effectiveSessionId!]: titleText,
            }));
            updateSession(effectiveSessionId, {
              metadata: { title: titleText },
            }).catch(() => {});
          }
        }

        const shouldPersistManually =
          useTransparentProxy && Boolean(effectiveSessionId);
        if (shouldPersistManually) {
          addSessionMessage(effectiveSessionId!, {
            role: "user",
            content: inputText,
          }).catch(() => {});
        }

        let threadId: string | undefined;
        if (useTransparentProxy && sessionEnabled && effectiveSessionId) {
          // 1. Check cache first (instant, no await)
          threadId =
            sessionThreadIdRef.current[effectiveSessionId] ||
            (sessions.find((s) => s.session_id === effectiveSessionId)?.metadata
              ?.langgraph_thread_id as string | undefined);

          // 2. If pre-creation is pending, give it a SHORT window (500ms max)
          //    to resolve. Don't block indefinitely — that was causing 8-22s TTFT.
          if (!threadId && pendingThreadRef.current) {
            const pendingThread = pendingThreadRef.current;
            try {
              const preCreatedId = await Promise.race([
                pendingThread,
                new Promise<null>((r) => setTimeout(() => r(null), 500)),
              ]);
              if (preCreatedId) {
                threadId = preCreatedId;
              }
            } catch {
              // Pre-creation failed, fall through
            } finally {
              if (pendingThreadRef.current === pendingThread) pendingThreadRef.current = null;
            }
            if (!isRequestValid()) return;
            if (!threadId) {
              threadId = sessionThreadIdRef.current[effectiveSessionId];
            }
          }

          // 3. If still no thread, create one NON-BLOCKING.
          //    Start streaming immediately; the thread will be ready by the time
          //    LangGraph processes the run (it creates thread + run atomically).
          if (!threadId) {
            // Fire-and-forget thread creation (save for future messages in this session)
            fetch(
              `/api/v1/proxy/${serviceId}/threads`,
              {
                method: "POST",
                headers: {
                  "Content-Type": "application/json",
                  ...(token ? { Authorization: `Bearer ${token}` } : {}),
                },
                body: JSON.stringify(({
                  metadata: { gateway_session_id: effectiveSessionId },
                } satisfies RequestBody<"POST /api/v1/proxy/{service_name}/threads">)),
              }
            )
              .then((resp) => (resp.ok ? resp.json() : null))
              .then((data) => {
                if (!isRequestCurrent()) return;
                if (data) {
                  const tid = data.thread_id || data.id || data.threadId;
                  if (tid && effectiveSessionId) {
                    sessionThreadIdRef.current[effectiveSessionId] = tid;
                    updateSession(effectiveSessionId, {
                      metadata: { langgraph_thread_id: tid },
                    }).catch(() => {});
                  }
                }
              })
              .catch(() => {});
            // Don't set threadId — streaming will use stateless mode for first message,
            // or the proxy will route to a new thread automatically.
          }
        }

        if (!isRequestValid()) return;
        sdkSessionId = effectiveSessionId;
        const difyConversation = isDifyService
          ? await prepareDifyConversation(serviceId, effectiveSessionId, isRequestValid)
          : undefined;
        if (!isRequestValid()) return;
        const req = {
          service_id: serviceId,
          inputs: [{ type: "text" as const, data: inputText }],
          session_id: effectiveSessionId,
          ...((difyConversation?.parameters || customDifyInputs !== undefined) ? { parameters: {
            ...difyConversation?.parameters,
            ...(customDifyInputs !== undefined ? { inputs: customDifyInputs } : {}),
          } } : {}),
        };

        const streamReducerContext = createStreamReducerContext();
        let nativeStreamCompleted = false;
        const toolChunkIdByIndex = new Map<number, string>();
        const toolChunkNameByIndex = new Map<number, string>();

        const scheduleFlush = () => {
          if (rafId !== null) return;
          rafId = requestAnimationFrame(() => {
            rafId = null;
            flushAssistant();
          });
        };

        const cancelFlush = () => {
          if (rafId !== null) {
            cancelAnimationFrame(rafId);
            rafId = null;
          }
        };

        // Process AG-UI events and update timeline
        const processAGUIEvent = (event: AGUIEvent) => {
          processTimelineEvent(event);

          if (
            event.event === "artifact_created" ||
            event.event === "file_created"
          ) {
            const artifact: ArtifactData = {
              id: (event.artifact_id ||
                event.file_id ||
                `art-${Date.now()}`) as string,
              type:
                (event.artifact_type as ArtifactData["type"]) || "file",
              name: (event.name ||
                event.file_name ||
                "File") as string,
              url: event.url as string,
              mimeType: event.mime_type as string,
              size: event.size as number,
              status: "ready",
            };
            if (
              !artifactsRef.current.some((a) => a.id === artifact.id)
            ) {
              artifactsRef.current.push(artifact);
            }
          }

          if (event.event === "document_generation_result") {
            const artifact: ArtifactData = {
              id: (event.file_id || `doc-${Date.now()}`) as string,
              type: "document",
              name: (event.title || "Document") as string,
              url: event.url as string,
              format: event.doc_type as string,
              status: "ready",
            };
            if (
              !artifactsRef.current.some((a) => a.id === artifact.id)
            ) {
              artifactsRef.current.push(artifact);
            }
          }

          if (event.event === "image_generation_result") {
            const artifact: ArtifactData = {
              id: `img-${Date.now()}`,
              type: "image",
              name: ((event.prompt as string) || "Image").slice(0, 30),
              url: event.url as string,
              status: "ready",
            };
            if (
              !artifactsRef.current.some((a) => a.id === artifact.id)
            ) {
              artifactsRef.current.push(artifact);
            }
          }
        };

        const processStreamChunk = (
          chunk: StreamChunk,
          options?: { updateTimeline?: boolean }
        ): boolean => {
          if (!isSdkService) tokenObservation = observeTokenUsage(tokenObservation, chunk.metadata?.usage);
          if (isSdkService) {
            const reference = sdkRunReference(chunk.metadata, sdkSessionId);
            if (reference && sdkRun && (reference.runId !== sdkRun.runId || reference.engine !== sdkRun.engine)) {
              throw new Error("sdk_run_identity_mismatch");
            }
            if (reference && !sdkRun) {
              sdkRun = reference;
              // A metadata-only admission chunk must expose controls before any text/RAF.
              flushAssistant({ status: "streaming", isStreaming: true });
            }
            if (chunk.metadata?.usage) sdkUsage = sdkUsageObservation(chunk.metadata.usage);
            // Admission publishes controls, but is not provider/content first-token timing.
            if (reference && chunk.chunk_index === 0 && chunk.event_type === "text_delta" &&
                !chunk.is_final && chunk.content?.data === "" && !chunk.metadata?.usage &&
                !chunk.metadata?.sdk_lifecycle) return false;
          }
          if (options?.updateTimeline !== false && !isSdkLifecycleChunk(chunk)) {
            const aguiEvent = streamChunkToAGUIEvent(chunk);
            processAGUIEvent(aguiEvent);
          }

          const now = performance.now();
          const reduced = reduceLegacyStreamChunk(
            streamState,
            chunk,
            streamReducerContext,
            now
          );
          streamState = reduced.state;

          if (
            streamState.firstTokenMs != null &&
            firstTokenTime === null
          ) {
            firstTokenTime = startTime + streamState.firstTokenMs;
            markChatStreamFirstToken(
              streamTrace,
              streamState.firstTokenMs
            );
          }

          if (reduced.changed) {
            scheduleFlush();
          }

          return reduced.terminal;
        };
        const processNativeLangGraphChunk = (chunk: StreamChunk): boolean => {
          nativeStreamCompleted ||= chunk.is_final;
          return processStreamChunk(chunk, { updateTimeline: false });
        };

        let syntheticChunkIndex = 0;
        const nextSyntheticChunkIndex = () => {
          syntheticChunkIndex += 1;
          return syntheticChunkIndex;
        };

        const resolveToolCallUpdate = (
          update: ToolCallUpdate
        ): ToolCallUpdate | null => {
          let id = update.id;
          let name = update.name;

          if (update.index != null) {
            if (id) {
              toolChunkIdByIndex.set(update.index, id);
            } else {
              id = toolChunkIdByIndex.get(update.index) || "";
            }

            if (name) {
              toolChunkNameByIndex.set(update.index, name);
            } else {
              name = toolChunkNameByIndex.get(update.index) || "";
            }

            if (!id && name) {
              id = `tool-${update.index}-${name}`;
              toolChunkIdByIndex.set(update.index, id);
            }
          }

          if (!id && !name) {
            return null;
          }

          return { ...update, id, name };
        };

        try {
          if (useTransparentProxy) {
            const payload = {
              input: {
                messages: [{ role: "user", content: inputText }],
              },
              // LangGraph Agent Server names the token stream mode
              // "messages-tuple"; library-level graph.stream() calls use
              // "messages". Keep updates/custom for tool state and gateway
              // failover notices.
              stream_mode: ["messages-tuple", "updates", "custom"],
              // "exit" = only persist final state, not intermediate checkpoints.
              // Eliminates ~2-3s overhead per graph step (model/tools).
              // Safe because we don't use human-in-the-loop interrupts.
              durability: "exit",
            };
            const streamPath = threadId
              ? `/api/v1/proxy/${serviceId}/threads/${threadId}/runs/stream`
              : `/api/v1/proxy/${serviceId}/runs/stream`;
            opts.onRequestSnapshot?.({
              attempt_id: `${requestId}-${Date.now()}`, protocol: "langgraph", service_id: serviceId,
              session_id: effectiveSessionId, memory_enabled: sessionEnabled, thread_id: threadId, body: payload,
            }, isRequestCurrent);
            let lastCumulativeContent = "";
            // Tracks the last content we applied for each message id, so
            // we can tell post-middleware mutations apart from cross-turn
            // leakage in 'updates' events. Same id + same content = skip.
            // Same id + different content = post-middleware modification.
            const lastContentById = new Map<string, string>();
            // Track message IDs already processed to prevent cross-turn
            // contamination in multi-turn threads.  LangGraph `updates`
            // events can include the full thread state (all historical
            // messages), so we skip any message whose ID was already seen.
            const seenMessageIds = new Set<string>();
            const seenToolCallIds = new Set<string>();
            const shownFailoverNotices = new Set<string>();
            for await (const evt of sseFetchEvents<unknown>(
              streamPath,
              {
                method: "POST",
                headers: {
                  "Content-Type": "application/json",
                  Accept: "text/event-stream",
                  ...(token
                    ? { Authorization: `Bearer ${token}` }
                    : {}),
                },
                body: JSON.stringify((payload satisfies RequestBody<"POST /api/v1/proxy/{service_name}/runs/stream">)),
                signal: abortController.signal,
                preserveControlFrames: true,
              }
            )) {
              if (!isRequestValid()) {
                break;
              }

              const normalized = normalizeLangGraphEvent(
                evt as LangGraphStreamEvent
              );
              const eventName = normalized.event || "";
              const eventData = normalized.data;

              if (
                !eventName &&
                eventData &&
                typeof eventData === "object" &&
                "event_type" in
                  (eventData as Record<string, unknown>)
              ) {
                if (
                  processNativeLangGraphChunk(
                    eventData as StreamChunk
                  )
                ) {
                  break;
                }
                continue;
              }

              if (eventName.split("|", 1)[0] === "error") {
                throw new Error(
                  typeof eventData === "string"
                    ? eventData
                    : "LangGraph stream error"
                );
              }

              if (eventName === "end" || (!eventName && eventData === "[DONE]")) {
                nativeStreamCompleted = true;
                break;
              }

              if (
                eventName === "metadata" &&
                eventData &&
                typeof eventData === "object"
              ) {
                const usage = (
                  eventData as Record<string, unknown>
                ).usage;
                if (
                  usage &&
                  typeof usage === "object" &&
                  !Array.isArray(usage)
                ) {
                  tokenObservation = observeTokenUsage(tokenObservation, usage);
                  scheduleFlush();
                  streamState = applyUsageToTurnState(
                    streamState,
                    usage as Record<string, unknown>
                  );
                }
                continue;
              }

              if (eventName === "custom") {
                const failoverEvent = readGatewayFailoverEvent(eventData);
                if (failoverEvent) {
                  const status = asSafeString(failoverEvent.status);
                  const providerId = asSafeString(failoverEvent.provider_id);
                  const modelId = asSafeString(failoverEvent.model_id);
                  const reason = asSafeString(failoverEvent.reason);
                  const noticeKey = `${status || "unknown"}:${providerId || "-"}:${modelId || "-"}`;
                  if (status === "selected") {
                    // Replace only with validated upstream observations, never configured candidates.
                    reportedRouting = readReportedRouting(failoverEvent);
                    scheduleFlush();
                    continue;
                  }
                  if (status === "exhausted" && !shownFailoverNotices.has(noticeKey)) {
                    shownFailoverNotices.add(noticeKey);
                    setMessages((current) => [
                      ...current,
                      {
                        id: `model-failover-${Date.now()}-${shownFailoverNotices.size}`,
                        role: "assistant",
                        content: t("playground.modelFailover.exhausted"),
                        createdAt: new Date().toISOString(),
                        status: "failed",
                        meta: {
                          type: "gateway_model_failover_notice",
                          failover_status: status,
                          provider_id: providerId,
                          model_id: modelId,
                          reason,
                        },
                      },
                    ]);
                  }
                  continue;
                }
              }

              // Handle 'updates' events
              if (
                eventName === "updates" &&
                eventData &&
                typeof eventData === "object"
              ) {
                const data =
                  eventData as Record<string, unknown>;
                for (const [, nodeData] of Object.entries(
                  data
                )) {
                  if (
                    nodeData &&
                    typeof nodeData === "object"
                  ) {
                    const nd =
                      nodeData as Record<string, unknown>;
                    const msgs = nd.messages as unknown[];
                    if (
                      Array.isArray(msgs) &&
                      msgs.length > 0
                    ) {
                      for (const msg of msgs) {
                        if (
                          !msg ||
                          typeof msg !== "object"
                        )
                          continue;
                        const message =
                          msg as Record<
                            string,
                            unknown
                          >;

                        // De-dup vs cross-turn leakage, BUT allow
                        // post-middleware modifications through.
                        //
                        // Backend agent middleware (SourcesFormat,
                        // SourcesLabel, etc.) mutate the final AIMessage in
                        // after_model, preserving its id so LangGraph's
                        // add_messages reducer upserts. Those updates arrive
                        // via 'updates' events with the SAME id as the
                        // streamed chunks — if we skip purely on id, the UI
                        // stays on the pre-middleware raw stream.
                        //
                        // Rule: skip when we've seen this id AND the content
                        // is identical to what we already applied for that
                        // specific id. Different content for the same id =
                        // mutation; let it fall through to the AIMessage
                        // accumulator below, which replaces content that
                        // no longer continues lastCumulativeContent.
                        const msgId =
                          (message.id as string) || "";
                        const usageMeta =
                          message.usage_metadata as
                            | Record<string, unknown>
                            | undefined;
                        // A usage-only receipt is meaningful even when its text is a duplicate.
                        tokenObservation = observeTokenUsage(tokenObservation, usageMeta);
                        if (usageMeta) scheduleFlush();
                        const updatedContent =
                          normalizeContentDelta(message);
                        if (msgId && seenMessageIds.has(msgId)) {
                          if (
                            !updatedContent ||
                            updatedContent ===
                              lastContentById.get(msgId)
                          ) {
                            continue;
                          }
                          // Post-middleware mutation — fall through.
                        }
                        if (msgId) seenMessageIds.add(msgId);
                        if (msgId && updatedContent) {
                          lastContentById.set(msgId, updatedContent);
                        }

                        if (usageMeta) {
                          streamState =
                            applyUsageToTurnState(
                              streamState,
                              usageMeta
                            );
                        }

                        const toolUpdates =
                          extractToolCallUpdates(message);
                        for (const update of toolUpdates) {
                          const resolvedUpdate =
                            resolveToolCallUpdate(update);
                          if (!resolvedUpdate) continue;
                          const tcId =
                            resolvedUpdate.id ||
                            `tc-${Date.now()}`;
                          // Skip tool calls already seen
                          // from messages/* events
                          if (
                            tcId &&
                            seenToolCallIds.has(tcId)
                          )
                            continue;
                          if (tcId)
                            seenToolCallIds.add(tcId);
                          const eventType =
                            streamState.toolCalls.some(
                              (tc) => tc.id === tcId
                            )
                              ? "tool_call_delta"
                              : "tool_call_start";
                          processNativeLangGraphChunk({
                            request_id: "",
                            chunk_index:
                              nextSyntheticChunkIndex(),
                            is_final: false,
                            event_type: eventType,
                            tool_call: {
                              tool_call_id: tcId,
                              name:
                                resolvedUpdate.name || "",
                              arguments:
                                resolvedUpdate.args || "",
                              status: "running",
                            },
                            content: {
                              type: "tool_call",
                              data: "",
                            },
                          });
                        }

                        const toolResult =
                          extractToolResult(message);
                        if (toolResult) {
                          const existingArgs =
                            streamState.toolCalls.find(
                              (tc) =>
                                tc.id ===
                                toolResult.id
                            )?.arguments || "";
                          processNativeLangGraphChunk({
                            request_id: "",
                            chunk_index:
                              nextSyntheticChunkIndex(),
                            is_final: false,
                            event_type: "tool_result",
                            tool_call: {
                              tool_call_id:
                                toolResult.id ||
                                `auto-${++toolCallIdCounter}`,
                              name:
                                toolResult.name ||
                                "",
                              arguments:
                                existingArgs,
                              status: toolResult.status,
                            },
                            content: {
                              type: "tool_result",
                              data: toolResult.content,
                            },
                          });
                        }

                        const msgType = (
                          (message.type as string) ||
                          ""
                        ).toLowerCase();
                        const role = (
                          (message.role as string) ||
                          ""
                        ).toLowerCase();
                        const isToolMessage =
                          msgType === "tool" ||
                          msgType === "toolmessage" ||
                          role === "tool";

                        const isAIMessage =
                          !isToolMessage &&
                          (msgType.includes("ai") ||
                            role === "assistant" ||
                            (message.content &&
                              !msgType &&
                              !role));

                        if (isAIMessage) {
                          const content =
                            normalizeContentDelta(
                              message
                            );
                          if (
                            content &&
                            content.length > 0
                          ) {
                            if (
                              content ===
                              lastCumulativeContent
                            ) {
                              // Exact duplicate - skip
                            } else if (
                              content.startsWith(
                                lastCumulativeContent
                              ) &&
                              lastCumulativeContent.length >
                                0
                            ) {
                              const delta =
                                content.slice(
                                  lastCumulativeContent.length
                                );
                              lastCumulativeContent =
                                content;
                              if (delta) {
                                processNativeLangGraphChunk(
                                  {
                                    request_id:
                                      "",
                                    chunk_index:
                                      nextSyntheticChunkIndex(),
                                    is_final:
                                      false,
                                    event_type:
                                      "text_delta",
                                    content: {
                                      type: "text",
                                      data: delta,
                                    },
                                  }
                                );
                              }
                            } else if (
                              lastCumulativeContent.startsWith(
                                content
                              )
                            ) {
                              // New content is prefix of old - skip
                            } else if (
                              content.length >=
                              lastCumulativeContent.length
                            ) {
                              lastCumulativeContent =
                                content;
                              streamState =
                                setStreamTurnContent(
                                  streamState,
                                  ""
                                );
                              processNativeLangGraphChunk(
                                {
                                  request_id:
                                    "",
                                  chunk_index:
                                    nextSyntheticChunkIndex(),
                                  is_final:
                                    false,
                                  event_type:
                                    "text_delta",
                                  content: {
                                    type: "text",
                                    data: content,
                                  },
                                }
                              );
                            }
                          }
                        }
                      }
                    }
                  }
                }
                continue;
              }

              // Handle messages/complete
              if (eventName === "messages/complete") {
                const msgArr = Array.isArray(eventData)
                  ? eventData
                  : [];
                for (const msg of msgArr) {
                  if (!msg || typeof msg !== "object")
                    continue;
                  const message =
                    msg as Record<string, unknown>;
                  // Track ID AND content so 'updates' can distinguish
                  // redundant echoes (skip) from post-middleware mutations
                  // (apply, because content will differ).
                  const msgId =
                    (message.id as string) || "";
                  if (msgId) seenMessageIds.add(msgId);
                  if (msgId) {
                    const completeContent =
                      normalizeContentDelta(message);
                    if (completeContent) {
                      lastContentById.set(msgId, completeContent);
                    }
                  }
                  const msgType = (
                    (message.type as string) || ""
                  ).toLowerCase();
                  const role = (
                    (message.role as string) || ""
                  ).toLowerCase();
                  const isToolMessage =
                    msgType === "tool" ||
                    msgType === "toolmessage" ||
                    role === "tool";

                  const usageMeta =
                    message.usage_metadata as
                      | Record<string, unknown>
                      | undefined;
                  if (usageMeta) {
                    tokenObservation = observeTokenUsage(tokenObservation, usageMeta);
                    scheduleFlush();
                    streamState = applyUsageToTurnState(
                      streamState,
                      usageMeta
                    );
                  }

                  // Extract tool results from completed tool messages
                  // (critical: must do this HERE since seenMessageIds
                  //  will cause the updates handler to skip this message)
                  if (isToolMessage) {
                    const toolResult =
                      extractToolResult(message);
                    if (toolResult) {
                      const existingArgs =
                        streamState.toolCalls.find(
                          (tc) =>
                            tc.id === toolResult.id
                        )?.arguments || "";
                      processNativeLangGraphChunk({
                        request_id: "",
                        chunk_index:
                          nextSyntheticChunkIndex(),
                        is_final: false,
                        event_type: "tool_result",
                        tool_call: {
                          tool_call_id:
                            toolResult.id ||
                            `auto-${++toolCallIdCounter}`,
                          name:
                            toolResult.name || "",
                          arguments: existingArgs,
                          status: toolResult.status,
                        },
                        content: {
                          type: "tool_result",
                          data: toolResult.content,
                        },
                      });
                    }
                  }

                  const isAIMessage =
                    !isToolMessage &&
                    (msgType.includes("ai") ||
                      role === "assistant" ||
                      (!msgType &&
                        !role &&
                        message.content));

                  if (isAIMessage) {
                    const content =
                      normalizeContentDelta(message);
                    if (content && content.length > 0) {
                      const currentContent =
                        streamState.content;
                      if (
                        content === currentContent ||
                        content ===
                          lastCumulativeContent
                      ) {
                        // duplicate
                      } else if (
                        content.startsWith(
                          currentContent
                        ) &&
                        currentContent.length > 0
                      ) {
                        const delta = content.slice(
                          currentContent.length
                        );
                        if (delta) {
                          lastCumulativeContent =
                            content;
                          processNativeLangGraphChunk({
                            request_id: "",
                            chunk_index:
                              nextSyntheticChunkIndex(),
                            is_final: false,
                            event_type:
                              "text_delta",
                            content: {
                              type: "text",
                              data: delta,
                            },
                          });
                        }
                      } else if (
                        currentContent.startsWith(
                          content
                        )
                      ) {
                        // skip
                      } else if (
                        content.length >=
                        currentContent.length
                      ) {
                        lastCumulativeContent =
                          content;
                        streamState =
                          setStreamTurnContent(
                            streamState,
                            ""
                          );
                        processNativeLangGraphChunk({
                          request_id: "",
                          chunk_index:
                            nextSyntheticChunkIndex(),
                          is_final: false,
                          event_type: "text_delta",
                          content: {
                            type: "text",
                            data: content,
                          },
                        });
                      }
                    }
                  }
                }
                // A completed message can precede more tools/messages or a run error.
                // Wait for the root end/[DONE] or a genuine Gateway terminal.
                continue;
              }

              // Handle messages/* (partial)
              if (
                eventName.startsWith("messages") &&
                eventName !== "messages/complete"
              ) {
                const payload =
                  extractMessagePayload(eventData);
                if (!payload) {
                  continue;
                }
                const message = payload.message;
                // Track message ID for cross-event dedup
                const msgId =
                  (message.id as string) || "";
                if (msgId) seenMessageIds.add(msgId);
                const msgType = (
                  (message.type as string) || ""
                ).toLowerCase();
                const role = (
                  (message.role as string) || ""
                ).toLowerCase();
                const isToolMessage =
                  msgType === "tool" ||
                  msgType === "toolmessage" ||
                  role === "tool";

                const usageMeta =
                  message.usage_metadata as
                    | Record<string, unknown>
                    | undefined;
                if (usageMeta) {
                  tokenObservation = observeTokenUsage(tokenObservation, usageMeta);
                  scheduleFlush();
                  streamState = applyUsageToTurnState(
                    streamState,
                    usageMeta
                  );
                }

                const toolUpdates =
                  extractToolCallUpdates(message);
                for (const update of toolUpdates) {
                  const resolvedUpdate =
                    resolveToolCallUpdate(update);
                  if (!resolvedUpdate) continue;
                  const tcId =
                    resolvedUpdate.id ||
                    `auto-${++toolCallIdCounter}`;
                  // Track for cross-event dedup
                  if (tcId) seenToolCallIds.add(tcId);
                  const eventType =
                    streamState.toolCalls.some(
                      (tc) => tc.id === tcId
                    )
                      ? "tool_call_delta"
                      : "tool_call_start";
                  const shouldStop = processNativeLangGraphChunk({
                    request_id: "",
                    chunk_index:
                      nextSyntheticChunkIndex(),
                    is_final: false,
                    event_type: eventType,
                    tool_call: {
                      tool_call_id: tcId,
                      name: resolvedUpdate.name || "",
                      arguments: resolvedUpdate.args || "",
                      status: "running",
                    },
                    content: {
                      type: "tool_call",
                      data: "",
                    },
                  });
                  if (shouldStop) break;
                }

                const toolResult =
                  extractToolResult(message);
                if (toolResult) {
                  const existingArgs =
                    streamState.toolCalls.find(
                      (tc) => tc.id === toolResult.id
                    )?.arguments || "";
                  processNativeLangGraphChunk({
                    request_id: "",
                    chunk_index:
                      nextSyntheticChunkIndex(),
                    is_final: false,
                    event_type: "tool_result",
                    tool_call: {
                      tool_call_id:
                        toolResult.id ||
                        `auto-${++toolCallIdCounter}`,
                      name: toolResult.name || "",
                      arguments: existingArgs,
                      status: toolResult.status,
                    },
                    content: {
                      type: "tool_result",
                      data: toolResult.content,
                    },
                  });
                }

                if (!isToolMessage) {
                  const cumulativeContent =
                    normalizeContentDelta(message);
                  if (
                    cumulativeContent &&
                    cumulativeContent.length > 0
                  ) {
                    const isMessageChunk =
                      eventName === "messages" ||
                      msgType.includes("chunk");
                    // Track per-id content so a later 'updates' event with
                    // the same id + same content can skip, while one with
                    // modified content (post-middleware mutation) falls
                    // through to the AIMessage accumulator.
                    if (msgId) {
                      lastContentById.set(
                        msgId,
                        isMessageChunk
                          ? `${lastContentById.get(msgId) || ""}${cumulativeContent}`
                          : cumulativeContent
                      );
                    }
                    if (isMessageChunk) {
                      lastCumulativeContent = `${lastCumulativeContent}${cumulativeContent}`;
                      processNativeLangGraphChunk({
                        request_id: "",
                        chunk_index:
                          nextSyntheticChunkIndex(),
                        is_final: false,
                        event_type: "text_delta",
                        content: {
                          type: "text",
                          data: cumulativeContent,
                        },
                      });
                      continue;
                    }
                    if (
                      cumulativeContent ===
                      lastCumulativeContent
                    ) {
                      // skip
                    } else if (
                      cumulativeContent.startsWith(
                        lastCumulativeContent
                      ) &&
                      lastCumulativeContent.length > 0
                    ) {
                      const actualDelta =
                        cumulativeContent.slice(
                          lastCumulativeContent.length
                        );
                      lastCumulativeContent =
                        cumulativeContent;
                      if (actualDelta) {
                        processNativeLangGraphChunk({
                          request_id: "",
                          chunk_index:
                            nextSyntheticChunkIndex(),
                          is_final: false,
                          event_type: "text_delta",
                          content: {
                            type: "text",
                            data: actualDelta,
                          },
                        });
                      }
                    } else if (
                      lastCumulativeContent.startsWith(
                        cumulativeContent
                      )
                    ) {
                      // skip
                    } else if (
                      cumulativeContent.length >=
                      lastCumulativeContent.length
                    ) {
                      lastCumulativeContent =
                        cumulativeContent;
                      streamState =
                        setStreamTurnContent(
                          streamState,
                          ""
                        );
                      processNativeLangGraphChunk({
                        request_id: "",
                        chunk_index:
                          nextSyntheticChunkIndex(),
                        is_final: false,
                        event_type: "text_delta",
                        content: {
                          type: "text",
                          data: cumulativeContent,
                        },
                      });
                    }
                  }
                }
              }
            }
          } else {
            opts.onRequestSnapshot?.({
              attempt_id: `${requestId}-${Date.now()}`, protocol: "gateway", service_id: serviceId,
              session_id: effectiveSessionId, memory_enabled: sessionEnabled,
              // Diagnostic consumers never receive arbitrary custom field values.
              body: customDifyInputs === undefined ? req : { ...req, parameters: difyConversation?.parameters },
            }, isRequestCurrent);
            if (!isRequestValid()) return;
            for await (const chunk of sseFetch<StreamChunk>(
              "/api/v1/stream",
              {
                method: "POST",
                headers: {
                  "Content-Type": "application/json",
                },
                body: JSON.stringify((req satisfies RequestBody<"POST /api/v1/stream">)),
                signal: abortController.signal,
              }
            )) {
              if (!isRequestValid()) {
                break;
              }

              nativeStreamCompleted ||= chunk.is_final;
              // A received Dify text delta belongs to this turn even if saving
              // its conversation binding fails. Keep terminal processing after
              // the write so a failed binding never publishes completion.
              const receivedDifyText = difyConversation && !chunk.is_final &&
                chunk.event_type === "text_delta";
              if (receivedDifyText) processStreamChunk(chunk);
              await difyConversation?.accept(chunk.metadata);
              if (!receivedDifyText && processStreamChunk(chunk)) {
                break;
              }
            }
          }
        } catch (streamErr) {
          // A failed transport does not prove the accepted request did not run.
          // Keep one request per explicit Send, including failures before output.
          cancelFlush();
          throw streamErr;
        }

        cancelFlush();
        if (isRequestValid() && isNativeAgentSdk(activeService?.metadata?.adapter_type) && !nativeStreamCompleted) {
          throw new Error(t("services.sdkAgent.interrupted"));
        }

        if (!isRequestValid()) {
          // Force-update the message so it doesn't stay stuck in streaming
          flushAssistant({
            status: "cancelled",
            isStreaming: false,
          }, true);
          closeStreamTrace("cancelled", {
            reason: "request_invalidated",
          });
          return;
        }

        // EOF only closes the transport. Completion needs an adapter terminal;
        // partial text or tool activity cannot prove the run succeeded.
        if (!isSdkService && !nativeStreamCompleted &&
            streamState.status !== "completed" && streamState.status !== "failed") {
          throw new Error("playground_stream_incomplete");
        }
        if (!isSdkService && streamState.status === "failed") {
          throw new Error(streamState.error || "stream_error");
        }

        const endTime = performance.now();
        streamState = completeStreamTurn(streamState, endTime);
        const durationMs =
          streamState.durationMs ??
          Math.round(endTime - startTime);
        const firstTokenMs =
          streamState.firstTokenMs ??
          (firstTokenTime
            ? Math.round(firstTokenTime - startTime)
            : undefined);
        const inputTokens = isSdkService ? sdkUsage.inputTokens : tokenObservation.inputTokens;
        const outputTokens = isSdkService ? sdkUsage.outputTokens : tokenObservation.outputTokens;
        const totalTokens = isSdkService ? sdkUsage.totalTokens : tokenObservation.totalTokens;

        const completedEmptyAgentRun = streamState.status === "completed" &&
          (isDifyService || (isSdkService && sdkRun !== undefined && nativeStreamCompleted));
        if (noAutomaticReplay && !completedEmptyAgentRun && !streamState.content && !streamState.sdkToolActivity.length && streamState.status !== "failed") {
          throw new Error(t("playground.errors.noStreamContent", "No response was received from the agent. Please retry."));
        }
        // A confirmed terminal can legitimately contain no text or only tools.
        // Thread history is not bound to this run, so it cannot supply a missing
        // answer or turn successful completion into a history-fetch failure.
        flushAssistant({
          status: streamState.status,
          completionConfirmed: streamState.status === "completed",
          isStreaming: false,
          stats: {
            ...(!isSdkService ? observedStats() : {}),
            durationMs,
            firstTokenMs,
            inputTokens,
            outputTokens,
            totalTokens,
          },
        });

        if (
          shouldPersistManually &&
          effectiveSessionId &&
          (streamState.content || reportedRouting ||
            streamState.toolCalls.length > 0)
        ) {
          const toolCalls = streamState.toolCalls.map(
            (toolCall) => ({
              tool_call_id: toolCall.id,
              name: toolCall.name,
              arguments: toolCall.arguments,
              result: toolCall.result ?? null,
              status: toolCall.status,
            })
          );
          addSessionMessage(effectiveSessionId, {
            role: "assistant",
            content: streamState.content,
            metadata: {
              tool_calls: toolCalls,
              stats: {
                duration_ms: durationMs,
                first_token_ms: firstTokenMs,
                input_tokens: inputTokens,
                output_tokens: outputTokens,
                total_tokens: totalTokens,
                ...(reportedRouting ? { reported_routing: reportedRouting } : {}),
                token_usage_source: tokenObservation.tokenUsageSource,
                token_provenance: tokenObservation.tokenProvenance,
                text_token_estimate: {
                  input_tokens: estimatedInputTokens,
                  output_tokens: estimateTokens(streamState.content),
                  total_tokens: estimatedInputTokens + estimateTokens(streamState.content),
                },
              },
            },
          }).catch(() => {});
        }
        const finalOutcome =
          streamState.status === "failed"
            ? "failed"
            : streamState.status === "cancelled"
              ? "cancelled"
              : "completed";
        closeStreamTrace(finalOutcome, {
          durationMs,
          firstTokenMs,
          totalTokens,
          ...(!isSdkService ? { tokenUsageSource: tokenObservation.tokenUsageSource, tokenProvenance: tokenObservation.tokenProvenance } : {}),
          toolCalls: streamState.toolCalls.length,
          ...(streamState.error
            ? { error: streamState.error }
            : {}),
        });
      } catch (err) {
        if (!isRequestCurrent()) return;
        if (
          err instanceof Error &&
          err.name === "AbortError"
        ) {
          streamState = cancelStreamTurn(
            streamState,
            performance.now()
          );
          flushAssistant({
            status: "cancelled",
            isStreaming: false,
          }, true);
          closeStreamTrace("cancelled", {
            reason: "abort_signal_outer",
          });
          return;
        }

        const message = err instanceof Error && err.message === "playground_stream_incomplete"
          ? t("playground.errors.incompleteStream")
          : getPlaygroundErrorMessage(err, t);
        const errorContent = streamState.content ? `${streamState.content}\n\n${message}` : message;
        streamState = failStreamTurn(
          streamState,
          message,
          performance.now()
        );
        closeStreamTrace("failed", {
          error: streamState.error || message,
        });
        setMessages((m) => {
          if (!isRequestCurrent()) return m;
          const next = [...m];
          if (next[assistantIndex]) {
            const messageId =
              next[assistantIndex].id ||
              createMessageId("assistant");
            const createdAt =
              next[assistantIndex].createdAt ||
              new Date().toISOString();
            next[assistantIndex] = {
              ...next[assistantIndex],
              id: messageId,
              content: errorContent,
              createdAt,
              parts: buildTextParts(
                messageId,
                errorContent,
                createdAt
              ),
              status: "failed",
              isThinking: false,
              isStreaming: false,
              toolCalls: mapToolCalls(streamState),
              ...(!isSdkService ? { stats: observedStats() } : {}),
              ...(isSdkService ? { sdkRun, sdkAuthorityScope: authorityScope } : {}),
            };
          } else {
            const messageId =
              createMessageId("assistant");
            const createdAt = new Date().toISOString();
            next.push({
              id: messageId,
              role: "assistant",
              content: message,
              createdAt,
              parts: buildTextParts(
                messageId,
                message,
                createdAt
              ),
              status: "failed",
            });
          }
          return next;
        });
      } finally {
        if (!streamTraceClosed) {
          closeStreamTrace("cancelled", {
            reason: "finalized_without_outcome",
          });
        }
        if (isRequestCurrent()) {
          setLoading(false);
        }
        if (
          abortControllerRef.current === abortController
        ) {
          abortControllerRef.current = null;
        }
        // Safety net: ensure no assistant message is left stuck in streaming state
        setMessages((m) => {
          if (!isRequestCurrent()) return m;
          const last = m[assistantIndex];
          if (last && last.isStreaming) {
            const next = [...m];
            next[assistantIndex] = {
              ...last,
              isStreaming: false,
              isThinking: false,
              status: last.status === "streaming" ? "cancelled" : last.status,
            };
            return next;
          }
          return m;
        });
        // Sync only the active session's title instead of a full list refresh.
        // Full refreshSessions() causes sidebar flash because the API returns
        // new updated_at timestamps, triggering a full React re-render.
        if (sessionEnabled && serviceId && activeSessionId && isRequestCurrent()) {
          getSession(activeSessionId)
            .then((s) => {
              if (!isRequestCurrent()) return;
              const title = s.metadata?.title as string | undefined;
              if (title) {
                opts.setLocalTitles((prev: Record<string, string>) => ({
                  ...prev,
                  [activeSessionId]: title,
                }));
              }
            })
            .catch(() => {});
        }
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [
      serviceId,
      activeService,
      sessionEnabled,
      activeSessionId,
      localTitles,
      sessions,
      messages,
      t,
    ]
  );

  return {
    handleSend,
    uiStreamingActive,
    timelineState,
    // UI preference resolution
    showTimeline,
    forceVisibleToolCalls,
    resolvedToolCallsMode,
    resolvedToolCallsDefaultOpen,
    showThinkingIndicator: true,
  };
}
