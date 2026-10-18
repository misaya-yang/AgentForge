import type { RequestBody } from "@/api/contracts";
/**
 * Playground page – slim orchestration layer.
 *
 * All heavy logic lives in the extracted hooks and utilities:
 *  - usePlaygroundSessions  – session CRUD, history restore
 *  - usePlaygroundStream    – SSE / AG-UI streaming, fallback recovery
 *  - useScrollToBottom      – auto-scroll behaviour
 *  - langgraph utils        – pure parsing helpers
 */
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { usePlaygroundServices } from "@/hooks/useServices";
import { cn, getPlaygroundErrorMessage } from "@/lib/utils";
import { toast } from "@/hooks/use-toast";
import { ChatWindow } from "@/components/ChatWindow";
import { MultimodalInput } from "@/components/MultimodalInput";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { useDifyInputs } from "./hooks/useDifyInputs";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { MessageSquarePlus, Trash2, ArrowDown, PanelLeft, X, Copy } from "lucide-react";
import { useAuthStore } from "@/store/useAuthStore";
import { PlaygroundServiceStatus } from "./PlaygroundServiceStatus";
import { useAppStore } from "@/store/useAppStore";
import { useChatShortcuts } from "@/features/chat/shortcuts";
import { formatDateTime } from "@/utils/intl";

import { usePlaygroundSessions } from "./hooks/usePlaygroundSessions";
import { usePlaygroundStream } from "./hooks/usePlaygroundStream";
import { useScrollToBottom } from "./hooks/useScrollToBottom";

const PLAYGROUND_COMPOSER_ID = "playground-chat-composer";

export function PlaygroundPage() {
  const { t, i18n } = useTranslation();
  const servicesQuery = usePlaygroundServices();
  const services = useMemo(() => servicesQuery.data ?? [], [servicesQuery.data]);
  const availableServices = useMemo(() => services.filter(service => service.enabled), [services]);
  const canManageServices = useAuthStore(state => state.hasPermission("console:services:view"));

  const {
    selectedServiceId: serviceId,
    setSelectedServiceId: setServiceId,
    activeSessionId,
    setActiveSessionId,
    playgroundSidebarOpen,
    setPlaygroundSidebarOpen,
    localTitles,
    setLocalTitles,
  } = useAppStore();

  const [isMobile, setIsMobile] = useState(false);
  const [showToolCalls, setShowToolCalls] = useState(() => {
    if (typeof window === "undefined") return true;
    const stored = window.localStorage.getItem("showToolCalls");
    return stored === null ? true : stored === "true";
  });

  const activeService = services.find((s) => s.service_id === serviceId);
  // A catalog observation gates new actions, never the existing stream or its Stop control.
  const serviceReady = servicesQuery.isSuccess && activeService?.enabled === true;
  const serviceEntryState = servicesQuery.isPending ? "loading"
    : servicesQuery.isError ? "error"
    : availableServices.length === 0 ? "empty"
    : serviceId && !serviceReady ? "unavailable"
    : !serviceId ? "unselected" : undefined;

  // ---------------------------------------------------------------------------
  // Hooks
  // ---------------------------------------------------------------------------

  const sessionHook = usePlaygroundSessions({
    serviceId,
    services: availableServices,
    onSessionCreationError: (error) => toast.error(
      t("playground.errors.sessionCreationFailed"),
      getPlaygroundErrorMessage(error, t),
    ),
  });
  const {
    requestSnapshot,
    requestSnapshotStatus,
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
    loading,
    setLoading,
    // refs for streaming hook
    abortControllerRef,
    currentRequestIdRef,
    currentRequestSessionRef,
    loadingHistorySessionRef,
    sessionThreadIdRef,
    pendingSessionInitRef,
    pendingThreadRef,
    messagesRef,
    interactionStartedRef,
    invalidatePendingHistoryLoad,
  } = sessionHook;

  const { scrollRef, showScrollToBottom, scrollToBottom, stickToBottomRef } =
    useScrollToBottom(messages);

  const isDifyService = activeService?.metadata?.adapter_type === "dify_chat" &&
    activeService.service_type !== "langgraph" && activeService.metadata.proxy_mode !== "transparent";
  const difyInputs = useDifyInputs(serviceId, activeSessionId, isDifyService, sessionEnabled);

  const streamHook = usePlaygroundStream({
    onRequestSnapshot,
    isSelectionCurrent: (targetService, targetSession) => {
      const selection = useAppStore.getState();
      return selection.selectedServiceId === targetService && selection.activeSessionId === targetSession;
    },
    serviceId,
    activeService,
    sessionEnabled,
    messagesRef,
    setMessages,
    setLoading,
    activeSessionId,
    setActiveSessionId,
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
  });

  const {
    handleSend: sendRequest,
    uiStreamingActive,
    showTimeline,
    forceVisibleToolCalls,
    resolvedToolCallsMode,
    resolvedToolCallsDefaultOpen,
    showThinkingIndicator,
  } = streamHook;

  const handleSend = async (inputs: Parameters<typeof sendRequest>[0]) => {
    if (!serviceReady || !difyInputs.isCurrent() || (isDifyService && (difyInputs.invalid || loading))) return;
    const release = isDifyService ? difyInputs.beginSend() : () => {};
    if (!release) return;
    try { await sendRequest(inputs, isDifyService ? difyInputs.value : undefined); } finally { release(); }
  };

  const effectiveShowToolCalls =
    resolvedToolCallsMode !== "hidden" && (showToolCalls || forceVisibleToolCalls);

  // ---------------------------------------------------------------------------
  // Side effects
  // ---------------------------------------------------------------------------

  useEffect(() => {
    const mediaQuery = window.matchMedia("(max-width: 767px)");
    const sync = () => setIsMobile(mediaQuery.matches);
    sync();
    mediaQuery.addEventListener("change", sync);
    return () => mediaQuery.removeEventListener("change", sync);
  }, []);

  useEffect(() => {
    if (!isMobile || !playgroundSidebarOpen) return;
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setPlaygroundSidebarOpen(false);
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [isMobile, playgroundSidebarOpen, setPlaygroundSidebarOpen]);

  useEffect(() => {
    if (typeof window === "undefined") return;
    window.localStorage.setItem("showToolCalls", showToolCalls ? "true" : "false");
  }, [showToolCalls]);

  // Debug window state (dev only)
  useEffect(() => {
    if (!import.meta.env.DEV || typeof window === "undefined") return;
    const target = window as typeof window & {
      __playgroundDebug?: Record<string, unknown>;
    };
    target.__playgroundDebug = {
      loading,
      uiStreamingActive,
      serviceId,
      activeSessionId,
      requestId: currentRequestIdRef.current,
      hasAbortController: Boolean(abortControllerRef.current),
      abortSignalAborted: abortControllerRef.current?.signal.aborted ?? null,
      messageCount: messages.length,
      lastAssistantStatus: [...messages]
        .reverse()
        .find((message) => message.role === "assistant")?.status,
      lastAssistantStreaming: [...messages]
        .reverse()
        .find((message) => message.role === "assistant")?.isStreaming,
    };
  }, [activeSessionId, loading, messages, serviceId, uiStreamingActive, abortControllerRef, currentRequestIdRef]);

  useChatShortcuts({
    surface: "playground",
    composerId: PLAYGROUND_COMPOSER_ID,
    enabled: Boolean(serviceId) || uiStreamingActive,
    onNewChat: serviceReady && !uiStreamingActive ? () => void handleNewSession() : undefined,
    onStop: loading ? handleStopStreaming : undefined,
  });

  // ---------------------------------------------------------------------------
  // Render
  // ---------------------------------------------------------------------------

  return (
    <div
      className="relative flex w-full overflow-hidden bg-background"
      style={{
        height: isMobile
          ? "calc(100dvh - 72px)"
          : "calc(100dvh - 86px)",
      }}
    >
      {/* Sessions Sidebar */}
      {playgroundSidebarOpen && (
        <aside
          role={isMobile ? "dialog" : undefined}
          aria-modal={isMobile || undefined}
          aria-label={t("assistant.showHistory", "Conversation history")}
          className={cn(
            "flex-col border-r border-border/70 bg-background",
            isMobile
              ? "absolute inset-y-0 left-0 z-30 flex w-[min(88vw,390px)] shadow-xl"
              : "hidden w-[280px] md:flex"
          )}
        >
        <div className="h-14 flex items-center px-4">
          <Button
            size="sm"
            onClick={() => void handleNewSession()}
            disabled={!serviceReady || uiStreamingActive}
            className="h-10 w-full gap-2.5 rounded-lg border border-transparent bg-muted/45 font-medium text-foreground transition-colors duration-150 hover:bg-muted/70"
          >
            <div className="flex h-5 w-5 items-center justify-center rounded-md bg-primary/10">
              <MessageSquarePlus className="h-3 w-3 text-primary" />
            </div>
            {t("playground.newChat", "New chat")}
          </Button>
          {isMobile && (
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="ml-2 shrink-0"
              onClick={() => setPlaygroundSidebarOpen(false)}
              aria-label={t("playground.hideHistory", "Hide history")}
            >
              <X className="h-4 w-4" />
            </Button>
          )}
        </div>

        <div className="flex-1 overflow-y-auto p-2">
          {!serviceId ? (
            <div className="px-3 py-8 text-sm text-muted-foreground text-center">
              <div className="mb-3 mx-auto w-10 h-10 rounded-xl bg-muted/50 flex items-center justify-center">
                <MessageSquarePlus className="h-5 w-5 text-muted-foreground/50" />
              </div>
              {t("playground.selectAgentToView", "Select an agent to view chats.")}
            </div>
          ) : sessionsLoading ? (
            <div className="px-3 py-8 text-sm text-muted-foreground text-center">
              <div className="animate-pulse flex flex-col items-center">
                <div className="h-8 w-8 rounded-lg bg-muted mb-2" />
                <div className="h-3 w-20 rounded bg-muted" />
              </div>
            </div>
          ) : sessions.length === 0 ? (
            <div className="px-3 py-8 text-sm text-muted-foreground text-center">
              <div className="mb-3 mx-auto w-10 h-10 rounded-xl bg-muted/50 flex items-center justify-center">
                <MessageSquarePlus className="h-5 w-5 text-muted-foreground/50" />
              </div>
              {t("playground.noChatsYet", "No chats yet. Start a new one.")}
            </div>
          ) : (
            <div className="space-y-0.5">
              {sessions.map((s) => {
                const title =
                  localTitles[s.session_id] ||
                  (s.metadata?.title as string | undefined) ||
                  (s.metadata?.name as string | undefined) ||
                  t("playground.newChat", "New chat");
                const ts = (s.created_at || s.updated_at) as string;
                const active = activeSessionId === s.session_id;
                return (
                  <div
                    key={s.session_id}
                    className={cn(
                      "group flex w-full items-start rounded-lg border-l-2 py-1 pr-1 transition-colors duration-150",
                      active
                        ? "border-l-primary bg-primary/8"
                        : "border-l-transparent hover:bg-muted/55"
                    )}
                  >
                    <button
                      type="button"
                      onClick={() => {
                        void handleSelectSession(s.session_id);
                        if (isMobile) setPlaygroundSidebarOpen(false);
                      }}
                      className="min-w-0 flex-1 px-2 py-1.5 text-left"
                      aria-current={active ? "page" : undefined}
                    >
                      <div className="flex items-start justify-between gap-2">
                      <div className="min-w-0 flex-1">
                        <div className={cn(
                          "truncate text-sm font-medium transition-colors",
                          active ? "text-foreground" : "text-foreground/80 group-hover:text-foreground"
                        )}>
                          {title}
                        </div>
                        <div className={cn(
                          "text-[11px] mt-0.5 transition-colors",
                          active ? "text-primary" : "text-muted-foreground group-hover:text-muted-foreground/80"
                        )}>
                          {formatDateTime(ts, i18n.language)}
                        </div>
                      </div>
                    </div>
                    </button>
                    <button
                      type="button"
                      onClick={() => void handleDeleteSession(s.session_id)}
                      className="mt-1 rounded-md p-2 text-muted-foreground opacity-70 transition-colors hover:bg-destructive/10 hover:text-destructive hover:opacity-100 focus-visible:opacity-100"
                      aria-label={`${t("playground.deleteChat", "Delete chat")}: ${title}`}
                      title={t("common.delete", "Delete")}
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </button>
                  </div>
                );
              })}
            </div>
          )}
        </div>
        </aside>
      )}

      {isMobile && playgroundSidebarOpen && (
        <button
          type="button"
          className="absolute inset-0 z-20 bg-black/40"
          aria-label={t("playground.hideHistory", "Hide history")}
          onClick={() => setPlaygroundSidebarOpen(false)}
        />
      )}

      <div className="relative flex min-h-0 flex-1 flex-col overflow-hidden">
      {/* Header / Config Bar */}
      <div className="flex min-h-12 items-center bg-background px-3 py-1 sm:px-6">
        <div className="mx-auto flex w-full max-w-4xl flex-wrap items-center justify-between gap-2">
          <div className="flex min-w-0 flex-1 items-center gap-2 sm:gap-3">
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="h-9 w-9 rounded-lg border border-transparent"
              onClick={() => setPlaygroundSidebarOpen(!playgroundSidebarOpen)}
              aria-label={
                playgroundSidebarOpen
                  ? t("playground.hideHistory", "Hide history")
                  : t("playground.showHistory", "Show history")
              }
            >
              <PanelLeft className="h-4 w-4" />
            </Button>
            <div className="hidden h-9 w-9 items-center justify-center rounded-lg bg-primary/10 text-primary sm:flex">
              <svg xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round"><path d="m12 3-1.912 5.813a2 2 0 0 1-1.275 1.275L3 12l5.813 1.912a2 2 0 0 1 1.275 1.275L12 21l1.912-5.813a2 2 0 0 1 1.275-1.275L21 12l-5.813-1.912a2 2 0 0 1-1.275-1.275L12 3Z" /></svg>
            </div>
            <div className="flex flex-col -space-y-0.5">
              <span className="text-[10px] font-semibold text-muted-foreground/70 uppercase tracking-wider">{t("playground.agent", "Agent")}</span>
                <Select value={serviceId} onValueChange={setServiceId} disabled={!servicesQuery.isSuccess || availableServices.length === 0}>
                  <SelectTrigger className="h-7 w-[min(48vw,200px)] rounded-md border-0 bg-transparent p-0 text-sm font-semibold shadow-none focus:ring-2 focus:ring-ring/30 focus:ring-offset-0">
                    <SelectValue placeholder={t("playground.selectAgent", "Select an Agent")} />
                  </SelectTrigger>
                  <SelectContent>
                    {availableServices.map((s) => (
                      <SelectItem key={s.service_id} value={s.service_id}>
                        {s.name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            </div>

          <div className="flex items-center gap-1 sm:gap-2">
            <label className="flex cursor-pointer select-none items-center gap-2 rounded-md bg-muted/30 px-2 py-1.5 transition-colors hover:bg-muted/50 sm:px-3">
              <input
                type="checkbox"
                id="session-toggle"
                checked={sessionEnabled}
                onChange={(e) => setSessionEnabled(e.target.checked)}
                className="h-3.5 w-3.5 rounded border-muted-foreground/30 accent-primary"
                aria-label={t("playground.memory", "Memory")}
              />
              <span className="hidden text-xs font-medium sm:inline">{t("playground.memory", "Memory")}</span>
            </label>

            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                if (sessionEnabled) {
                  void handleNewSession();
                } else {
                  setMessages([]);
                  setActiveSessionId(undefined);
                }
              }}
              disabled={uiStreamingActive || (sessionEnabled ? !serviceReady : messages.length === 0)}
              className="h-8 gap-1.5 px-2 text-muted-foreground hover:text-foreground sm:px-3"
              aria-label={t("common.clear", "Clear")}
            >
              <Trash2 className="h-3.5 w-3.5" />
              <span className="hidden sm:inline">{t("common.clear", "Clear")}</span>
            </Button>
          </div>
        </div>
      </div>

      {serviceEntryState && (
        <PlaygroundServiceStatus
          state={serviceEntryState}
          canManageServices={canManageServices}
          refreshing={servicesQuery.isFetching}
          onRefresh={() => { void servicesQuery.refetch(); }}
        />
      )}

      {requestSnapshot && (
        <div className="mx-auto flex w-full max-w-4xl flex-wrap items-center gap-2 px-3 py-2 text-xs text-muted-foreground sm:px-6">
          <Button type="button" variant="outline" size="sm" onClick={async () => {
            try {
              await copyRequestSnapshot();
              toast.success(t("playground.snapshot.copied"));
            } catch {
              toast.error(t("playground.snapshot.copyFailed"));
            }
          }}>
            <Copy className="mr-1 h-3.5 w-3.5" />{t("playground.snapshot.copy")}
          </Button>
          <span>{typeof requestSnapshot.input_text === "string"
            ? t("playground.snapshot.includesText") : t("playground.snapshot.settingsOnly")}</span>
          {requestSnapshotStatus === "failed" && <span role="status">{t("playground.snapshot.saveFailed")}</span>}
          {requestSnapshotStatus === "saved" && <span>{t("playground.snapshot.saved")}</span>}
        </div>
      )}

      {/* Chat Area */}
      <div ref={scrollRef} className="flex-1 overflow-y-auto pb-48 min-h-0 bg-background">
        {!serviceId ? (
          <div className="flex h-full flex-col items-center justify-center p-8 text-center">
            <div className="mb-5 flex h-16 w-16 items-center justify-center rounded-2xl bg-primary/10 text-primary">
              <svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round"><path d="m12 3-1.912 5.813a2 2 0 0 1-1.275 1.275L3 12l5.813 1.912a2 2 0 0 1 1.275 1.275L12 21l1.912-5.813a2 2 0 0 1 1.275-1.275L21 12l-5.813-1.912a2 2 0 0 1-1.275-1.275L12 3Z" /></svg>
            </div>
            <h2 className="text-xl font-semibold tracking-tight text-foreground">{t("playground.welcomeTitle", "How can I help you today?")}</h2>
            <p className="mt-2 text-sm text-muted-foreground max-w-md">{t("playground.welcomeDescription", "Select an agent service from the dropdown above to start a conversation.")}</p>
          </div>
        ) : historyLoading ? (
          <div className="flex h-full items-center justify-center p-8 text-sm text-muted-foreground">
            {t("playground.loadingHistory", "Loading chat history...")}
          </div>
        ) : messages.length === 0 ? (
          /* Empty state */
          historyRestoreState === "loading" || historyRestoreState === "failed" ? (
          <div className="flex h-full items-center justify-center p-8">
            <div className="max-w-md rounded-3xl border border-transparent dark:border-transparent bg-card/70 px-6 py-5 text-sm shadow-xs">
              <div className="font-medium text-foreground">
                {historyRestoreState === "loading" && activeSessionId
                  ? t("playground.restoringSessionTitle", "Restoring selected conversation")
                  : t("playground.restoreFailedTitle", "Couldn't restore the selected conversation")}
              </div>
              <div className="mt-2 text-muted-foreground">
                {historyRestoreState === "loading" && activeSessionId
                  ? t("playground.restoringSessionDescription", "We are loading the latest messages and tool activity for this conversation.")
                  : t("playground.restoreFailedDescription", "The conversation still exists, but the last restore attempt did not finish. Retry it or start a fresh chat.")}
              </div>
              {historyRestoreState === "failed" && activeSessionId && (
                <div className="mt-4 flex flex-wrap gap-2">
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => void handleSelectSession(activeSessionId)}
                  >
                    {t("common.retry", "Retry")}
                  </Button>
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    onClick={() => void handleNewSession()}
                    disabled={!serviceReady || uiStreamingActive}
                  >
                    {t("playground.newChat", "New chat")}
                  </Button>
                </div>
              )}
              {historyRestoreState === "failed" && historyRestoreError && (
                <div className="mt-3 truncate text-xs text-muted-foreground">
                  {historyRestoreError}
                </div>
              )}
            </div>
          </div>
          ) : (
          /* Normal empty state for all agents */
          <div className="flex h-full flex-col items-center justify-center p-8 text-center">
            <div className="mb-5 flex h-16 w-16 items-center justify-center rounded-2xl bg-primary/10 text-primary">
              <svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round"><path d="m12 3-1.912 5.813a2 2 0 0 1-1.275 1.275L3 12l5.813 1.912a2 2 0 0 1 1.275 1.275L12 21l1.912-5.813a2 2 0 0 1 1.275-1.275L21 12l-5.813-1.912a2 2 0 0 1-1.275-1.275L12 3Z" /></svg>
            </div>
            <h2 className="text-xl font-semibold tracking-tight">{t("playground.typeToStart", "Type a message to start.")}</h2>
            <p className="mt-2 text-sm text-muted-foreground max-w-md">
              {t(
                "playground.typeToStartHint",
                "Debug your model services here before wiring them into agents."
              )}
            </p>
          </div>
          )
        ) : (
          <ChatWindow
            messages={messages}
            showToolCalls={effectiveShowToolCalls}
            toolCallsMode={resolvedToolCallsMode}
            toolCallsDefaultOpen={resolvedToolCallsDefaultOpen}
            showTimeline={showTimeline}
            showThinkingIndicator={showThinkingIndicator}
            onShare={async () => {
              if (!activeSessionId || messages.length === 0) return;
              try {
                const resp = await fetch(`/api/v1/assistant/sessions/${encodeURIComponent(activeSessionId)}/share`, {
                  method: "POST",
                  headers: { "Content-Type": "application/json" },
                  body: JSON.stringify(({ include_artifacts: true } satisfies RequestBody<"POST /api/v1/assistant/sessions/{session_id}/share">)),
                });
                if (!resp.ok) {
                  const errText = await resp.text();
                  throw new Error(errText || `Status ${resp.status}`);
                }
                const data = await resp.json();
                const shareUrl = `${window.location.origin}${data.share_url || `/share/${data.share_code}`}`;
                // Copy to clipboard (with HTTP fallback)
                try {
                  await navigator.clipboard.writeText(shareUrl);
                } catch {
                  const ta = document.createElement("textarea");
                  ta.value = shareUrl;
                  ta.style.position = "fixed";
                  ta.style.opacity = "0";
                  document.body.appendChild(ta);
                  ta.select();
                  document.execCommand("copy");
                  document.body.removeChild(ta);
                }
                alert(`Share link copied to clipboard!\n${shareUrl}`);
              } catch (e: unknown) {
                const message = e instanceof Error ? e.message : "unknown error";
                alert(`Failed to create share link: ${message}`);
              }
            }}
            onRegenerate={serviceReady ? () => {
              // Find last user message and resend via existing handleSend
              const lastUserMsg = [...messages].reverse().find(m => m.role === "user");
              if (lastUserMsg) {
                handleSend([{ type: "text", data: lastUserMsg.content }]);
              }
            } : undefined}
          />
        )}
      </div>

      {/* Scroll to Bottom Button */}
      {showScrollToBottom && messages.length > 0 && (
        <button
          onClick={scrollToBottom}
          className={cn(
            "absolute bottom-40 left-1/2 -translate-x-1/2 z-10",
            "flex items-center justify-center",
            "h-9 w-9 rounded-full",
            "border border-border bg-card/95 shadow-sm backdrop-blur-xs",
            "text-muted-foreground hover:text-foreground hover:bg-accent",
            "transition-colors duration-150",
            ""
          )}
          aria-label={t("playground.scrollToBottom", "Scroll to bottom")}
        >
          <ArrowDown className="h-4 w-4" />
        </button>
      )}

      {/* Floating Input Area */}
      <div className="absolute bottom-0 left-0 w-full bg-background/95 px-3 pb-[max(12px,env(safe-area-inset-bottom))] pt-3 backdrop-blur-sm sm:px-6 sm:pb-5">
        <div className="mx-auto w-full max-w-4xl">
          <div className="overflow-hidden rounded-xl border border-border bg-card/95 transition-[border-color,box-shadow] focus-within:border-primary/40 focus-within:ring-2 focus-within:ring-ring/15">
            {isDifyService && (
              <details className="border-b border-border px-3 py-2" open={difyInputs.invalid || undefined}>
                <summary className="cursor-pointer text-sm">{t("playground.difyInputs.label", "Dify custom inputs (JSON)")}</summary>
                <p id="dify-inputs-help" className="my-2 text-xs text-muted-foreground">{t("playground.difyInputs.help", "Requires native Dify chat support. Enter the field names configured in your Dify chat app as a JSON object. Values are sent to the configured Dify service. Do not enter passwords or API keys. This draft is not saved and clears when the service, session or account changes.")}</p>
                <Textarea id="dify-custom-inputs" aria-label={t("playground.difyInputs.label", "Dify custom inputs (JSON)")}
                  aria-describedby="dify-inputs-help dify-inputs-error" aria-invalid={difyInputs.invalid}
                  value={difyInputs.text} placeholder='{"topic": "example"}' rows={3} autoComplete="off" spellCheck={false}
                  disabled={!serviceReady || loading} onChange={event => { if (!loading && serviceReady) difyInputs.setText(event.target.value); }} />
                {difyInputs.invalid && <p id="dify-inputs-error" role="alert" className="mt-1 text-sm text-destructive">{t("playground.difyInputs.invalid", "Enter a valid JSON object with finite numbers, or leave this field empty.")}</p>}
                <Button type="button" variant="ghost" disabled={!serviceReady || loading || !difyInputs.text} onClick={() => { if (!loading && serviceReady) difyInputs.setText(""); }}>{t("playground.difyInputs.clear", "Clear inputs")}</Button>
              </details>
            )}
            <MultimodalInput
              onSend={handleSend}
              onStop={handleStopStreaming}
              isStreaming={uiStreamingActive}
              composerId={PLAYGROUND_COMPOSER_ID}
              disabled={!serviceReady || uiStreamingActive || (isDifyService && (difyInputs.invalid || loading))}
              includeFiles={true}
            />
          </div>
          <div className="mt-2 flex flex-wrap items-center justify-between gap-2 px-1 text-xs text-muted-foreground/80 sm:px-2">
            <label className="flex items-center gap-2 cursor-pointer select-none hover:text-foreground transition-colors">
              <Switch
                id="toggle-tool-calls"
                checked={forceVisibleToolCalls ? true : showToolCalls}
                onCheckedChange={setShowToolCalls}
                disabled={resolvedToolCallsMode === "hidden" || forceVisibleToolCalls}
              />
              {t("playground.showToolCalls", "Show tool calls")}
            </label>
            <span className="text-[10px]">
              {t("playground.disclaimer", "AI responses may be inaccurate")}
            </span>
          </div>
        </div>
      </div>
    </div>
  </div>
  );
}
