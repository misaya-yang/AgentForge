/**
 * Assistant Page - Enterprise-grade AI Chat Interface
 *
 * Refactored modular architecture.
 */

import { lazy, Suspense, useEffect, useState, useRef, useCallback, useMemo, useLayoutEffect, Component, type ErrorInfo, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { motion, AnimatePresence, useReducedMotion } from "motion/react";
import {
  ArrowDown,
  PanelLeftClose,
  PanelLeft,
  FileText,
  AlertCircle,
  Share2,
  Sparkles,
  MonitorCog,
  Network,
  Brain,
  Wrench,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { ConversationSidebar } from "@/components/ConversationSidebar";
import {
  listDatasets,
  type DatasetInfo,
} from "@/api/assistant";
import { createSession, listSessions } from "@/api/sessions";
import { api as apiClient } from "@/lib/api";
import { cn } from "@/lib/utils";
import { useToast } from "@/hooks/use-toast";
import { useAuthStore } from "@/store/useAuthStore";

// Local Components & Hooks
import { useAssistantModelCatalog } from "./hooks/useAssistantModelCatalog";
import { AssistantModelStatus } from "./components/AssistantModelStatus";
import { CompactModelSelector } from "./components/CompactModelSelector";
import { preferredAssistantModelId, resolveAssistantModel, savedAssistantModelSelection } from "./modelSelection";
import {
  assistantDraftKey,
  bindNewAssistantDraft,
  clearAdmittedAssistantDrafts,
  readAssistantDrafts,
  transitionAssistantDraft,
  writeAssistantDrafts,
} from "./sessionDrafts";
import { AgentTaskTimeline, type AgentTask } from "./components/AgentTaskTimeline";
import { ChatInputArea } from "./components/ChatInputArea";
import {
  readHydratedLastModelId,
  readLastModelId,
  writeLastModelId,
} from "./lastModel";
import { WelcomeScreen } from "./components/WelcomeScreen";
import { useLocalOSControl } from "./local-os/useLocalOSControl";
import {
  RightPanelContext,
  type RightPanel,
  type RightPanelState,
} from "./components/rightPanelContext";
import { useChatSession } from "./hooks/useChatSession";
import { useFileHandler } from "./hooks/useFileHandler";
import { useImageGeneration } from "./hooks/useImageGeneration";
import { DEFAULT_STYLE_ID } from "./styles";
import { ASSISTANT_COMPACT_MEDIA_QUERY } from "./layout";
import i18n from "@/i18n";
import { useChatShortcuts } from "@/features/chat/shortcuts";
import { useAppStore } from "@/store/useAppStore";
import { trackChatHistoryEmptyState } from "@/features/chat/telemetry";
import {
  reasoningOptionsForModel,
  resolveReasoningOptionId,
} from "@/features/models/modelCapabilities";

const ASSISTANT_UI_V2 = import.meta.env.VITE_ASSISTANT_UI_V2 !== "false";
const ASSISTANT_COMPOSER_ID = "assistant-chat-composer";
const ChatMessage = lazy(async () => {
  const module = await import("./components/ChatMessage");
  return { default: module.ChatMessage };
});
const ActivityPanel = lazy(async () => {
  const module = await import("./components/ActivityPanel");
  return { default: module.ActivityPanel };
});
const ArtifactsPanel = lazy(async () => {
  const module = await import("@/components/artifacts/ArtifactsPanel");
  return { default: module.ArtifactsPanel };
});
const SubAgentWorkspacePanel = lazy(async () => {
  const module = await import("./components/SubAgentWorkspacePanel");
  return { default: module.SubAgentWorkspacePanel };
});
const LocalOSPanel = lazy(async () => {
  const module = await import("./local-os/LocalOSPanel");
  return { default: module.LocalOSPanel };
});
const ShareDialog = lazy(async () => {
  const module = await import("./components/ShareDialog");
  return { default: module.ShareDialog };
});
const MemoryDialog = lazy(async () => {
  const module = await import("./components/MemoryDialog");
  return { default: module.MemoryDialog };
});
const ToolsDialog = lazy(async () => {
  const module = await import("./components/ToolsDialog");
  return { default: module.ToolsDialog };
});
const ConnectorsPanel = lazy(() => import("./components/ConnectorsPanel"));

function LazyPanelFallback() {
  return (
    <div className="flex h-full min-h-32 items-center justify-center text-sm text-[hsl(var(--assistant-text-secondary))]">
      Loading…
    </div>
  );
}

function countUniqueArtifactAffordances(
  artifacts: Array<{ id?: string | null }>,
  outputFiles: Array<{
    artifact_id?: string | null;
    filename?: string | null;
    download_url?: string | null;
  }>
): number {
  const keys = new Set<string>();
  for (const artifact of artifacts) {
    if (artifact.id) {
      keys.add(`artifact:${artifact.id}`);
    }
  }
  for (const file of outputFiles) {
    if (file.artifact_id) {
      keys.add(`artifact:${file.artifact_id}`);
      continue;
    }
    const fallback = file.download_url || file.filename;
    if (fallback) {
      keys.add(`file:${fallback}`);
    }
  }
  return keys.size;
}

/**
 * Top-bar chip for toggling a right-side panel (Activity / Artifacts).
 * Active state shows a 1.5px gold underline stripe; rest state has no
 * background. Shares the `act-btn` motion treatment so the whole bar
 * feels coherent with the Activity panel's controls.
 */
function RightPanelChip({
  icon,
  label,
  count,
  active,
  disabled,
  compact = false,
  onClick,
}: {
  icon: ReactNode;
  label: string;
  count?: number;
  active: boolean;
  disabled?: boolean;
  compact?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      aria-pressed={active}
      aria-label={typeof count === "number" && count > 0 ? `${label} (${count})` : label}
      className={cn(
        "act-btn relative inline-flex items-center gap-1.5 h-8 px-2.5 rounded-md",
        compact && "w-8 justify-center gap-0 px-0",
        "text-[12.5px] transition-colors",
        "disabled:opacity-40 disabled:cursor-not-allowed",
        active
          ? "text-[hsl(var(--assistant-text-primary))]"
          : "text-[hsl(var(--assistant-text-secondary))] hover:text-[hsl(var(--assistant-text-primary))] hover:bg-[hsl(var(--assistant-surface-soft))]",
      )}
    >
      <span
        className={cn(
          active
            ? "text-[hsl(var(--assistant-accent))]"
            : "text-[hsl(var(--assistant-text-tertiary))]",
        )}
      >
        {icon}
      </span>
      <span className={compact ? "sr-only" : undefined}>{label}</span>
      {typeof count === "number" && count > 0 && (
        <span className={cn(
          "font-mono tabular-nums text-[11px] text-[hsl(var(--assistant-text-tertiary))]",
          compact && "sr-only",
        )}>
          {count}
        </span>
      )}
      {active && (
        <span
          aria-hidden
          className="absolute left-2.5 right-2.5 bottom-[2px] h-[1.5px] rounded-sm bg-[hsl(var(--assistant-accent))]"
        />
      )}
    </button>
  );
}

// Error Boundary for ChatMessage rendering failures
interface ErrorBoundaryProps {
  children: ReactNode;
  fallback?: ReactNode;
}

interface ErrorBoundaryState {
  hasError: boolean;
  error?: Error;
}

class MessageErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  constructor(props: ErrorBoundaryProps) {
    super(props);
    this.state = { hasError: false };
  }

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    console.error("[MessageErrorBoundary] Caught error:", error, errorInfo);
  }

  render() {
    if (this.state.hasError) {
      return this.props.fallback || (
        <div className="flex items-center gap-2 p-4 rounded-lg bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800">
          <AlertCircle className="h-5 w-5 text-red-500 shrink-0" />
          <div className="flex-1 min-w-0">
            <p className="text-sm font-medium text-red-800 dark:text-red-200">{i18n.t("assistant.messageRenderFailed")}</p>
            <p className="text-xs text-red-600 dark:text-red-400 truncate">
              {this.state.error?.message || "Unknown error"}
            </p>
          </div>
        </div>
      );
    }
    return this.props.children;
  }
}

export function AssistantPage() {
  const { t } = useTranslation();
  const { toast } = useToast();
  const userId = useAuthStore((state) => state.user?.user_id);
  const authHydrated = useAuthStore((state) => state.hydrated);

  // 1. Data Loading State
  const { models, config, status: modelCatalogStatus, refresh: refreshModelCatalog, isCurrent: ownsModelCatalog, isReady: canUseModelCatalog } = useAssistantModelCatalog();
  const modelsLoadError = modelCatalogStatus === "error";
  const canManageModels = useAuthStore(state => state.hasPermission("console:services:view"));
  const [datasets, setDatasets] = useState<DatasetInfo[]>([]);

  // 2. Settings State
  // Restore the last explicit choice for display; sending waits for the
  // current enabled/provider catalog to validate it.
  const [selectedModel, setSelectedModel] = useState<string>("");
  const [datasetsLoadError, setDatasetsLoadError] = useState(false);
  const [selectedDatasets, setSelectedDatasets] = useState<string[]>([]);
  const [temperature, setTemperature] = useState(0.7);
  const [webSearchEnabled, setWebSearchEnabled] = useState(false);
  const [thinkingLevel, setThinkingLevel] = useState<string>("auto");
  const [selectedStyle, setSelectedStyle] = useState(DEFAULT_STYLE_ID);
  
  // 3. UI State
  const [input, setInput] = useState("");
  const draftInputRef = useRef("");
  const imageViewEpochRef = useRef(0);
  const currentUserIdRef = useRef(userId);
  currentUserIdRef.current = userId;
  const draftsRef = useRef<Record<string, string>>({});
  const draftKeyRef = useRef(assistantDraftKey(undefined));
  const draftsReadyRef = useRef(false);
  const [showScrollButton, setShowScrollButton] = useState(false);
  const [showShareDialog, setShowShareDialog] = useState(false);
  const [showMemoryDialog, setShowMemoryDialog] = useState(false);
  const [showToolsDialog, setShowToolsDialog] = useState(false);
  const [showConnectors, setShowConnectors] = useState(false);
  const [showLocalOS, setShowLocalOS] = useState(false);
  const [subagentMessageId, setSubagentMessageId] = useState<string | null>(null);
  const [connectorCount, setConnectorCount] = useState(0);
  const [isMobile, setIsMobile] = useState(false);
  const shouldReduceMotion = useReducedMotion();
  const selectedModelInfo = useMemo(
    () => resolveAssistantModel(models, selectedModel),
    [models, selectedModel]
  );
  const reasoningOptions = useMemo(
    () => reasoningOptionsForModel(selectedModelInfo?.effective_capabilities),
    [selectedModelInfo?.effective_capabilities]
  );
  useEffect(() => {
    setThinkingLevel((current) =>
      resolveReasoningOptionId(selectedModelInfo?.effective_capabilities, current)
    );
  }, [selectedModelInfo?.capability_revision, selectedModelInfo?.effective_capabilities]);
  const scrollContainerRef = useRef<HTMLDivElement>(null);
  const scrollAfterSendRef = useRef(false);
  const wasCompactLayoutRef = useRef(false);
  const showLeftPanel = useAppStore((state) => state.assistantSidebarOpen);
  const setShowLeftPanel = useAppStore((state) => state.setAssistantSidebarOpen);

  // Right-panel mutex: "activity" | "artifacts" | null. Only one sheet
  // is visible at a time. Artifacts state continues to live in
  // useChatSession (it persists across sessions and is driven by SSE
  // events); Activity lives here.
  const [activityMessageId, setActivityMessageId] = useState<string | null>(null);
  
  // 4. Complex Logic Hooks
  const localOSState = useLocalOSControl();
  const {
    sessions,
    setSessions,
    activeSessionId,
    setActiveSessionId,
    activeSessionConfig,
    messages,
    setMessages,
    isStreaming,
    hasActiveRun,
    isRecoveringImage,
    modelRecreateNeeded,
    sourceRecreateNeeded,
    isComposerBlocked,
    sessionsLoading,
    historyRestoreState,
    historyRestoreError,
    handleNewChat,
    handleSelectSession,
    handleDeleteSession,
    sendMessage,
    stopStreaming,
    handleToolApproval,
    artifacts,
    artifactLoadError,
    refreshArtifacts,
    artifactRefreshStatus,
    canRefreshArtifacts,
    setArtifacts,
    showArtifacts,
    setShowArtifacts,
    workingMemory,
    showTaskPanel,
    codeExecution,
  } = useChatSession({
    isOSAgentEligible: localOSState.isSessionOptInEffectiveNow,
    getLocalNodeBinding: localOSState.getSessionBindingNow,
  });

  useLayoutEffect(() => {
    if (!authHydrated) return;
    const loaded = readAssistantDrafts(userId);
    const key = assistantDraftKey(activeSessionId);
    draftsRef.current = loaded;
    draftKeyRef.current = key;
    draftsReadyRef.current = true;
    draftInputRef.current = loaded[key] ?? "";
    setInput(draftInputRef.current);
  // The active session is handled by switchDraft below after hydration.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [authHydrated, userId]);

  const setDraftInput = useCallback((value: string) => {
    draftInputRef.current = value;
    setInput(value);
    if (!draftsReadyRef.current) return;
    draftsRef.current = { ...draftsRef.current, [draftKeyRef.current]: value };
    writeAssistantDrafts(userId, draftsRef.current);
  }, [userId]);

  const bindDraftToSession = useCallback((sessionId: string) => {
    if (!draftsReadyRef.current || draftKeyRef.current !== assistantDraftKey(undefined)) return;
    draftsRef.current = bindNewAssistantDraft(draftsRef.current, draftInputRef.current, sessionId);
    draftKeyRef.current = assistantDraftKey(sessionId);
    writeAssistantDrafts(userId, draftsRef.current);
  }, [userId]);

  const switchDraft = useCallback((targetSessionId: string | null | undefined) => {
    if (!draftsReadyRef.current) return;
    const target = assistantDraftKey(targetSessionId);
    if (draftKeyRef.current === target) return;
    const transition = transitionAssistantDraft(
      draftsRef.current,
      draftKeyRef.current,
      draftInputRef.current,
      target,
    );
    draftsRef.current = transition.drafts;
    draftKeyRef.current = target;
    draftInputRef.current = transition.input;
    setInput(transition.input);
    writeAssistantDrafts(userId, transition.drafts);
  }, [userId]);

  useEffect(() => {
    if (authHydrated) switchDraft(activeSessionId);
  }, [activeSessionId, authHydrated, switchDraft]);
  const {
    disableSessionOptIn,
    isSessionOptInEffectiveNow,
  } = localOSState;

  // Mutex: opening Activity forces Artifacts closed, and vice-versa.
  const openActivity = useCallback(
    (messageId: string) => {
      setActivityMessageId(messageId);
      setSubagentMessageId(null);
      setShowLocalOS(false);
      if (showArtifacts) setShowArtifacts(false);
    },
    [showArtifacts, setShowArtifacts],
  );

  const closeActivity = useCallback(() => {
    setActivityMessageId(null);
  }, []);

  const openSubagents = useCallback(
    (messageId: string) => {
      setSubagentMessageId(messageId);
      setActivityMessageId(null);
      setShowLocalOS(false);
      if (showArtifacts) setShowArtifacts(false);
    },
    [showArtifacts, setShowArtifacts],
  );

  const closeSubagents = useCallback(() => {
    setSubagentMessageId(null);
  }, []);

  // When something opens Artifacts (SSE delivery, user affordance),
  // close the other right-side panels.
  useEffect(() => {
    if (!showArtifacts || (!activityMessageId && !subagentMessageId && !showLocalOS)) return;
    const timer = window.setTimeout(() => {
      setActivityMessageId(null);
      setSubagentMessageId(null);
      setShowLocalOS(false);
    }, 0);
    return () => window.clearTimeout(timer);
  }, [showArtifacts, activityMessageId, subagentMessageId, showLocalOS]);

  // If the user switches sessions, drop any stale Activity selection.
  useEffect(() => {
    const timer = window.setTimeout(() => {
      setActivityMessageId(null);
      setSubagentMessageId(null);
    }, 0);
    return () => window.clearTimeout(timer);
  }, [activeSessionId]);

  // If the currently-open Activity message is no longer in the list
  // (session change / history replace), close the drawer.
  useEffect(() => {
    if (!activityMessageId) return;
    if (!messages.some((m) => m.id === activityMessageId)) {
      const timer = window.setTimeout(() => setActivityMessageId(null), 0);
      return () => window.clearTimeout(timer);
    }
  }, [messages, activityMessageId]);

  useEffect(() => {
    if (!subagentMessageId) return;
    if (!messages.some((message) => message.id === subagentMessageId)) {
      const timer = window.setTimeout(() => setSubagentMessageId(null), 0);
      return () => window.clearTimeout(timer);
    }
  }, [messages, subagentMessageId]);

  const rightPanel: RightPanel = showLocalOS
    ? "local_os"
    : subagentMessageId
      ? "subagents"
      : activityMessageId
        ? "activity"
        : showArtifacts
          ? "artifacts"
          : null;
  const mobilePanelWidth =
    isMobile && typeof window !== "undefined"
      ? Math.min(window.innerWidth, 430)
      : 380;

  const activeActivityMessage = useMemo(
    () => (activityMessageId ? messages.find((m) => m.id === activityMessageId) ?? null : null),
    [messages, activityMessageId],
  );

  const activeSubagentMessage = useMemo(
    () => (subagentMessageId
      ? messages.find((message) => message.id === subagentMessageId) ?? null
      : null),
    [messages, subagentMessageId],
  );

  const latestSubagentMessageId = useMemo(() => {
    for (let index = messages.length - 1; index >= 0; index -= 1) {
      const message = messages[index];
      if (message.role === "assistant" && (message.activeSubAgents?.length ?? 0) > 0) {
        return message.id;
      }
    }
    return null;
  }, [messages]);

  const latestSubagentCount = useMemo(() => {
    if (!latestSubagentMessageId) return 0;
    return messages.find((message) => message.id === latestSubagentMessageId)?.activeSubAgents?.length ?? 0;
  }, [latestSubagentMessageId, messages]);

  // Most recent assistant message — the default target for the top-bar
  // Activity chip when the drawer is closed. We don't need step content
  // here, only whether a non-empty timeline exists so the chip can gate
  // its disabled state.
  const latestActivityMessageId = useMemo(() => {
    for (let i = messages.length - 1; i >= 0; i--) {
      const m = messages[i];
      if (m.role !== "assistant") continue;
      if (m.isStreaming) return m.id;
      const processSummary = m.processSummary;
      const hasProcessSignal =
        Boolean(processSummary) &&
        ((processSummary?.steps.length ?? 0) > 0 ||
          (processSummary?.tools.length ?? 0) > 0 ||
          Boolean(processSummary?.contextBudget) ||
          Boolean(processSummary?.contextCompacted) ||
          typeof processSummary?.thinkingDurationMs === "number");
      const hasSignal =
        (m.toolCalls && m.toolCalls.length > 0) ||
        (m.searchStatus && m.searchStatus.length > 0) ||
        (m.thinkingContent && m.thinkingContent.length > 0) ||
        hasProcessSignal ||
        (m.contexts && m.contexts.length > 0) ||
        (m.generatedArtifacts && m.generatedArtifacts.length > 0);
      if (hasSignal) return m.id;
    }
    return null;
  }, [messages]);

  const latestActivitySteps = useMemo(() => {
    if (!latestActivityMessageId) return 0;
    const message = messages.find((candidate) => candidate.id === latestActivityMessageId);
    if (!message) return 0;
    return (
      (message.processSummary?.steps.length || 0) +
      (message.processSummary?.tools.length || 0) +
      (message.activeSubAgents?.length || 0)
    );
  }, [latestActivityMessageId, messages]);
  const uniqueArtifactCount = useMemo(
    () => countUniqueArtifactAffordances(artifacts, codeExecution.outputFiles),
    [artifacts, codeExecution.outputFiles]
  );

  const rightPanelState: RightPanelState = useMemo(
    () => ({
      rightPanel,
      activityMessageId,
      subagentMessageId,
      openActivity,
      closeActivity,
      openSubagents,
      closeSubagents,
    }),
    [
      rightPanel,
      activityMessageId,
      subagentMessageId,
      openActivity,
      closeActivity,
      openSubagents,
      closeSubagents,
    ],
  );

  const {
    files,
    isUploading,
    selectionNotice,
    fileInputRef,
    handleFileSelect,
    handlePaste,
    removeFile,
    retryFile,
    toggleFileSelection,
    consumeSelectedFiles,
    rekeyFiles,
  } = useFileHandler(assistantDraftKey(activeSessionId));

  const {
    isImageMode,
    isGeneratingImage: isImageRequestPending,
    handleImageGenerate,
    cancelImageMode,
    sendImageGeneration
  } = useImageGeneration(
    activeSessionId ?? null,
    selectedModelInfo?.id || "",
    setMessages,
    setArtifacts,
    setActiveSessionId,
    createSession,
    listSessions,
    setSessions,
    { selected_style: selectedStyle, web_search_enabled: webSearchEnabled, selected_provider: selectedModelInfo?.provider }
  );
  const isGeneratingImage = isImageRequestPending || isRecoveringImage;

  // Resolve browser state only after auth hydration identifies its owner.
  // Layout timing prevents an interactive frame from retaining the previous
  // account's selection while a new account is being applied.
  useLayoutEffect(() => {
    setSelectedModel(readHydratedLastModelId(authHydrated, userId));
  }, [authHydrated, userId]);

  // Load initial data
  useEffect(() => {
    if (!authHydrated) return;
    let cancelled = false;

    async function loadData() {
      try {
        const [datasetsResult, connectionsData] = await Promise.all([
          listDatasets().then((data) => ({ ok: true, data })).catch(() => ({ ok: false, data: [] as DatasetInfo[] })),
          apiClient.get<Array<{ connected?: boolean }>>("/api/v1/connectors/available").catch(() => ({ data: [] })),
        ]);
        if (cancelled) return;
        const datasetsData = datasetsResult.data;
        setDatasetsLoadError(!datasetsResult.ok);
        setDatasets(datasetsData);
        setConnectorCount(connectionsData.data.filter((c: { connected?: boolean }) => c.connected).length);

      } catch (error) {
        console.error("Failed to load assistant data:", error);
        if (!cancelled) setDatasetsLoadError(true);
      }
    }
    loadData();
    return () => {
      cancelled = true;
    };
  }, [authHydrated, userId]);

  // Catalog refresh never replaces an explicit or session-restored selection.
  useEffect(() => {
    if (modelCatalogStatus !== "ready" || !ownsModelCatalog()) return;
    setSelectedModel(current => current || preferredAssistantModelId(models, config?.default_model_id || "", readLastModelId(userId)));
  }, [modelCatalogStatus, ownsModelCatalog, models, config?.default_model_id, userId]);

  // A saved conversation keeps its own model when restored on page load.
  useEffect(() => {
    const sessionModelId = savedAssistantModelSelection({ selected_model: activeSessionConfig?.selected_model, selected_provider: activeSessionConfig?.selected_provider });
    if (activeSessionId && sessionModelId) {
      setSelectedModel(sessionModelId);
    }
  }, [activeSessionId, activeSessionConfig?.selected_model, activeSessionConfig?.selected_provider]);

  useEffect(() => {
    const mediaQuery = window.matchMedia(ASSISTANT_COMPACT_MEDIA_QUERY);
    const sync = () => setIsMobile(mediaQuery.matches);
    sync();
    mediaQuery.addEventListener("change", sync);
    return () => mediaQuery.removeEventListener("change", sync);
  }, []);

  useEffect(() => {
    if (isMobile && !wasCompactLayoutRef.current && showLeftPanel) {
      setShowLeftPanel(false);
    }
    wasCompactLayoutRef.current = isMobile;
  }, [isMobile, setShowLeftPanel, showLeftPanel]);

  useEffect(() => {
    if (!isMobile || !showLeftPanel) return;
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setShowLeftPanel(false);
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [isMobile, setShowLeftPanel, showLeftPanel]);

  useEffect(() => {
    if (!isMobile || !rightPanel || !showLeftPanel) return;
    setShowLeftPanel(false);
  }, [isMobile, rightPanel, setShowLeftPanel, showLeftPanel]);

  useEffect(() => {
    if (sessionsLoading || messages.length > 0) return;
    if (historyRestoreState === "loading" || historyRestoreState === "failed") return;

    if (!showLeftPanel && sessions.length > 0) {
      trackChatHistoryEmptyState("assistant", {
        state: "history_hidden",
        sessionCount: sessions.length,
        activeSessionId,
      });
      return;
    }

    if (activeSessionId) {
      trackChatHistoryEmptyState("assistant", {
        state: "selected_session_empty",
        sessionCount: sessions.length,
        activeSessionId,
      });
      return;
    }

    trackChatHistoryEmptyState("assistant", {
      state: "no_sessions",
      sessionCount: sessions.length,
      activeSessionId: null,
    });
  }, [activeSessionId, historyRestoreState, messages.length, sessions.length, sessionsLoading, showLeftPanel]);

  // Sync settings when session changes
  const onSessionSelect = useCallback(async (sessionId: string) => {
    if (sessionId !== activeSessionId) imageViewEpochRef.current += 1;
    scrollAfterSendRef.current = false;
    cancelImageMode(); // Reset image mode when switching sessions
    disableSessionOptIn();
    if (isMobile) setShowLeftPanel(false);
    const sessionConfig = await handleSelectSession(sessionId);
    if (sessionConfig) {
      const sessionSelection = savedAssistantModelSelection(sessionConfig);
      const sessionModel = resolveAssistantModel(models, sessionSelection);
      if (sessionSelection) {
        setSelectedModel(sessionSelection);
      }
      setSelectedDatasets(sessionConfig.selected_datasets || []);  // Always reset, even if empty
      if (typeof sessionConfig.web_search_enabled === "boolean") setWebSearchEnabled(sessionConfig.web_search_enabled);
      setThinkingLevel(
        resolveReasoningOptionId(
          sessionModel?.effective_capabilities,
          sessionConfig.reasoning_option || sessionConfig.thinking_level || "auto"
        )
      );
      if (typeof sessionConfig.temperature === "number") setTemperature(sessionConfig.temperature);
      if (sessionConfig.selected_style) setSelectedStyle(sessionConfig.selected_style);
    }
  }, [handleSelectSession, models, cancelImageMode, isMobile, setShowLeftPanel, disableSessionOptIn, activeSessionId]);

  const onDeleteSession = useCallback(async (sessionId: string) => {
    if (sessionId === activeSessionId) imageViewEpochRef.current += 1;
    return handleDeleteSession(sessionId);
  }, [activeSessionId, handleDeleteSession]);

  // Handle new chat - reset all state including feature toggles
  const onNewChat = useCallback(() => {
    imageViewEpochRef.current += 1;
    scrollAfterSendRef.current = false;
    switchDraft(undefined);
    cancelImageMode(); // Reset image mode
    disableSessionOptIn();
    handleNewChat();
    if (isMobile) setShowLeftPanel(false);
    // Reset feature toggles to defaults
    setSelectedDatasets([]);  // Clear selected knowledge bases
    setWebSearchEnabled(false);  // Disable web search
    setThinkingLevel("auto");
    setSelectedModel(preferredAssistantModelId(
      models,
      config?.default_model_id || "",
      readLastModelId(userId),
    ));
    // Keep temperature as a user preference.
  }, [handleNewChat, cancelImageMode, isMobile, setShowLeftPanel, disableSessionOptIn, models, config?.default_model_id, userId, switchDraft]);

  const savedModelUnavailable = Boolean(
    modelCatalogStatus === "ready" && activeSessionId && activeSessionConfig?.selected_model &&
    !resolveAssistantModel(models, savedAssistantModelSelection(activeSessionConfig)),
  );
  const onNewChatKeepingDraft = useCallback(() => {
    const retainedInput = input;
    const chosenModel = resolveAssistantModel(models, selectedModel)
      ? selectedModel
      : preferredAssistantModelId(models, config?.default_model_id || "", readLastModelId(userId));
    if (activeSessionId) rekeyFiles(assistantDraftKey(activeSessionId), assistantDraftKey(undefined));
    onNewChat();
    setDraftInput(retainedInput);
    setSelectedModel(chosenModel);
  }, [input, selectedModel, models, config?.default_model_id, userId, activeSessionId, rekeyFiles, onNewChat, setDraftInput]);

  useChatShortcuts({
    surface: "assistant",
    composerId: ASSISTANT_COMPOSER_ID,
    onNewChat,
    onStop: hasActiveRun ? stopStreaming : undefined,
  });

  // Handle Send
  const handleSend = useCallback(() => {
    // A cached choice is provisional until the current enabled/provider
    // catalog confirms it. Never start a turn with a stale model id.
    if (!canUseModelCatalog()) return;
    if (!selectedModelInfo) {
      toast({
        title: t("assistant.noModels", "No models available"),
        variant: "destructive",
      });
      return;
    }

    const successfulUploads = files.filter((f) => f.status === "success" && f.selected && f.response);
    if (!selectedModelInfo?.supports_vision && successfulUploads.some((f) => f.file.type.startsWith("image/"))) {
      toast({ title: t("assistant.imageModelRequired", "The selected model cannot read images. Choose a vision model or exclude the image before sending."), variant: "destructive" });
      return;
    }
    const unavailableDatasets = selectedDatasets.filter((id) => !datasets.some((dataset) => dataset.dataset_id === id));
    if (selectedDatasets.length > 0 && (datasetsLoadError || unavailableDatasets.length > 0)) {
      toast({ title: t("assistant.datasetScopeUnavailable", "Selected knowledge sources are unavailable. Review this round's sources before sending."), variant: "destructive" });
      return;
    }
    if (webSearchEnabled && !config?.web_search_enabled) {
      toast({ title: t("assistant.webScopeUnavailable", "Web search is unavailable for this round. Turn it off or check configuration."), variant: "destructive" });
      return;
    }
    if (
      isComposerBlocked ||
      isUploading ||
      isGeneratingImage ||
      (!input.trim() && successfulUploads.length === 0)
    ) {
      return;
    }
    const filePaths = successfulUploads.map((f) => f.response!.file_path);

    let messageContent = input.trim();
    if (successfulUploads.length > 0 && !messageContent) {
      messageContent = t("assistant.analyzeFiles", "Please analyze these uploaded files.");
    }

    const attachments = successfulUploads.map((f) => ({
      id: f.response!.file_id,
      type: (f.file.type.startsWith("image/") ? "image" : "file"),
      url: f.response!.file_path,
      filename: f.file.name,
    }));

    // Sending a new message makes any open Activity drawer stale (it's
    // pinned to a prior message's id). Close it; the user reopens it for
    // the new message by clicking its Activity pill. Artifacts are
    // session-scoped, so leave them alone.
    setActivityMessageId(null);
    scrollAfterSendRef.current = true;

    sendMessage({
      messageContent,
      filePaths,
      attachments,
      config: {
        selected_model: selectedModelInfo.id,
        selected_provider: selectedModelInfo.provider,
        selected_datasets: selectedDatasets,
        web_search_enabled: webSearchEnabled,
        reasoning_option: thinkingLevel,
        temperature,
        selected_style: selectedStyle,
        execution_profile: "safe",
        memory_mode: "auto",
        os_agent_enabled: isSessionOptInEffectiveNow(),
      },
      selectedDatasets,
      models,
      datasets,
      onRunStarted: (acceptedSessionId) => {
        draftsRef.current = clearAdmittedAssistantDrafts(draftsRef.current, activeSessionId, acceptedSessionId);
        writeAssistantDrafts(userId, draftsRef.current);
        setDraftInput("");
        consumeSelectedFiles();
        if (!activeSessionId) rekeyFiles(assistantDraftKey(undefined), assistantDraftKey(acceptedSessionId));
      },
      onSessionBound: bindDraftToSession,
    });
  }, [input, files, selectedModelInfo, selectedDatasets, webSearchEnabled, thinkingLevel, temperature, selectedStyle, models, canUseModelCatalog, datasets, datasetsLoadError, config?.web_search_enabled, isComposerBlocked, isUploading, isGeneratingImage, sendMessage, consumeSelectedFiles, rekeyFiles, activeSessionId, userId, t, toast, isSessionOptInEffectiveNow, setDraftInput, bindDraftToSession]);

  // Handle Image Send
  const handleImageSend = useCallback(() => {
     if (!canUseModelCatalog() || !selectedModelInfo) return;
     // Same rationale as handleSend: close stale Activity drawer before a new send.
     setActivityMessageId(null);
     scrollAfterSendRef.current = true;
     const viewEpoch = imageViewEpochRef.current;
     const originUserId = userId;
     const isOriginCurrent = () =>
       imageViewEpochRef.current === viewEpoch && currentUserIdRef.current === originUserId;
     void sendImageGeneration(input, selectedStyle, bindDraftToSession, isOriginCurrent).then((accepted) => {
       if (accepted && isOriginCurrent()) setDraftInput("");
     });
  }, [input, selectedStyle, sendImageGeneration, setDraftInput, bindDraftToSession, userId, canUseModelCatalog, selectedModelInfo]);

  // Auto-scroll
  const scrollToBottomDom = useCallback((behavior: ScrollBehavior = "smooth") => {
    scrollContainerRef.current?.scrollTo({
      top: scrollContainerRef.current.scrollHeight,
      behavior,
    });
  }, []);

  const handleScroll = useCallback(() => {
    const container = scrollContainerRef.current;
    if (!container) return;

    const distanceFromBottom =
      container.scrollHeight - container.scrollTop - container.clientHeight;
    setShowScrollButton(distanceFromBottom >= 150);
  }, []);

  const scrollToBottom = useCallback(() => {
    scrollToBottomDom();
    setShowScrollButton(false);
  }, [scrollToBottomDom]);

  useEffect(() => {
    const container = scrollContainerRef.current;
    if (!container) return;

    const isNearBottom =
      container.scrollHeight - container.scrollTop - container.clientHeight < 150;

    if (isNearBottom || scrollAfterSendRef.current) {
      scrollToBottomDom();
      scrollAfterSendRef.current = false;
    }
  }, [messages, scrollToBottomDom]);

  return (
    <>
    <RightPanelContext.Provider value={rightPanelState}>
    <TooltipProvider>
      <div
        className={cn(
          "relative flex w-full flex-col",
          ASSISTANT_UI_V2 ? "assistant-v2 font-assistant" : "bg-slate-50 dark:bg-slate-900"
        )}
        style={{
          height: isMobile
            ? "calc(100dvh - 72px)"
            : "calc(100dvh - 86px)",
        }}
      >
        <div className="relative flex flex-1 overflow-hidden">
          
          {/* Left Sidebar — matches --assistant-canvas-bg so the sidebar
              and chat area feel like the same plane in both themes. */}
          <AnimatePresence initial={false}>
            {showLeftPanel && isMobile && (
              <motion.button
                key="assistant-history-backdrop"
                type="button"
                aria-label={t("assistant.hideHistory", "Hide history")}
                initial={shouldReduceMotion ? false : { opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                transition={{ duration: shouldReduceMotion ? 0 : 0.18 }}
                className="absolute inset-0 z-20 bg-black/35"
                onClick={() => setShowLeftPanel(false)}
              />
            )}
            {showLeftPanel && (
              <motion.aside
                key="assistant-history-sheet"
                role={isMobile ? "dialog" : undefined}
                aria-modal={isMobile || undefined}
                aria-label={t("assistant.showHistory", "Conversation history")}
                initial={
                  shouldReduceMotion
                    ? false
                    : isMobile
                      ? { x: "-100%", opacity: 0 }
                      : { width: 0, opacity: 0 }
                }
                animate={
                  isMobile
                    ? { x: 0, opacity: 1 }
                    : { width: 280, opacity: 1 }
                }
                exit={
                  shouldReduceMotion
                    ? { opacity: 0 }
                    : isMobile
                      ? { x: "-100%", opacity: 0 }
                      : { width: 0, opacity: 0 }
                }
                transition={{ duration: shouldReduceMotion ? 0 : 0.2, ease: "easeOut" }}
                className={cn(
                  "shrink-0 overflow-hidden border-r border-[hsl(var(--assistant-border))] bg-[hsl(var(--assistant-canvas-bg))]",
                  isMobile
                    ? "absolute inset-y-0 left-0 z-30 w-[min(88vw,390px)] shadow-xl"
                    : "relative w-[280px]"
                )}
              >
                <div className={cn("h-full", isMobile ? "w-full" : "w-[280px]")}>
                  <ConversationSidebar
                    sessions={sessions}
                    activeSessionId={activeSessionId ?? null}
                    isLoading={sessionsLoading}
                    onNewChat={onNewChat}
                    onSelectSession={onSessionSelect}
                    onDeleteSession={onDeleteSession}
                    onSessionsChange={setSessions}
                  />
                </div>
              </motion.aside>
            )}
          </AnimatePresence>

          {/* Main Content */}
          <div className={cn("flex-1 flex flex-col min-w-0 relative", ASSISTANT_UI_V2 ? "assistant-v2" : "bg-slate-50 dark:bg-slate-900")}>
            {/* Header */}
            <div className="flex shrink-0 items-center gap-1.5 px-3 py-2.5 sm:gap-2 sm:px-4 sm:py-3">
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-8 w-8 rounded-md hover:bg-[hsl(var(--assistant-surface-soft))] transition-colors duration-150"
                    onClick={() => setShowLeftPanel(!showLeftPanel)}
                    aria-label={
                      showLeftPanel
                        ? t("assistant.hideHistory", "Hide history")
                        : t("assistant.showHistory", "Show history")
                    }
                  >
                    {showLeftPanel ? <PanelLeftClose className="h-4 w-4 text-[hsl(var(--assistant-text-secondary))]" /> : <PanelLeft className="h-4 w-4 text-[hsl(var(--assistant-text-secondary))]" />}
                  </Button>
                </TooltipTrigger>
                <TooltipContent side="bottom">{showLeftPanel ? t("assistant.hideHistory", "Hide history") : t("assistant.showHistory", "Show history")}</TooltipContent>
              </Tooltip>
              {activeSessionId && (
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button variant="ghost" size="icon" className="h-8 w-8 shrink-0 rounded-md" onClick={() => setShowToolsDialog(true)} aria-label={t("assistant.toolsTitle", "Assistant tools")}>
                      <Wrench className="h-3.5 w-3.5 text-[hsl(var(--assistant-text-secondary))]" />
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent side="bottom">{t("assistant.toolsTitle", "Assistant tools")}</TooltipContent>
                </Tooltip>
              )}
              <CompactModelSelector
                models={models}
                selectedModel={selectedModel}
                onSelect={(modelId) => {
                  setSelectedModel(modelId);
                  writeLastModelId(modelId, userId);
                }}
                disabled={hasActiveRun || modelCatalogStatus !== "ready"}
              />
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-8 w-8 shrink-0 rounded-md"
                    onClick={() => setShowMemoryDialog(true)}
                    aria-label={t("assistant.memoryTitle", "Assistant memory")}
                  >
                    <Brain className="h-3.5 w-3.5 text-[hsl(var(--assistant-text-secondary))]" />
                  </Button>
                </TooltipTrigger>
                <TooltipContent side="bottom">{t("assistant.memoryTitle", "Assistant memory")}</TooltipContent>
              </Tooltip>
              {/* Share button */}
              {activeSessionId && messages.length > 0 && !hasActiveRun && (
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button
                      variant="ghost"
                      size="icon"
                      className="h-8 w-8 rounded-md hover:bg-[hsl(var(--assistant-surface-soft))] transition-colors duration-150"
                      onClick={() => setShowShareDialog(true)}
                      aria-label={t("assistant.share", "Share")}
                    >
                      <Share2 className="h-3.5 w-3.5 text-[hsl(var(--assistant-text-secondary))]" />
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent side="bottom">{t("assistant.shareConversation", "Share Conversation")}</TooltipContent>
                </Tooltip>
              )}
              {/* Spacer pushes right-side chips to the far right of the top bar */}
              <div className="flex-1" />
              <RightPanelChip
                icon={<Network className="h-3.5 w-3.5" />}
                label={t("assistant.subagents", "Agents")}
                count={latestSubagentCount}
                active={rightPanel === "subagents"}
                disabled={latestSubagentMessageId == null}
                compact={isMobile}
                onClick={() => {
                  if (!latestSubagentMessageId) return;
                  if (rightPanel === "subagents") {
                    closeSubagents();
                  } else {
                    openSubagents(latestSubagentMessageId);
                  }
                }}
              />
              <RightPanelChip
                icon={<MonitorCog className="h-3.5 w-3.5" />}
                label={
                  localOSState.loadState === "loading"
                    ? t("assistant.localFilesLoading", "Local files…")
                    : localOSState.loadState === "online"
                      ? t("assistant.localFiles", "Local files")
                      : t("assistant.localFilesOffline", "Local files offline")
                }
                count={localOSState.onlineDeviceCount}
                active={rightPanel === "local_os"}
                compact={isMobile}
                onClick={() => {
                  if (rightPanel === "local_os") {
                    setShowLocalOS(false);
                  } else {
                    setActivityMessageId(null);
                    setSubagentMessageId(null);
                    setShowArtifacts(false);
                    setShowLocalOS(true);
                  }
                }}
              />
              {isMobile && (uniqueArtifactCount > 0 || artifactLoadError) && (
                <RightPanelChip
                  icon={<FileText className="h-3.5 w-3.5" />}
                  label={t("assistant.artifacts", "Artifacts")}
                  count={uniqueArtifactCount}
                  active={rightPanel === "artifacts"}
                  compact
                  onClick={() => {
                    if (rightPanel === "artifacts") {
                      setShowArtifacts(false);
                    } else {
                      setActivityMessageId(null);
                      setSubagentMessageId(null);
                      setShowLocalOS(false);
                      setShowArtifacts(true);
                    }
                  }}
                />
              )}
              {/* Activity and Artifacts share the same mutex as Local OS
                  (rightPanel = "local_os" | "activity" | "artifacts" | null).
                  Each chip toggles its own panel; opening one auto-closes the
                  other via useEffect. A chip shows a subtle gold underline
                  stripe when its panel is open.

                  We intentionally do NOT restore a floating Artifacts popup.
                  Multi-file, multi-view artifact content scales poorly as a
                  modal; the right-side drawer keeps parity with Activity and
                  avoids covering the chat. */}
              {!isMobile && (
                <div className="flex items-center gap-0.5">
                  <RightPanelChip
                    icon={<Sparkles className="h-3.5 w-3.5" />}
                    label={t("playground.activity.title", "Activity")}
                    count={latestActivitySteps}
                    active={rightPanel === "activity"}
                    disabled={latestActivityMessageId == null}
                    onClick={() => {
                      if (!latestActivityMessageId) return;
                      if (rightPanel === "activity") {
                        closeActivity();
                      } else {
                        openActivity(latestActivityMessageId);
                      }
                    }}
                  />
                  {(uniqueArtifactCount > 0 || artifactLoadError) && (
                    <RightPanelChip
                      icon={<FileText className="h-3.5 w-3.5" />}
                      label={t("assistant.artifacts", "Artifacts")}
                      count={uniqueArtifactCount}
                      active={rightPanel === "artifacts"}
                      onClick={() => {
                        if (rightPanel === "artifacts") {
                          setShowArtifacts(false);
                        } else {
                          setActivityMessageId(null);
                          setSubagentMessageId(null);
                          setShowLocalOS(false);
                          setShowArtifacts(true);
                        }
                      }}
                    />
                  )}
                </div>
              )}
            </div>

            {/* Messages Area */}
            <div
              ref={scrollContainerRef}
              className="flex-1 overflow-y-auto"
              onScroll={handleScroll}
            >
            <div className={cn("mx-auto px-3 py-5 sm:px-6 sm:py-8", ASSISTANT_UI_V2 ? "max-w-[760px] w-full" : "max-w-3xl")}>
                <AssistantModelStatus state={modelCatalogStatus === "loading" ? "loading" : modelCatalogStatus === "error" ? "error" : models.length === 0 ? "empty" : !selectedModel ? "unselected" : !selectedModelInfo ? "unavailable" : "ready"}
                  canManageModels={canManageModels} onRefresh={refreshModelCatalog} />
                {(savedModelUnavailable || modelRecreateNeeded || sourceRecreateNeeded) && (
                  <div role="alert" className="mb-4 rounded-lg border border-amber-300 bg-amber-50 p-3 text-sm text-amber-950 dark:border-amber-500/40 dark:bg-amber-500/10 dark:text-amber-100">
                    <p>
                      {sourceRecreateNeeded
                          ? t("assistant.sourceNeedsNewConversation", "Earlier knowledge sources are unavailable. Start a new conversation to continue.")
                        : savedModelUnavailable
                          ? t("assistant.savedModelUnavailable", "This conversation''s saved model ({model}) is unavailable or you no longer have access. Its existing history is unchanged.", { model: activeSessionConfig?.selected_model })
                          : t("assistant.modelNeedsNewConversation", "This model uses a different tool configuration. Start a new conversation to continue.")}
                    </p>
                    {!modelsLoadError && models.length > 0 && (
                      <Button type="button" size="sm" variant="outline" className="mt-2" onClick={onNewChatKeepingDraft}>
                        {t("assistant.newConversationKeepDraft", "New conversation with draft")}
                      </Button>
                    )}
                  </div>
                )}
                {messages.length === 0 ? (
                  <div className="space-y-5">
                    {!isMobile && !showLeftPanel && sessions.length > 0 && (
                      <div className="rounded-2xl border border-amber-200/70 bg-amber-50/80 px-4 py-3 text-sm text-amber-900 dark:border-amber-500/30 dark:bg-amber-500/10 dark:text-amber-100">
                        <div className="flex items-start justify-between gap-3">
                          <div>
                            <div className="font-medium">
                              {t("assistant.historyHiddenTitle", "History is hidden")}
                            </div>
                            <div className="mt-1 text-amber-800/80 dark:text-amber-100/80">
                              {t("assistant.historyHiddenDescription", "Your previous chats are still available in the left sidebar.")}
                            </div>
                          </div>
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() => setShowLeftPanel(true)}
                            className="shrink-0 border-amber-300/80 bg-white/80 text-amber-900 hover:bg-white dark:border-amber-400/30 dark:bg-amber-500/10 dark:text-amber-100"
                          >
                            {t("assistant.showHistory", "Show history")}
                          </Button>
                        </div>
                      </div>
                    )}
                    {activeSessionId && !sessionsLoading && (
                      historyRestoreState === "loading" ? (
                        <div className="rounded-2xl border border-slate-200/80 bg-white/70 px-4 py-3 text-sm text-slate-700 shadow-xs dark:border-slate-700/60 dark:bg-slate-900/40 dark:text-slate-200">
                          <div className="font-medium">
                            {t("assistant.restoringSessionTitle", "Restoring selected conversation")}
                          </div>
                          <div className="mt-1 text-slate-500 dark:text-slate-400">
                            {t("assistant.restoringSessionDescription", "We are loading the latest messages and files for this conversation.")}
                          </div>
                        </div>
                      ) : historyRestoreState === "failed" ? (
                        <div className="rounded-2xl border border-red-200/80 bg-red-50/80 px-4 py-3 text-sm text-red-900 shadow-xs dark:border-red-500/30 dark:bg-red-500/10 dark:text-red-100">
                          <div className="font-medium">
                            {t("assistant.restoreFailedTitle", "Couldn't restore the selected conversation")}
                          </div>
                          <div className="mt-1 text-red-800/80 dark:text-red-100/80">
                            {t("assistant.restoreFailedDescription", "The conversation still exists, but the last restore attempt did not finish. You can retry or start a new chat.")}
                          </div>
                          {historyRestoreError && (
                            <div className="mt-2 truncate text-xs text-red-700/80 dark:text-red-200/80">
                              {historyRestoreError}
                            </div>
                          )}
                          <div className="mt-3 flex gap-2">
                            <Button
                              variant="outline"
                              size="sm"
                              onClick={() => {
                                if (activeSessionId) {
                                  void onSessionSelect(activeSessionId);
                                }
                              }}
                              className="border-red-300/80 bg-white/90 text-red-900 hover:bg-white dark:border-red-400/30 dark:bg-red-500/10 dark:text-red-100"
                            >
                              {t("common.retry", "Retry")}
                            </Button>
                            <Button
                              variant="ghost"
                              size="sm"
                              onClick={onNewChat}
                              className="text-red-900 hover:bg-red-100 dark:text-red-100 dark:hover:bg-red-500/10"
                            >
                              {t("assistant.newChat", "New chat")}
                            </Button>
                          </div>
                        </div>
                      ) : (
                        <div className="rounded-2xl border border-slate-200/80 bg-white/70 px-4 py-3 text-sm text-slate-700 shadow-xs dark:border-slate-700/60 dark:bg-slate-900/40 dark:text-slate-200">
                          <div className="font-medium">
                            {t("assistant.selectedSessionEmptyTitle", "Selected conversation has no restored messages")}
                          </div>
                          <div className="mt-1 text-slate-500 dark:text-slate-400">
                            {t("assistant.selectedSessionEmptyDescription", "This can happen when the conversation is empty or the last restore failed.")}
                          </div>
                        </div>
                      )
                    )}
                    <WelcomeScreen />
                  </div>
                ) : (
                  <div
                    className={ASSISTANT_UI_V2 ? "space-y-8" : "space-y-6"}
                    role="log"
                    aria-live="polite"
                    aria-relevant="additions text"
                    aria-label={t("assistant.chatLog", "Assistant conversation log")}
                  >
                    {/* Manus-style task timeline for agentic workflows */}
                    {!ASSISTANT_UI_V2 && showTaskPanel && workingMemory && workingMemory.tasks?.length > 0 && (
                      <AgentTaskTimeline
                        goal={workingMemory.goal}
                        tasks={workingMemory.tasks.map((task): AgentTask => {
                          const timelineStatus: AgentTask["status"] =
                            task.status === "blocked" || task.status === "failed"
                              ? "failed"
                              : task.status === "skipped"
                                ? "completed"
                                : task.status;

                          return {
                            id: task.id,
                            title: task.description,
                            status: timelineStatus,
                            result: task.result,
                            error: task.error,
                          };
                        })}
                        isThinking={
                          isStreaming &&
                          workingMemory.tasks.some((task) => task.status === "in_progress")
                        }
                        thinkingMessage={t("assistant.taskRunning")}
                        className="mb-4"
                      />
                    )}
                    <Suspense fallback={null}>
                      {messages.map((message) => (
                        <MessageErrorBoundary key={message.id}>
                          <ChatMessage message={message} sessionId={activeSessionId} onToolApproval={handleToolApproval} />
                        </MessageErrorBoundary>
                      ))}
                    </Suspense>
                  </div>
                )}
              </div>
            </div>

            {/* Scroll Button */}
            <AnimatePresence>
              {showScrollButton && (
                <motion.div initial={shouldReduceMotion ? false : { opacity: 0, y: 10 }} animate={{ opacity: 1, y: 0 }} exit={shouldReduceMotion ? { opacity: 0 } : { opacity: 0, y: 10 }} className="absolute bottom-[180px] left-1/2 z-10 -translate-x-1/2">
                  <Button size="icon" variant="outline" onClick={scrollToBottom} className="h-9 w-9 rounded-full shadow-sm" aria-label={t("playground.scrollToBottom", "Scroll to bottom")}>
                    <ArrowDown className="h-4 w-4 text-slate-500" />
                  </Button>
                </motion.div>
              )}
            </AnimatePresence>

            {/* Input Area */}
            <ChatInputArea
              composerId={ASSISTANT_COMPOSER_ID}
              input={input}
              setInput={setDraftInput}
              files={files}
              isUploading={isUploading}
              selectionNotice={selectionNotice}
              isStreaming={hasActiveRun}
              isComposerBlocked={isComposerBlocked}
              isGeneratingImage={isGeneratingImage}
              isImageMode={isImageMode}
              hasAvailableModel={modelCatalogStatus === "ready" && Boolean(selectedModelInfo)}
              supportsVision={selectedModelInfo?.supports_vision === true}
              handleFileSelect={handleFileSelect}
              removeFile={removeFile}
              retryFile={retryFile}
              toggleFileSelection={toggleFileSelection}
              onSend={isImageMode ? handleImageSend : handleSend}
              onStop={stopStreaming}
              onCancelImageMode={cancelImageMode}
              handlePaste={handlePaste}
              fileInputRef={fileInputRef}
              config={config}
              datasets={datasets}
              datasetsLoadError={datasetsLoadError}
              selectedDatasets={selectedDatasets}
              onToggleDataset={(id) => setSelectedDatasets(prev => prev.includes(id) ? prev.filter(x => x !== id) : [...prev, id])}
              webSearchEnabled={webSearchEnabled}
              setWebSearchEnabled={setWebSearchEnabled}
              thinkingLevel={thinkingLevel}
              setThinkingLevel={setThinkingLevel}
              reasoningOptions={reasoningOptions}
              handleImageGenerate={handleImageGenerate}
              selectedStyle={selectedStyle}
              setSelectedStyle={setSelectedStyle}
              onOpenConnectors={() => setShowConnectors(true)}
              connectorCount={connectorCount}
            />
          </div>

          {/* Right-side sheet: Activity OR Artifacts. Mutex enforced via
              rightPanelState; never both. ActivityPanel is mounted first
              so it takes priority when the user explicitly opens it. */}
          {!isMobile && rightPanel === "activity" && activeActivityMessage && (
            <Suspense fallback={<LazyPanelFallback />}>
              <ActivityPanel
                open
                onClose={closeActivity}
                message={activeActivityMessage}
                width={380}
                onToolApproval={handleToolApproval}
              />
            </Suspense>
          )}

          <AnimatePresence>
            {!isMobile && rightPanel === "subagents" && activeSubagentMessage && (
              <motion.aside
                initial={shouldReduceMotion ? false : { width: 0, opacity: 0 }}
                animate={{ width: 420, opacity: 1 }}
                exit={shouldReduceMotion ? { opacity: 0 } : { width: 0, opacity: 0 }}
                className="shrink-0 overflow-hidden"
              >
                <Suspense fallback={<LazyPanelFallback />}>
                  <SubAgentWorkspacePanel
                    open
                    onClose={closeSubagents}
                    message={activeSubagentMessage}
                    width={420}
                  />
                </Suspense>
              </motion.aside>
            )}
          </AnimatePresence>

          <AnimatePresence>
            {isMobile && rightPanel === "activity" && activeActivityMessage && (
              <motion.div
                initial={shouldReduceMotion ? false : { opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                className="fixed inset-0 z-40 bg-black/45"
                onClick={closeActivity}
                role="presentation"
              >
                <motion.div
                  initial={shouldReduceMotion ? false : { y: "100%" }}
                  animate={{ y: 0 }}
                  exit={shouldReduceMotion ? { opacity: 0 } : { y: "100%" }}
                  transition={shouldReduceMotion ? { duration: 0 } : { type: "spring", damping: 28, stiffness: 260 }}
                  className="absolute bottom-0 left-0 right-0 h-[78vh] rounded-t-2xl overflow-hidden"
                  onClick={(e) => e.stopPropagation()}
                  role="dialog"
                  aria-modal="true"
                  aria-label={t("playground.activity.title", "Activity")}
                >
                  <Suspense fallback={<LazyPanelFallback />}>
                    <ActivityPanel
                      open
                      onClose={closeActivity}
                      message={activeActivityMessage}
                      width={mobilePanelWidth}
                      onToolApproval={handleToolApproval}
                    />
                  </Suspense>
                </motion.div>
              </motion.div>
            )}
          </AnimatePresence>

          {typeof document !== "undefined" ? createPortal(
            <AnimatePresence>
              {isMobile && rightPanel === "subagents" && activeSubagentMessage && (
              <motion.div
                initial={shouldReduceMotion ? false : { opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                className="fixed inset-0 z-40 bg-black/45"
                onClick={closeSubagents}
                role="presentation"
              >
                <motion.div
                  initial={shouldReduceMotion ? false : { y: "100%" }}
                  animate={{ y: 0 }}
                  exit={shouldReduceMotion ? { opacity: 0 } : { y: "100%" }}
                  transition={shouldReduceMotion ? { duration: 0 } : { type: "spring", damping: 28, stiffness: 260 }}
                  className="absolute inset-x-0 bottom-0 h-[88vh] overflow-hidden rounded-t-2xl"
                  onClick={(event) => event.stopPropagation()}
                  role="dialog"
                  aria-modal="true"
                  aria-label="Sub-agent workbench"
                >
                  <Suspense fallback={<LazyPanelFallback />}>
                    <SubAgentWorkspacePanel
                      open
                      onClose={closeSubagents}
                      message={activeSubagentMessage}
                      width={typeof window === "undefined" ? 430 : window.innerWidth}
                      className="rounded-t-2xl"
                    />
                  </Suspense>
                </motion.div>
              </motion.div>
              )}
            </AnimatePresence>,
            document.body,
          ) : null}

          {/* Artifacts Panel — same 380px width as ActivityPanel so the
              right lane feels uniform when switching chips. */}
          <AnimatePresence>
            {showArtifacts && !isMobile && rightPanel === "artifacts" && (
              <motion.aside initial={shouldReduceMotion ? false : { width: 0, opacity: 0 }} animate={{ width: 380, opacity: 1 }} exit={shouldReduceMotion ? { opacity: 0 } : { width: 0, opacity: 0 }} className="shrink-0 overflow-hidden">
                <div className="h-full w-[380px]">
                  <Suspense fallback={<LazyPanelFallback />}>
                    <ArtifactsPanel
                      isOpen={showArtifacts}
                      onClose={() => setShowArtifacts(false)}
                      artifacts={artifacts}
                      loadError={artifactLoadError}
                      onRefresh={() => void refreshArtifacts()}
                      refreshStatus={artifactRefreshStatus}
                      refreshDisabled={!canRefreshArtifacts}
                      executionStatus={codeExecution.status}
                      executionOutput={codeExecution.output}
                      currentCode={codeExecution.code || undefined}
                      executionTimeMs={codeExecution.executionTimeMs || undefined}
                      outputFiles={codeExecution.outputFiles}
                    />
                  </Suspense>
                </div>
              </motion.aside>
            )}
          </AnimatePresence>

          <AnimatePresence>
            {showArtifacts && isMobile && (
              <motion.div
                initial={shouldReduceMotion ? false : { opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                className="fixed inset-0 z-40 bg-black/45"
                onClick={() => setShowArtifacts(false)}
                role="presentation"
              >
                <motion.div
                  initial={shouldReduceMotion ? false : { y: "100%" }}
                  animate={{ y: 0 }}
                  exit={shouldReduceMotion ? { opacity: 0 } : { y: "100%" }}
                  transition={shouldReduceMotion ? { duration: 0 } : { type: "spring", damping: 28, stiffness: 260 }}
                  className="absolute bottom-0 left-0 right-0 h-[78vh] rounded-t-2xl overflow-hidden"
                  onClick={(e) => e.stopPropagation()}
                  role="dialog"
                  aria-modal="true"
                  aria-label={t("assistant.artifacts", "Artifacts")}
                >
                  <Suspense fallback={<LazyPanelFallback />}>
                    <ArtifactsPanel
                      isOpen={showArtifacts}
                      onClose={() => setShowArtifacts(false)}
                      artifacts={artifacts}
                      loadError={artifactLoadError}
                      onRefresh={() => void refreshArtifacts()}
                      refreshStatus={artifactRefreshStatus}
                      refreshDisabled={!canRefreshArtifacts}
                      executionStatus={codeExecution.status}
                      executionOutput={codeExecution.output}
                      currentCode={codeExecution.code || undefined}
                      executionTimeMs={codeExecution.executionTimeMs || undefined}
                      outputFiles={codeExecution.outputFiles}
                      className="h-full rounded-t-2xl"
                    />
                  </Suspense>
                </motion.div>
              </motion.div>
            )}
          </AnimatePresence>

          <AnimatePresence>
            {!isMobile && rightPanel === "local_os" && (
              <motion.aside
                initial={shouldReduceMotion ? false : { width: 0, opacity: 0 }}
                animate={{ width: 560, opacity: 1 }}
                exit={shouldReduceMotion ? { opacity: 0 } : { width: 0, opacity: 0 }}
                className="shrink-0 overflow-hidden"
              >
                <Suspense fallback={<LazyPanelFallback />}>
                  <LocalOSPanel
                    open
                    onClose={() => setShowLocalOS(false)}
                    state={localOSState}
                    width={560}
                  />
                </Suspense>
              </motion.aside>
            )}
          </AnimatePresence>

          <AnimatePresence>
            {isMobile && rightPanel === "local_os" && (
              <motion.div
                initial={shouldReduceMotion ? false : { opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                className="fixed inset-0 z-40 bg-black/45"
                onClick={() => setShowLocalOS(false)}
                role="presentation"
              >
                <motion.div
                  initial={shouldReduceMotion ? false : { y: "100%" }}
                  animate={{ y: 0 }}
                  exit={shouldReduceMotion ? { opacity: 0 } : { y: "100%" }}
                  transition={
                    shouldReduceMotion
                      ? { duration: 0 }
                      : { type: "spring", damping: 28, stiffness: 260 }
                  }
                  className="absolute inset-x-0 bottom-0 h-[92vh] overflow-hidden rounded-t-2xl"
                  onClick={(event) => event.stopPropagation()}
                  role="dialog"
                  aria-modal="true"
                  aria-label="Local files"
                >
                  <Suspense fallback={<LazyPanelFallback />}>
                    <LocalOSPanel
                      open
                      onClose={() => setShowLocalOS(false)}
                      state={localOSState}
                      width={typeof window === "undefined" ? 430 : window.innerWidth}
                      className="rounded-t-2xl"
                    />
                  </Suspense>
                </motion.div>
              </motion.div>
            )}
          </AnimatePresence>

        </div>
      </div>
    </TooltipProvider>

    {/* Share Dialog */}
    {showShareDialog && (
      <Suspense fallback={null}>
        <ShareDialog
          sessionId={activeSessionId || ""}
          messageCount={messages.length}
          artifactCount={uniqueArtifactCount}
          isOpen
          onClose={() => setShowShareDialog(false)}
        />
      </Suspense>
    )}
    {showMemoryDialog && (
      <Suspense fallback={null}>
        <MemoryDialog open onClose={() => setShowMemoryDialog(false)} />
      </Suspense>
    )}
    {showToolsDialog && activeSessionId && (
      <Suspense fallback={null}>
        <ToolsDialog sessionId={activeSessionId} open onClose={() => setShowToolsDialog(false)} />
      </Suspense>
    )}

    {/* Connectors Panel */}
    {showConnectors && (
      <Suspense fallback={null}>
        <ConnectorsPanel
          open
          onClose={() => setShowConnectors(false)}
          onCountChange={setConnectorCount}
        />
      </Suspense>
    )}
    </RightPanelContext.Provider>
    </>
  );
}

export default AssistantPage;
