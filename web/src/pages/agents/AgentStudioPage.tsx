import { useEffect, useRef, useState, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Alert,
  App as AntApp,
  Button,
  Checkbox,
  Drawer,
  Form,
  Input,
  InputNumber,
  Select,
  Skeleton,
  Slider,
  Tabs,
  Typography,
} from "antd";
import {
  AlertTriangle,
  ArrowLeft,
  BarChart3,
  BookOpen,
  Bot,
  ChevronRight,
  Copy,
  FileText,
  Gauge,
  Menu,
  RefreshCw,
  Save,
  Settings2,
  ShieldCheck,
  Sparkles,
  Wrench,
  X,
  type LucideIcon,
} from "lucide-react";
import { useLocation, useNavigate, useParams } from "react-router-dom";
import { useTranslation } from "react-i18next";

import {
  agentErrorDetail,
  getAgent,
  getAgentDraft,
  listAgentConnectors,
  listAgentDatasets,
  listAgentEvalDatasets,
  listAgentMcpTools,
  listAgentModels,
  listAgentSkills,
  listAgentTools,
  listAgentVersions,
  updateAgentDraft,
} from "@/api/agents";
import type {
  AgentCapabilityBinding,
  AgentCatalogConnector,
  AgentCatalogMcpTool,
  AgentCatalogSkill,
  AgentCatalogTool,
  AgentDetail,
  AgentDraft,
  AgentKnowledgeBinding,
  AgentSpec,
  AgentVersion,
} from "@/types/agents";
import { capabilityDisplayName } from "./agentCatalogPresentation";
import { AgentPreviewPanel } from "./AgentPreviewPanel";
import { AgentReleasePanel } from "./AgentReleasePanel";
import "./agent-studio.css";

const { Paragraph, Text, Title } = Typography;

type SectionKey =
  | "overview"
  | "instructions"
  | "model"
  | "capabilities"
  | "knowledge"
  | "memory"
  | "eval"
  | "channels";

const SECTIONS: Array<{ key: SectionKey; icon: LucideIcon }> = [
  { key: "overview", icon: Settings2 },
  { key: "instructions", icon: FileText },
  { key: "model", icon: Gauge },
  { key: "capabilities", icon: Wrench },
  { key: "knowledge", icon: BookOpen },
  { key: "memory", icon: ShieldCheck },
  { key: "eval", icon: Sparkles },
  { key: "channels", icon: BarChart3 },
];

interface CatalogState {
  tools: AgentCatalogTool[];
  mcp: AgentCatalogMcpTool[];
  skills: AgentCatalogSkill[];
  connectors: AgentCatalogConnector[];
  datasets: Awaited<ReturnType<typeof listAgentDatasets>>;
  evalDatasets: Awaited<ReturnType<typeof listAgentEvalDatasets>>;
  models: Awaited<ReturnType<typeof listAgentModels>>;
  degraded: string[];
  retry: () => void;
}

interface DraftSavePayload {
  revision: number;
  spec: AgentSpec;
  name: string;
  description: string;
  savedName: string;
  savedDescription: string;
}

function deepEqual(left: unknown, right: unknown): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

function isSafeIconUrl(value: string | null | undefined): boolean {
  if (!value) return true;
  try {
    const url = new URL(value);
    return url.protocol === "https:" && !url.username && !url.password && !url.search && !url.hash;
  } catch {
    return false;
  }
}

function SectionNav({ selected, onSelect }: { selected: SectionKey; onSelect: (key: SectionKey) => void }) {
  const { t } = useTranslation();
  return (
    <nav className="agent-section-nav" aria-label={t("agents.studio.sectionsLabel")}>
      {SECTIONS.map(({ key, icon: Icon }) => (
        <button key={key} type="button" className={selected === key ? "is-active" : ""} aria-current={selected === key ? "page" : undefined} onClick={() => onSelect(key)}>
          <Icon size={16} /><span>{t(`agents.studio.sections.${key}`)}</span>
        </button>
      ))}
    </nav>
  );
}

function ReadOnlyNotice() {
  const { t } = useTranslation();
  return <Alert type="info" showIcon title={t("agents.studio.viewerTitle")} description={t("agents.studio.viewerDescription")} />;
}

function FieldSection({ title, description, children }: { title: string; description: string; children: ReactNode }) {
  return (
    <section className="agent-field-section">
      <header><Title level={3}>{title}</Title><Paragraph type="secondary">{description}</Paragraph></header>
      {children}
    </section>
  );
}

function CapabilityOption({
  checked,
  disabled,
  title,
  description,
  meta,
  onChange,
  onTest,
}: {
  checked: boolean;
  disabled?: boolean;
  title: string;
  description: string;
  meta?: string;
  onChange: (checked: boolean) => void;
  onTest: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div className={`agent-capability-option${disabled ? " is-disabled" : ""}`}>
      <Checkbox aria-label={t("agents.studio.capabilities.enable", { title })} checked={checked} disabled={disabled} onChange={(event) => onChange(event.target.checked)} />
      <span className="agent-capability-copy"><strong>{title}</strong><span>{description}</span>{meta && <small>{meta}</small>}</span>
      <Button size="small" disabled={disabled} onClick={onTest}>{t("agents.studio.capabilities.testPreview")}</Button>
    </div>
  );
}

function AgentStudioWorkspace({
  initialAgent,
  initialDraft,
  versions,
  catalog,
}: {
  initialAgent: AgentDetail;
  initialDraft: AgentDraft;
  versions: AgentVersion[];
  catalog: CatalogState;
}) {
  const { t, i18n } = useTranslation();
  const navigate = useNavigate();
  const location = useLocation();
  const queryClient = useQueryClient();
  const { message: messageApi } = AntApp.useApp();
  const [agent, setAgent] = useState(initialAgent);
  const [name, setName] = useState(initialAgent.name);
  const [description, setDescription] = useState(initialAgent.description);
  const [spec, setSpec] = useState<AgentSpec>(initialDraft.spec);
  const [savedSpec, setSavedSpec] = useState<AgentSpec>(initialDraft.spec);
  const [savedName, setSavedName] = useState(initialAgent.name);
  const [savedDescription, setSavedDescription] = useState(initialAgent.description);
  const [revision, setRevision] = useState(initialDraft.revision);
  const [section, setSection] = useState<SectionKey>(() => (
    location.pathname.endsWith("/evals")
      ? "eval"
      : location.pathname.endsWith("/versions")
        ? "channels"
        : "overview"
  ));
  const [mobileTab, setMobileTab] = useState("configure");
  const [drawerOpen, setDrawerOpen] = useState(false);
  const mobileSectionTriggerRef = useRef<HTMLButtonElement>(null);
  const draftReadGenerationRef = useRef(0);
  const [saveState, setSaveState] = useState<"clean" | "dirty" | "saving" | "saved" | "conflict" | "error">("clean");
  const [conflictRevision, setConflictRevision] = useState<number | null>(null);
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [saveError, setSaveError] = useState<string | null>(null);
  const [lastSavedAt, setLastSavedAt] = useState(initialDraft.updated_at);
  const canEdit = agent.caller_role !== "viewer" && agent.status !== "archived";
  const dirty = !deepEqual(spec, savedSpec) || name !== savedName || description !== savedDescription;
  const selectedModelInfo = catalog.models.find((model) => model.id === spec.model.model_id);
  const includesImages = spec.knowledge.some((binding) => Boolean(binding.retrieval_config.include_images));
  const memoryMode = String(spec.memory.mode || "session");
  const memoryModeLabel = t(`agents.studio.memory.${memoryMode === "user" ? "user" : memoryMode === "off" ? "off" : "session"}`);
  const degradedLabels = catalog.degraded.map((item) => {
    if (item === "tools") return t("agents.studio.capabilities.platform");
    if (item === "MCP") return t("agents.studio.capabilities.mcp");
    if (item === "Skills") return t("agents.studio.capabilities.skills");
    if (item === "Connectors") return t("agents.studio.capabilities.connectors");
    if (item === "Knowledge") return t("agents.studio.sections.knowledge");
    if (item === "Eval Datasets") return t("agents.studio.sections.eval");
    return t("agents.studio.sections.model");
  });
  const validationIssues = [
    !spec.instructions.trim() ? t("agents.studio.validation.instructionsRequired") : null,
    !isSafeIconUrl(spec.identity.icon_url) ? t("agents.studio.validation.iconInvalid") : null,
    selectedModelInfo && !selectedModelInfo.supports_tools && spec.capabilities.length > 0 ? t("agents.studio.validation.toolsUnsupported") : null,
    selectedModelInfo && !selectedModelInfo.supports_vision && includesImages ? t("agents.studio.validation.visionUnsupported") : null,
  ].filter(Boolean) as string[];

  useEffect(() => {
    if (saveState === "saving" || saveState === "conflict" || saveState === "error") return;
    setSaveState(dirty ? "dirty" : saveState === "saved" ? "saved" : "clean");
  }, [dirty, saveState]);

  useEffect(() => {
    const beforeUnload = (event: BeforeUnloadEvent) => {
      if (!dirty) return;
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", beforeUnload);
    return () => window.removeEventListener("beforeunload", beforeUnload);
  }, [dirty]);

  useEffect(() => {
    if (location.pathname.endsWith("/evals")) setSection("eval");
    if (location.pathname.endsWith("/versions")) setSection("channels");
  }, [location.pathname]);

  useEffect(() => () => { draftReadGenerationRef.current += 1; }, []);

  const saveMutation = useMutation({
    retry: false,
    mutationFn: async (payload: DraftSavePayload) => {
      const agentChanges = {
        ...(payload.name !== payload.savedName ? { name: payload.name.trim() } : {}),
        ...(payload.description !== payload.savedDescription
          ? { description: payload.description.trim() }
          : {}),
      };
      const savedDraft = await updateAgentDraft(
        agent.agent_id,
        payload.revision,
        payload.spec,
        agentChanges,
      );
      const savedAgent: AgentDetail = {
        ...agent,
        ...agentChanges,
        draft_revision: savedDraft.revision,
        updated_at: savedDraft.updated_at,
      };
      return { savedDraft, savedAgent };
    },
    onMutate: () => {
      setSaveState("saving");
      setSaveError(null);
      setFieldErrors({});
    },
    onSuccess: ({ savedDraft, savedAgent }, payload) => {
      setRevision(savedDraft.revision);
      setSavedSpec(savedDraft.spec);
      setSpec((current) => deepEqual(current, payload.spec) ? savedDraft.spec : current);
      setSavedName(savedAgent.name);
      setName((current) => current === payload.name ? savedAgent.name : current);
      setSavedDescription(savedAgent.description);
      setDescription((current) => current === payload.description ? savedAgent.description : current);
      setAgent(savedAgent);
      setLastSavedAt(savedDraft.updated_at);
      setConflictRevision(null);
      setSaveState("saved");
      // The detail/draft queries use staleTime: Infinity, so without writing
      // the fresh payloads back into the cache a remount within the gcTime
      // window (e.g. opening Analytics then returning) re-seeds the workspace
      // from the pre-save draft — the edits look reverted and the next save
      // sends the stale If-Match revision and always 409s. Keep the cache in
      // sync with the just-saved server state.
      queryClient.setQueryData(["agent", agent.agent_id, "draft"], savedDraft);
      queryClient.setQueryData(["agent", agent.agent_id], savedAgent);
      void queryClient.invalidateQueries({
        queryKey: ["agent", agent.agent_id, "release-evaluations"],
      });
      messageApi.success(t("agents.studio.draftSaved"));
    },
    onError: (error) => {
      const detail = agentErrorDetail(error);
      if (detail.status === 409 && detail.code === "AGENT_DRAFT_CONFLICT") {
        setConflictRevision(detail.currentRevision ?? null);
        setSaveState("conflict");
        return;
      }
      const errors = Object.fromEntries((detail.errors ?? []).map((item) => [item.field, item.message]));
      setFieldErrors(errors);
      setSaveError(detail.message);
      setSaveState("error");
      const firstField = detail.errors?.[0]?.field;
      const focusLabel = firstField === "name"
        ? t("agents.studio.overview.name")
        : firstField === "description"
          ? t("agents.studio.overview.descriptionLabel")
          : firstField?.includes("instructions")
            ? t("agents.studio.instructions.label")
            : firstField?.includes("model")
              ? t("agents.studio.model.label")
              : null;
      if (focusLabel) requestAnimationFrame(() => document.querySelector<HTMLElement>(`[aria-label="${focusLabel}"]`)?.focus());
    },
  });

  const requestSave = () => {
    draftReadGenerationRef.current += 1;
    saveMutation.mutate({ revision, spec, name, description, savedName, savedDescription });
  };

  const discard = () => {
    draftReadGenerationRef.current += 1;
    setSpec(savedSpec);
    setName(savedName);
    setDescription(savedDescription);
    setFieldErrors({});
    setSaveError(null);
    setConflictRevision(null);
    setSaveState("clean");
  };

  const readDraft = async (keepLocal: boolean) => {
    const generation = ++draftReadGenerationRef.current;
    try {
      const [freshAgent, freshDraft] = await Promise.all([getAgent(agent.agent_id), getAgentDraft(agent.agent_id)]);
      if (generation !== draftReadGenerationRef.current) return;
      setAgent(freshAgent);
      setSavedName(freshAgent.name);
      setSavedDescription(freshAgent.description);
      setSavedSpec(freshDraft.spec);
      setRevision(freshDraft.revision);
      setLastSavedAt(freshDraft.updated_at);
      // Reapply leaves the current editor intact, including edits typed while
      // the read was pending. A click-time snapshot would silently lose them.
      if (!keepLocal) {
        setName(freshAgent.name);
        setDescription(freshAgent.description);
        setSpec(freshDraft.spec);
      }
      setFieldErrors({});
      setSaveError(null);
      setConflictRevision(null);
      setSaveState(keepLocal ? "dirty" : "clean");
      // Reopening must seed from the read server baseline, never local edits.
      queryClient.setQueryData(["agent", agent.agent_id, "draft"], freshDraft);
      queryClient.setQueryData(["agent", agent.agent_id], freshAgent);
    } catch (error) {
      if (generation !== draftReadGenerationRef.current) return;
      setSaveError(agentErrorDetail(error).message);
    }
  };

  const reloadDraft = () => readDraft(false);
  const reloadAndReapply = () => readDraft(true);

  const copyChanges = async () => {
    await navigator.clipboard.writeText(JSON.stringify({ name, description, spec }, null, 2));
    messageApi.success(t("agents.studio.changesCopied"));
  };

  const guardedBack = () => {
    if (!dirty || window.confirm(t("agents.studio.leaveConfirm"))) navigate("/agents");
  };

  // The Analytics button unmounts the workspace (separate route), so it must
  // run the same dirty check as the back arrow or unsaved Draft edits are
  // silently discarded (AS-UI-004: warn before leaving an unsaved Draft).
  const guardedAnalytics = () => {
    if (!dirty || window.confirm(t("agents.studio.leaveConfirm"))) {
      navigate(`/agents/${agent.agent_id}/analytics`);
    }
  };

  const toggleCapability = (binding: AgentCapabilityBinding, checked: boolean) => {
    if (!canEdit) return;
    setSpec((current) => ({
      ...current,
      capabilities: checked
        ? [...current.capabilities.filter((item) => !(item.type === binding.type && item.resource_id === binding.resource_id)), binding]
        : current.capabilities.filter((item) => !(item.type === binding.type && item.resource_id === binding.resource_id)),
    }));
  };
  const selected = new Set(spec.capabilities.map((binding) => `${binding.type}:${binding.resource_id}`));
  const conflictFields = [
    name !== savedName ? t("agents.studio.overview.name") : null,
    description !== savedDescription ? t("agents.studio.overview.descriptionLabel") : null,
    !deepEqual(spec, savedSpec) ? t("agents.studio.configuration") : null,
  ].filter(Boolean).join(", ");
  const openPreviewTest = () => {
    setMobileTab("preview");
    requestAnimationFrame(() => document.querySelector<HTMLElement>("[data-testid='agent-preview-panel'] textarea")?.focus());
  };
  const selectSection = (key: SectionKey) => {
    setSection(key);
    if (key === "eval") {
      navigate(`/agents/${agent.agent_id}/evals`);
    } else if (key === "channels") {
      navigate(`/agents/${agent.agent_id}/versions`);
    } else if (location.pathname.endsWith("/evals") || location.pathname.endsWith("/versions")) {
      navigate(`/agents/${agent.agent_id}`);
    }
  };
  const currentSection = SECTIONS.find((item) => item.key === section) ?? SECTIONS[0];
  const currentSectionLabel = t(`agents.studio.sections.${currentSection.key}`);
  const saveLabel = saveState === "saving"
    ? t("agents.studio.saveStates.saving")
    : saveState === "saved"
      ? t("agents.studio.saveStates.saved")
      : saveState === "conflict"
        ? t("agents.studio.saveStates.conflict")
        : dirty
          ? t("agents.studio.saveStates.dirty")
          : t("agents.studio.saveStates.clean");
  const locale = i18n.resolvedLanguage === "zh-CN" ? "zh-CN" : "en-US";

  const renderSection = () => {
    if (section === "overview") {
      return (
        <FieldSection title={t("agents.studio.sections.overview")} description={t("agents.studio.overview.description")}>
          {!canEdit && <ReadOnlyNotice />}
          <Form layout="vertical" disabled={!canEdit}>
            <Form.Item label={t("agents.studio.overview.name")} required validateStatus={fieldErrors.name ? "error" : undefined} help={<span id="studio-agent-name-help">{fieldErrors.name || t("agents.create.characterCount", { count: name.length, limit: 255 })}</span>}>
              <Input aria-label={t("agents.studio.overview.name")} aria-describedby="studio-agent-name-help" aria-invalid={Boolean(fieldErrors.name)} value={name} maxLength={255} onChange={(event) => setName(event.target.value)} />
            </Form.Item>
            <Form.Item label={t("agents.studio.overview.descriptionLabel")} validateStatus={fieldErrors.description ? "error" : undefined} help={<span id="studio-agent-description-help">{fieldErrors.description || t("agents.create.characterCount", { count: description.length, limit: 4000 })}</span>}>
              <Input.TextArea aria-label={t("agents.studio.overview.descriptionLabel")} aria-describedby="studio-agent-description-help" aria-invalid={Boolean(fieldErrors.description)} value={description} rows={5} maxLength={4000} onChange={(event) => setDescription(event.target.value)} placeholder={t("agents.studio.overview.descriptionPlaceholder")} />
            </Form.Item>
            <Form.Item label={t("agents.studio.overview.welcome")} help={t("agents.studio.overview.welcomeHelp")}>
              <Input.TextArea aria-label={t("agents.studio.overview.welcome")} value={spec.identity.welcome_message} rows={3} maxLength={4000} onChange={(event) => setSpec((current) => ({ ...current, identity: { ...current.identity, welcome_message: event.target.value } }))} />
            </Form.Item>
            <div className="agent-form-grid">
              <Form.Item label={t("agents.studio.overview.iconUrl")} validateStatus={!isSafeIconUrl(spec.identity.icon_url) ? "error" : undefined} help={!isSafeIconUrl(spec.identity.icon_url) ? t("agents.studio.overview.iconError") : t("agents.studio.overview.iconHelp")}>
                <Input type="url" aria-label={t("agents.studio.overview.iconUrl")} aria-invalid={!isSafeIconUrl(spec.identity.icon_url)} value={spec.identity.icon_url || ""} maxLength={2048} onChange={(event) => setSpec((current) => ({ ...current, identity: { ...current.identity, icon_url: event.target.value || null } }))} placeholder="https://example.com/agent-icon.png" />
              </Form.Item>
              <Form.Item label={t("agents.studio.overview.themeColor")} help={t("agents.studio.overview.themeHelp")}>
                <Input type="color" aria-label={t("agents.studio.overview.themeColor")} value={spec.identity.theme_color || "#7B7BE8"} onChange={(event) => setSpec((current) => ({ ...current, identity: { ...current.identity, theme_color: event.target.value } }))} />
              </Form.Item>
            </div>
            <Form.Item label={t("agents.studio.overview.suggestedPrompts")} help={t("agents.studio.overview.suggestedHelp")}>
              <Input.TextArea aria-label={t("agents.studio.overview.suggestedPrompts")} value={spec.identity.suggested_prompts.join("\n")} rows={4} onChange={(event) => setSpec((current) => ({ ...current, identity: { ...current.identity, suggested_prompts: event.target.value.split("\n").map((prompt) => prompt.trim()).filter(Boolean).slice(0, 12) } }))} placeholder={t("agents.studio.overview.suggestedPlaceholder")} />
            </Form.Item>
          </Form>
          <div className="agent-runtime-summary">
            <Title level={4}>{t("agents.studio.overview.runtimeSummary")}</Title>
            <button type="button" onClick={() => setSection("model")}><span>{t("agents.studio.overview.model")}</span><strong>{spec.model.model_id || t("agents.common.serverDefault")}</strong><ChevronRight size={15} /></button>
            <button type="button" onClick={() => setSection("capabilities")}><span>{t("agents.studio.overview.capabilities")}</span><strong>{spec.capabilities.length}</strong><ChevronRight size={15} /></button>
            <button type="button" onClick={() => setSection("knowledge")}><span>{t("agents.studio.overview.knowledgeSources")}</span><strong>{spec.knowledge.length}</strong><ChevronRight size={15} /></button>
            <button type="button" onClick={() => setSection("memory")}><span>{t("agents.studio.overview.memory")}</span><strong>{memoryModeLabel}</strong><ChevronRight size={15} /></button>
          </div>
        </FieldSection>
      );
    }
    if (section === "instructions") {
      return (
        <FieldSection title={t("agents.studio.sections.instructions")} description={t("agents.studio.instructions.description")}>
          {!canEdit && <ReadOnlyNotice />}
          <Alert type="info" showIcon title={t("agents.studio.instructions.safetyTitle")} description={t("agents.studio.instructions.safetyDescription")} />
          <Form layout="vertical" disabled={!canEdit}>
            <div className="agent-form-grid">
              <Form.Item label={t("agents.studio.instructions.outcomeGoal")} help={t("agents.studio.instructions.outcomeHelp")}>
                <Input.TextArea aria-label={t("agents.studio.instructions.outcomeGoal")} value={spec.outcome.goal} rows={3} maxLength={4000} onChange={(event) => setSpec((current) => ({ ...current, outcome: { ...current.outcome, goal: event.target.value } }))} />
              </Form.Item>
              <Form.Item label={t("agents.studio.instructions.successCriteria")} help={t("agents.studio.instructions.onePerLine")}>
                <Input.TextArea aria-label={t("agents.studio.instructions.successCriteria")} value={spec.outcome.success_criteria.join("\n")} rows={3} onChange={(event) => setSpec((current) => ({ ...current, outcome: { ...current.outcome, success_criteria: event.target.value.split("\n").map((item) => item.trim()).filter(Boolean).slice(0, 32) } }))} />
              </Form.Item>
            </div>
            <Form.Item label={t("agents.studio.instructions.triggers")} help={t("agents.studio.instructions.onePerLine")}>
              <Input.TextArea aria-label={t("agents.studio.instructions.triggers")} value={spec.outcome.triggers.join("\n")} rows={2} onChange={(event) => setSpec((current) => ({ ...current, outcome: { ...current.outcome, triggers: event.target.value.split("\n").map((item) => item.trim()).filter(Boolean).slice(0, 16) } }))} />
            </Form.Item>
            <div className="agent-form-grid">
              <Form.Item label={t("agents.studio.instructions.inputs")} help={t("agents.studio.instructions.onePerLine")}>
                <Input.TextArea aria-label={t("agents.studio.instructions.inputs")} value={spec.outcome.inputs.join("\n")} rows={2} onChange={(event) => setSpec((current) => ({ ...current, outcome: { ...current.outcome, inputs: event.target.value.split("\n").map((item) => item.trim()).filter(Boolean).slice(0, 32) } }))} />
              </Form.Item>
              <Form.Item label={t("agents.studio.instructions.humanBoundaries")} help={t("agents.studio.instructions.onePerLine")}>
                <Input.TextArea aria-label={t("agents.studio.instructions.humanBoundaries")} value={spec.outcome.human_boundaries.join("\n")} rows={2} onChange={(event) => setSpec((current) => ({ ...current, outcome: { ...current.outcome, human_boundaries: event.target.value.split("\n").map((item) => item.trim()).filter(Boolean).slice(0, 32) } }))} />
              </Form.Item>
            </div>
            <Form.Item label={t("agents.studio.instructions.label")} required validateStatus={!spec.instructions.trim() || fieldErrors.instructions ? "error" : undefined} help={fieldErrors.instructions || (!spec.instructions.trim() ? t("agents.studio.instructions.required") : t("agents.studio.instructions.count", { count: spec.instructions.length, tokens: Math.max(1, Math.ceil(spec.instructions.length / 4)) }))}>
              <Input.TextArea aria-label={t("agents.studio.instructions.label")} aria-invalid={!spec.instructions.trim()} className="agent-instructions-editor" value={spec.instructions} rows={18} maxLength={100000} onChange={(event) => setSpec((current) => ({ ...current, instructions: event.target.value }))} placeholder={t("agents.studio.instructions.placeholder")} />
            </Form.Item>
            <Text type="secondary">{t("agents.studio.instructions.formatHelp")}</Text>
          </Form>
        </FieldSection>
      );
    }
    if (section === "model") {
      // Empty id → the server applies its deployment default when the model is omitted.
      const modelRows = catalog.models.length ? catalog.models : [{ id: "", name: t("agents.common.serverDefault"), provider: "dashscope", context_window: 0, max_output_tokens: 4096, supports_vision: false, supports_tools: true }];
      return (
        <FieldSection title={t("agents.studio.sections.model")} description={t("agents.studio.model.description")}>
          {!canEdit && <ReadOnlyNotice />}
          {catalog.degraded.includes("models") && <Alert type="warning" showIcon title={t("agents.studio.model.catalogError")} description={t("agents.studio.model.catalogErrorDescription")} />}
          <Form layout="vertical" disabled={!canEdit}>
            <div className="agent-form-grid">
              <Form.Item label={t("agents.studio.model.provider")} required><Select aria-label={t("agents.studio.model.provider")} value={spec.model.provider_id || undefined} options={Array.from(new Set(modelRows.map((model) => model.provider))).map((provider) => ({ value: provider, label: provider === "dashscope" ? "DashScope" : provider }))} onChange={(value) => setSpec((current) => ({ ...current, model: { ...current.model, provider_id: value, model_id: "" } }))} /></Form.Item>
              <Form.Item label={t("agents.studio.model.label")}><Select aria-label={t("agents.studio.model.label")} placeholder={t("agents.common.serverDefault")} showSearch value={spec.model.model_id || undefined} options={modelRows.filter((model) => model.provider === spec.model.provider_id).map((model) => ({ value: model.id, label: model.name || model.id }))} onChange={(value) => setSpec((current) => ({ ...current, model: { ...current.model, model_id: value } }))} /></Form.Item>
            </div>
            <Form.Item label={t("agents.studio.model.temperature", { value: spec.model.temperature ?? 0.3 })}><Slider aria-label={t("agents.studio.model.temperature", { value: spec.model.temperature ?? 0.3 })} min={0} max={2} step={0.1} value={spec.model.temperature ?? 0.3} onChange={(value) => setSpec((current) => ({ ...current, model: { ...current.model, temperature: value } }))} /></Form.Item>
            <Form.Item label={t("agents.studio.model.maxTokens")}><InputNumber aria-label={t("agents.studio.model.maxTokens")} min={1} max={1_000_000} value={spec.model.max_tokens ?? 4096} onChange={(value) => setSpec((current) => ({ ...current, model: { ...current.model, max_tokens: value ?? 4096 } }))} /></Form.Item>
            <Form.Item label={t("agents.studio.model.thinking")} help={t("agents.studio.model.thinkingHelp")}><Select aria-label={t("agents.studio.model.thinking")} allowClear value={spec.model.thinking_mode || undefined} options={[{ value: "auto", label: t("agents.studio.model.providerDefault") }, { value: "enabled", label: t("agents.studio.model.enabled") }, { value: "disabled", label: t("agents.studio.model.disabled") }]} onChange={(value) => setSpec((current) => ({ ...current, model: { ...current.model, thinking_mode: value || null } }))} /></Form.Item>
          </Form>
          {selectedModelInfo && <Alert type={validationIssues.some((issue) => issue === t("agents.studio.validation.toolsUnsupported") || issue === t("agents.studio.validation.visionUnsupported")) ? "warning" : "info"} showIcon title={t("agents.studio.model.capabilityTitle")} description={t("agents.studio.model.capabilityDescription", { tools: t(selectedModelInfo.supports_tools ? "agents.studio.model.supported" : "agents.studio.model.unsupported"), vision: t(selectedModelInfo.supports_vision ? "agents.studio.model.supported" : "agents.studio.model.unsupported") })} />}
        </FieldSection>
      );
    }
    if (section === "capabilities") {
      return (
        <FieldSection title={t("agents.studio.sections.capabilities")} description={t("agents.studio.capabilities.description")}>
          {!canEdit && <ReadOnlyNotice />}
          <Tabs
            className="agent-capability-tabs"
            items={[
              {
                key: "platform",
                label: t("agents.studio.capabilities.platform"),
                children: <CapabilityGroup title={t("agents.studio.capabilities.platform")} empty={t("agents.studio.capabilities.platformEmpty")}>{catalog.tools.map((tool) => <CapabilityOption key={tool.id} checked={selected.has(`native:${tool.id}`)} disabled={!canEdit} title={capabilityDisplayName(tool.name, tool.title)} description={tool.summary || tool.description} meta={t("agents.studio.capabilities.platformMeta", { category: tool.category, risk: tool.risk })} onChange={(checked) => toggleCapability({ type: "native", resource_id: tool.id, config: { risk: tool.risk } }, checked)} onTest={openPreviewTest} />)}</CapabilityGroup>,
              },
              {
                key: "mcp",
                label: t("agents.studio.capabilities.mcp"),
                children: <CapabilityGroup title={t("agents.studio.capabilities.mcp")} empty={t("agents.studio.capabilities.mcpEmpty")}>{catalog.mcp.map((tool) => <CapabilityOption key={tool.tool_id} checked={selected.has(`mcp:${tool.runtime_name}`)} disabled={!canEdit || !tool.connection_id || !tool.enabled} title={capabilityDisplayName(tool.runtime_name)} description={tool.connection_id ? tool.description : t("agents.studio.capabilities.mcpSetupDescription")} meta={t("agents.studio.capabilities.mcpMeta", { server: tool.server_name, risk: tool.risk_level, state: tool.connection_id ? t("agents.studio.capabilities.mcpConfigured", { principal: tool.principal_type || t("agents.studio.capabilities.unknown") }) : t("agents.studio.capabilities.mcpSetup"), snapshot: tool.snapshot_id.slice(0, 8), schema: tool.schema_hash.slice(0, 8) })} onChange={(checked) => toggleCapability({ type: "mcp", resource_id: tool.runtime_name, resource_version: tool.snapshot_id, schema_hash: tool.schema_hash, config: { connection_id: tool.connection_id, principal_type: tool.principal_type, risk: tool.risk_level } }, checked)} onTest={openPreviewTest} />)}</CapabilityGroup>,
              },
              {
                key: "skills",
                label: t("agents.studio.capabilities.skills"),
                children: <CapabilityGroup title={t("agents.studio.capabilities.skills")} empty={t("agents.studio.capabilities.skillsEmpty")}>{catalog.skills.map((skill) => { const versionId = skill.version_id || ""; return <CapabilityOption key={`${skill.name}:${versionId}`} checked={selected.has(`skill:${versionId}`)} disabled={!canEdit || !versionId} title={skill.title || skill.name} description={skill.description || t("agents.studio.capabilities.skillDescription")} meta={versionId ? t("agents.studio.capabilities.skillMeta", { version: skill.version || t("agents.studio.capabilities.current"), hash: (skill.content_hash || t("agents.studio.capabilities.unavailable")).slice(0, 8) }) : t("agents.studio.capabilities.skillSetup")} onChange={(checked) => toggleCapability({ type: "skill", resource_id: versionId, resource_version: versionId, schema_hash: skill.content_hash, config: {} }, checked)} onTest={openPreviewTest} />; })}</CapabilityGroup>,
              },
              {
                key: "connectors",
                label: t("agents.studio.capabilities.connectors"),
                children: <CapabilityGroup title={t("agents.studio.capabilities.connectors")} empty={t("agents.studio.capabilities.connectorsEmpty")}>{catalog.connectors.map((connector) => { const canBind = connector.provider === "confluence" && Boolean(connector.grant_id && connector.principal_type); return <CapabilityOption key={connector.provider} checked={selected.has("connector:confluence_read")} disabled={!canEdit || !canBind} title={connector.display_name} description={canBind ? t("agents.studio.capabilities.connectorReady") : t("agents.studio.capabilities.connectorSetup")} meta={t("agents.studio.capabilities.connectorMeta", { connection: connector.connected ? t("agents.studio.capabilities.connected") : t("agents.studio.capabilities.notConnected"), principal: connector.principal_type || t("agents.studio.capabilities.none"), health: canBind ? t("agents.studio.capabilities.connected") : t("agents.studio.capabilities.notConnected"), permission: canBind ? t("agents.studio.capabilities.previewAllowed") : t("agents.studio.capabilities.previewBlocked"), preview: (connector.allowed_channels || []).includes("preview") ? t("agents.studio.capabilities.previewAllowed") : t("agents.studio.capabilities.previewBlocked") })} onChange={(checked) => toggleCapability({ type: "connector", resource_id: "confluence_read", config: { provider: connector.provider, principal_type: connector.principal_type, grant_id: connector.grant_id, tool_name: "confluence_read", risk: "low" } }, checked)} onTest={openPreviewTest} />; })}</CapabilityGroup>,
              },
            ]}
          />
          <Alert type="warning" showIcon title={t("agents.studio.capabilities.restrictionTitle")} description={t("agents.studio.capabilities.restrictionDescription")} />
        </FieldSection>
      );
    }
    if (section === "knowledge") {
      return (
        <FieldSection title={t("agents.studio.sections.knowledge")} description={t("agents.studio.knowledge.description")}>
          {!canEdit && <ReadOnlyNotice />}
          <Alert type="info" showIcon title={t("agents.studio.knowledge.testTitle")} description={t("agents.studio.knowledge.testDescription")} action={<Button onClick={openPreviewTest}>{t("agents.studio.knowledge.testButton")}</Button>} />
          <div className="agent-knowledge-list">
            {catalog.datasets.length === 0 && <Text type="secondary">{t("agents.studio.knowledge.empty")}</Text>}
            {catalog.datasets.map((dataset) => {
              const binding = spec.knowledge.find((item) => item.dataset_id === dataset.dataset_id);
              return (
                <article key={dataset.dataset_id} className={binding ? "is-selected" : ""}>
                  <Checkbox disabled={!canEdit} checked={Boolean(binding)} onChange={(event) => setSpec((current) => ({ ...current, knowledge: event.target.checked ? [...current.knowledge, { dataset_id: dataset.dataset_id, retrieval_config: { mode: "auto", top_k: 5, threshold: 0.4, include_images: false } }] : current.knowledge.filter((item) => item.dataset_id !== dataset.dataset_id) }))}><strong>{dataset.name}</strong><span>{dataset.description || t("agents.studio.knowledge.workspaceDataset")}</span><small>{dataset.statistics ? t("agents.studio.knowledge.stats", { documents: dataset.statistics.document_count, segments: dataset.statistics.segment_count }) : t("agents.studio.knowledge.statsUnavailable")} · {t("agents.studio.knowledge.permission", { permission: dataset.my_permission || t("agents.studio.knowledge.authorized") })}{dataset.updated_at ? ` · ${t("agents.studio.knowledge.updated", { date: new Date(dataset.updated_at).toLocaleDateString(locale) })}` : ""}</small></Checkbox>
                  {binding && <KnowledgeControls disabled={!canEdit} binding={binding} onChange={(next) => setSpec((current) => ({ ...current, knowledge: current.knowledge.map((item) => item.dataset_id === next.dataset_id ? next : item) }))} />}
                </article>
              );
            })}
          </div>
        </FieldSection>
      );
    }
    if (section === "memory") {
      return (
        <FieldSection title={t("agents.studio.sections.memory")} description={t("agents.studio.memory.description")}>
          {!canEdit && <ReadOnlyNotice />}
          <Form layout="vertical" disabled={!canEdit}>
            <Form.Item label={t("agents.studio.memory.mode")} help={t("agents.studio.memory.help")}><Select aria-label={t("agents.studio.memory.mode")} value={String(spec.memory.mode || "session")} options={[{ value: "off", label: t("agents.studio.memory.off") }, { value: "session", label: t("agents.studio.memory.session") }, { value: "user", label: t("agents.studio.memory.user") }]} onChange={(value) => setSpec((current) => ({ ...current, memory: { mode: value } }))} /></Form.Item>
          </Form>
          <Alert type="info" showIcon title={t("agents.studio.memory.credentialsTitle")} description={t("agents.studio.memory.credentialsDescription")} />
          <dl className="agent-policy-facts">
            <div><dt>{t("agents.studio.memory.attachments")}</dt><dd>{t("agents.studio.memory.attachmentsValue")}</dd></div>
            <div><dt>{t("agents.studio.memory.retention")}</dt><dd>{t("agents.studio.memory.retentionValue")}</dd></div>
            <div><dt>{t("agents.studio.memory.toolApproval")}</dt><dd>{t("agents.studio.memory.toolApprovalValue")}</dd></div>
            <div><dt>{t("agents.studio.memory.publicPolicy")}</dt><dd>{t("agents.studio.memory.publicPolicyValue")}</dd></div>
          </dl>
        </FieldSection>
      );
    }
    if (section === "eval") {
      return <FieldSection title={t("agents.studio.sections.eval")} description={t("agents.studio.eval.description")}><AgentReleasePanel mode="eval" agentId={agent.agent_id} draftRevision={revision} dirty={dirty} role={agent.caller_role} versions={versions} datasets={catalog.evalDatasets} /></FieldSection>;
    }
    return <FieldSection title={t("agents.studio.sections.channels")} description={t("agents.studio.channels.description")}><AgentReleasePanel mode="versions" agentId={agent.agent_id} draftRevision={revision} dirty={dirty} role={agent.caller_role} versions={versions} datasets={catalog.evalDatasets} /></FieldSection>;
  };

  return (
    <main className="agent-studio" data-testid="agent-studio-page">
      <header className="agent-studio-header">
        <div className="agent-studio-identity">
          <Button type="text" icon={<ArrowLeft size={16} />} onClick={guardedBack} aria-label={t("agents.studio.backToAgents")} />
          <span className="agent-avatar agent-avatar-large"><Bot size={20} /></span>
          <div><Title level={2}>{name || agent.name}</Title><div className="agent-studio-meta"><span>{t("agents.studio.draftRevision", { revision })}</span><span>{t(`agents.common.roles.${agent.caller_role}`)}</span><span className={`agent-save-state is-${saveState}`}>{saveLabel}</span></div></div>
        </div>
        <div className="agent-studio-actions">
          <Button icon={<BarChart3 size={16} />} onClick={guardedAnalytics}>{t("agents.analytics.title")}</Button>
          <Button disabled={!dirty || !canEdit || saveMutation.isPending} onClick={discard}>{t("agents.studio.discard")}</Button>
          <Button type="primary" icon={<Save size={16} />} disabled={!dirty || !canEdit || validationIssues.length > 0} loading={saveMutation.isPending} onClick={requestSave}>{t("agents.common.saveDraft")}</Button>
        </div>
      </header>

      {catalog.degraded.length > 0 && <Alert className="agent-degraded-banner" type="warning" showIcon title={t(catalog.degraded.length === 1 ? "agents.studio.degraded" : "agents.studio.degradedPlural", { catalogs: degradedLabels.join(", ") })} action={<Button icon={<RefreshCw size={14} />} onClick={catalog.retry}>{t("agents.common.retry")}</Button>} />}
      {saveError && <Alert className="agent-save-error" type="error" showIcon title={t("agents.studio.saveErrorTitle")} description={saveError} action={<div className="agent-error-actions"><Button onClick={() => void copyChanges()}>{t("agents.studio.copyDraft")}</Button><Button type="primary" onClick={requestSave}>{t("agents.studio.retrySave")}</Button></div>} closable onClose={() => setSaveError(null)} />}
      {saveState === "conflict" && <aside className="agent-conflict" role="alert"><AlertTriangle size={18} /><div><strong>{t("agents.studio.conflictTitle")}</strong><p>{t("agents.studio.conflictDescription", { revision: conflictRevision ?? t("agents.studio.latest") })}</p>{conflictFields && <p>{t("agents.studio.conflictFields", { fields: conflictFields })}</p>}<div><Button icon={<Copy size={14} />} onClick={() => void copyChanges()}>{t("agents.studio.copyChanges")}</Button><Button icon={<RefreshCw size={14} />} onClick={() => void reloadDraft()}>{t("agents.studio.reloadDraft")}</Button><Button type="primary" icon={<RefreshCw size={14} />} onClick={() => void reloadAndReapply()}>{t("agents.studio.reloadReapply")}</Button></div></div><Button type="text" icon={<X size={14} />} aria-label={t("agents.studio.dismissConflict")} onClick={() => setSaveState("dirty")} /></aside>}

      <div className="agent-mobile-tabs"><Tabs activeKey={mobileTab} onChange={setMobileTab} items={[{ key: "configure", label: t("agents.common.configure") }, { key: "preview", label: <span>{t("agents.common.preview")} <i aria-hidden /></span> }]} /></div>
      <Button ref={mobileSectionTriggerRef} className={`agent-mobile-section-trigger${mobileTab !== "configure" ? " is-hidden" : ""}`} icon={<Menu size={16} />} onClick={() => setDrawerOpen(true)}>{currentSectionLabel}<ChevronRight size={15} /></Button>

      <div className={`agent-studio-workbench mobile-${mobileTab}`}>
        <aside className="agent-studio-sections"><SectionNav selected={section} onSelect={selectSection} /><div className="agent-validation-summary"><AlertTriangle size={15} /> {t(validationIssues.length === 1 ? "agents.studio.validationSummaryOne" : "agents.studio.validationSummaryOther", { count: validationIssues.length })}</div></aside>
        <section className="agent-config-canvas" aria-label={t("agents.studio.configurationLabel", { section: currentSectionLabel })}>
          {renderSection()}
          <footer className="agent-config-footer"><Text type="secondary">{t("agents.studio.lastSaved", { time: new Date(lastSavedAt).toLocaleTimeString(locale, { hour: "2-digit", minute: "2-digit" }) })}</Text><Button type="primary" icon={<Save size={15} />} disabled={!dirty || !canEdit || validationIssues.length > 0} loading={saveMutation.isPending} onClick={requestSave}>{t("agents.common.saveDraft")}</Button></footer>
        </section>
        <AgentPreviewPanel key={agent.agent_id} agentId={agent.agent_id} agentName={agent.name} draftRevision={revision} versions={versions} savedSpec={savedSpec} dirty={dirty} />
      </div>

      <Drawer className="agent-section-drawer" title={t("agents.studio.drawerTitle")} placement="bottom" size="large" open={drawerOpen} onClose={() => setDrawerOpen(false)} afterOpenChange={(open) => { if (!open) mobileSectionTriggerRef.current?.focus(); }} extra={<Button type="text" icon={<X size={17} />} aria-label={t("agents.studio.closeSections")} onClick={() => setDrawerOpen(false)} />}>
        <SectionNav selected={section} onSelect={(key) => { selectSection(key); setDrawerOpen(false); }} />
      </Drawer>
    </main>
  );
}

function CapabilityGroup({ title, empty, children }: { title: string; empty: string; children: ReactNode }) {
  const hasChildren = Array.isArray(children) ? children.length > 0 : Boolean(children);
  return <section className="agent-capability-group"><Title level={4}>{title}</Title><div>{hasChildren ? children : <Text type="secondary">{empty}</Text>}</div></section>;
}

function KnowledgeControls({ disabled, binding, onChange }: { disabled: boolean; binding: AgentKnowledgeBinding; onChange: (binding: AgentKnowledgeBinding) => void }) {
  const { t } = useTranslation();
  const config = binding.retrieval_config;
  return <div className="agent-knowledge-controls"><Select disabled={disabled} aria-label={t("agents.studio.knowledge.retrievalMode")} value={config.mode || "auto"} options={[{ value: "auto", label: t("agents.studio.knowledge.auto") }, { value: "tool", label: t("agents.studio.knowledge.toolOnly") }, { value: "off", label: t("agents.studio.knowledge.off") }]} onChange={(mode) => onChange({ ...binding, retrieval_config: { ...config, mode } })} /><InputNumber disabled={disabled} aria-label={t("agents.studio.knowledge.topK")} min={1} max={20} value={config.top_k ?? 5} onChange={(topK) => onChange({ ...binding, retrieval_config: { ...config, top_k: topK ?? 5 } })} /><InputNumber disabled={disabled} aria-label={t("agents.studio.knowledge.threshold")} min={0} max={1} step={0.05} value={config.threshold ?? 0.4} onChange={(threshold) => onChange({ ...binding, retrieval_config: { ...config, threshold: threshold ?? 0.4 } })} /><Checkbox disabled={disabled} checked={Boolean(config.include_images)} onChange={(event) => onChange({ ...binding, retrieval_config: { ...config, include_images: event.target.checked } })}>{t("agents.studio.knowledge.includeImages")}</Checkbox></div>;
}

export function AgentStudioPage() {
  const { t } = useTranslation();
  const { agentId = "" } = useParams();
  const detail = useQuery({ queryKey: ["agent", agentId], queryFn: () => getAgent(agentId), enabled: Boolean(agentId), retry: false, staleTime: Number.POSITIVE_INFINITY });
  const draft = useQuery({ queryKey: ["agent", agentId, "draft"], queryFn: () => getAgentDraft(agentId), enabled: Boolean(agentId), retry: false, staleTime: Number.POSITIVE_INFINITY });
  const versions = useQuery({ queryKey: ["agent", agentId, "versions"], queryFn: () => listAgentVersions(agentId), enabled: Boolean(agentId), retry: false });
  const tools = useQuery({ queryKey: ["agent-catalog", "tools"], queryFn: listAgentTools, retry: false });
  const mcp = useQuery({ queryKey: ["agent-catalog", "mcp"], queryFn: listAgentMcpTools, retry: false });
  const skills = useQuery({ queryKey: ["agent-catalog", "skills"], queryFn: listAgentSkills, retry: false });
  const connectors = useQuery({ queryKey: ["agent-catalog", "connectors"], queryFn: listAgentConnectors, retry: false });
  const datasets = useQuery({ queryKey: ["agent-catalog", "datasets"], queryFn: listAgentDatasets, retry: false });
  const evalDatasets = useQuery({ queryKey: ["agent-catalog", "eval-datasets"], queryFn: listAgentEvalDatasets, retry: false });
  const models = useQuery({ queryKey: ["agent-catalog", "models"], queryFn: listAgentModels, retry: false });
  const primaryError = detail.error || draft.error || versions.error;
  const loading = detail.isLoading || draft.isLoading || versions.isLoading;
  const degraded = [tools.isError ? "tools" : null, mcp.isError ? "MCP" : null, skills.isError ? "Skills" : null, connectors.isError ? "Connectors" : null, datasets.isError ? "Knowledge" : null, evalDatasets.isError ? "Eval Datasets" : null, models.isError ? "models" : null].filter(Boolean) as string[];
  const retryCatalog = () => { void tools.refetch(); void mcp.refetch(); void skills.refetch(); void connectors.refetch(); void datasets.refetch(); void evalDatasets.refetch(); void models.refetch(); };

  if (loading) return <main className="agent-studio-loading" role="status"><Skeleton active paragraph={{ rows: 12 }} /></main>;
  if (primaryError || !detail.data || !draft.data) {
    const error = agentErrorDetail(primaryError);
    return <main className="agent-studio-error"><Alert type={error.status === 403 ? "warning" : "error"} showIcon title={error.status === 403 ? t("agents.studio.loadForbidden") : t("agents.studio.loadError")} description={error.message} action={<Button onClick={() => { void detail.refetch(); void draft.refetch(); void versions.refetch(); }}>{t("agents.common.retry")}</Button>} /></main>;
  }
  return <AgentStudioWorkspace key={detail.data.agent_id} initialAgent={detail.data} initialDraft={draft.data} versions={versions.data ?? []} catalog={{ tools: tools.data ?? [], mcp: mcp.data ?? [], skills: skills.data ?? [], connectors: connectors.data ?? [], datasets: datasets.data ?? [], evalDatasets: evalDatasets.data ?? [], models: models.data ?? [], degraded, retry: retryCatalog }} />;
}
