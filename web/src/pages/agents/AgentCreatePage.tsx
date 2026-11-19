import { useMemo, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import {
  Alert,
  Button,
  Checkbox,
  Form,
  Input,
  InputNumber,
  Select,
  Slider,
  Steps,
  Typography,
} from "antd";
import { ArrowLeft, Bot, LockKeyhole, Save } from "lucide-react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";

import {
  agentErrorDetail,
  createAgent,
  listAgentConnectors,
  listAgentDatasets,
  listAgentMcpTools,
  listAgentModels,
  listAgentSkills,
  listAgentTools,
} from "@/api/agents";
import {
  createDefaultAgentSpec,
  type AgentCapabilityBinding,
  type AgentSpec,
} from "@/types/agents";
import { capabilityDisplayName } from "./agentCatalogPresentation";
import "./agent-studio.css";

const { Paragraph, Text, Title } = Typography;

type AgentTemplateKey = "blank" | "support" | "knowledge";

interface AgentTemplate {
  title: string;
  description: string;
  instructions: string;
  welcomeMessage: string;
  suggestedPrompts: string[];
  detail: string;
  goal: string;
  triggers: string[];
  successCriteria: string[];
}

function getAgentTemplates(t: TFunction): Record<AgentTemplateKey, AgentTemplate> {
  return {
    blank: {
      title: t("agents.create.templates.blank.title"),
      description: t("agents.create.templates.blank.description"),
      detail: t("agents.create.templates.blank.detail"),
      instructions: t("agents.create.templates.blank.instructions"),
      welcomeMessage: "",
      suggestedPrompts: [],
      goal: "",
      triggers: [],
      successCriteria: [],
    },
    support: {
      title: t("agents.create.templates.support.title"),
      description: t("agents.create.templates.support.description"),
      detail: t("agents.create.templates.support.detail"),
      instructions: t("agents.create.templates.support.instructions"),
      welcomeMessage: t("agents.create.templates.support.welcome"),
      suggestedPrompts: [t("agents.create.templates.support.promptOne"), t("agents.create.templates.support.promptTwo")],
      goal: t("agents.create.templates.support.goal"),
      triggers: [t("agents.create.templates.support.trigger")],
      successCriteria: [t("agents.create.templates.support.success")],
    },
    knowledge: {
      title: t("agents.create.templates.knowledge.title"),
      description: t("agents.create.templates.knowledge.description"),
      detail: t("agents.create.templates.knowledge.detail"),
      instructions: t("agents.create.templates.knowledge.instructions"),
      welcomeMessage: t("agents.create.templates.knowledge.welcome"),
      suggestedPrompts: [t("agents.create.templates.knowledge.promptOne"), t("agents.create.templates.knowledge.promptTwo")],
      goal: t("agents.create.templates.knowledge.goal"),
      triggers: [t("agents.create.templates.knowledge.trigger")],
      successCriteria: [t("agents.create.templates.knowledge.success")],
    },
  };
}

function specForTemplate(template: AgentTemplate): AgentSpec {
  const spec = createDefaultAgentSpec();
  return {
    ...spec,
    identity: {
      ...spec.identity,
      welcome_message: template.welcomeMessage,
      suggested_prompts: [...template.suggestedPrompts],
    },
    outcome: {
      ...spec.outcome,
      goal: template.goal,
      triggers: [...template.triggers],
      success_criteria: [...template.successCriteria],
    },
    instructions: template.instructions,
  };
}

function lines(value: string, limit: number): string[] {
  return value.split("\n").map((item) => item.trim()).filter(Boolean).slice(0, limit);
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

function CatalogError({ label }: { label: string }) {
  const { t } = useTranslation();
  return <Text type="warning">{t("agents.create.catalogs.unavailable", { label })}</Text>;
}

export function AgentCreatePage() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const requestedTemplate = searchParams.get("template");
  const initialTemplate: AgentTemplateKey = requestedTemplate === "support" || requestedTemplate === "knowledge"
    ? requestedTemplate
    : "blank";
  const templates = getAgentTemplates(t);
  const [step, setStep] = useState(0);
  const [templateKey, setTemplateKey] = useState<AgentTemplateKey>(initialTemplate);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [spec, setSpec] = useState<AgentSpec>(() => specForTemplate(templates[initialTemplate]));
  const models = useQuery({ queryKey: ["agent-catalog", "models"], queryFn: listAgentModels, retry: false });
  const tools = useQuery({ queryKey: ["agent-catalog", "tools"], queryFn: listAgentTools, retry: false });
  const mcp = useQuery({ queryKey: ["agent-catalog", "mcp"], queryFn: listAgentMcpTools, retry: false });
  const skills = useQuery({ queryKey: ["agent-catalog", "skills"], queryFn: listAgentSkills, retry: false });
  const connectors = useQuery({ queryKey: ["agent-catalog", "connectors"], queryFn: listAgentConnectors, retry: false });
  const datasets = useQuery({ queryKey: ["agent-catalog", "datasets"], queryFn: listAgentDatasets, retry: false });
  // Empty id → the server applies its deployment default when the model is omitted.
  const fallbackModels = [{ id: "", name: t("agents.common.serverDefault"), provider: "dashscope" }];
  const modelRows = models.data?.length ? models.data : fallbackModels;
  const createMutation = useMutation({
    mutationFn: () => createAgent({ name: name.trim(), description: description.trim(), spec }),
    onSuccess: (agent) => navigate(`/agents/${agent.agent_id}`, { replace: true }),
  });
  const createError = createMutation.error ? agentErrorDetail(createMutation.error) : null;
  const identityValid = name.trim().length > 0 && name.trim().length <= 255 && description.length <= 4000 && isSafeIconUrl(spec.identity.icon_url);
  // An empty model_id is valid: the server applies its deployment default (DEFAULT_MODEL).
  const behaviorValid = Boolean(spec.instructions.trim());
  const canSubmit = identityValid && behaviorValid && !createMutation.isPending;

  const selectedKeys = useMemo(
    () => new Set(spec.capabilities.map((binding) => `${binding.type}:${binding.resource_id}`)),
    [spec.capabilities],
  );
  const toggleCapability = (binding: AgentCapabilityBinding, checked: boolean) => {
    setSpec((current) => ({
      ...current,
      capabilities: checked
        ? [...current.capabilities.filter((item) => !(item.type === binding.type && item.resource_id === binding.resource_id)), binding]
        : current.capabilities.filter((item) => !(item.type === binding.type && item.resource_id === binding.resource_id)),
    }));
  };

  const submit = () => {
    if (canSubmit) createMutation.mutate();
  };

  const applyTemplate = (nextTemplate: AgentTemplateKey) => {
    setTemplateKey(nextTemplate);
    setSpec(specForTemplate(templates[nextTemplate]));
  };

  return (
    <main className="agent-create-page" data-testid="agent-create-page">
      <div className="agent-create-heading">
        <Button type="text" icon={<ArrowLeft size={16} />} onClick={() => navigate("/agents")}>{t("agents.list.title")}</Button>
        <div>
          <Title level={2}>{t("agents.create.title")}</Title>
          <Paragraph type="secondary">{t("agents.create.subtitle")}</Paragraph>
        </div>
        <Button type="text" onClick={() => navigate("/agents")}>{t("agents.common.cancel")}</Button>
      </div>

      <Steps
        className="agent-create-steps"
        current={step}
        items={[
          { title: t("agents.create.steps.identity"), content: t("agents.create.steps.identityContent") },
          { title: t("agents.create.steps.behavior"), content: t("agents.create.steps.behaviorContent") },
          { title: t("agents.create.steps.start"), content: t("agents.create.steps.startContent") },
        ]}
      />

      {createError && <Alert type="error" showIcon title={t("agents.create.errorTitle")} description={createError.message} />}

      <section className="agent-create-panel" aria-labelledby={`create-step-${step}`}>
        {step === 0 && (
          <div className="agent-create-form">
            <div className="agent-step-title"><span className="agent-avatar"><Bot size={18} /></span><div><Title id="create-step-0" level={3}>{t("agents.create.steps.identity")}</Title><Text type="secondary">{t("agents.create.identityDescription")}</Text></div></div>
            <div className="agent-template-grid" role="list" aria-label={t("agents.create.templatesLabel")}>
              {(Object.entries(templates) as Array<[AgentTemplateKey, AgentTemplate]>).map(([key, template]) => (
                <div key={key} role="listitem">
                  <button type="button" aria-pressed={templateKey === key} className={templateKey === key ? "is-selected" : ""} onClick={() => applyTemplate(key)}>
                    <strong>{template.title}</strong>
                    <span>{template.description}</span>
                    <small>{template.detail}</small>
                  </button>
                </div>
              ))}
            </div>
            <Alert type="info" showIcon title={t("agents.create.templateSafetyTitle")} description={t("agents.create.templateSafetyDescription")} />
            <Form layout="vertical" requiredMark="optional">
              <Form.Item
                label={t("agents.create.name")}
                required
                validateStatus={name.length > 255 ? "error" : undefined}
                help={<span id="create-agent-name-help">{name.length > 255 ? t("agents.create.nameError") : t("agents.create.characterCount", { count: name.length, limit: 255 })}</span>}
              >
                <Input id="create-agent-name" aria-label={t("agents.create.name")} aria-describedby="create-agent-name-help" aria-invalid={name.length > 255} value={name} onChange={(event) => setName(event.target.value)} placeholder={t("agents.create.namePlaceholder")} autoFocus aria-required="true" />
              </Form.Item>
              <Form.Item
                label={t("agents.create.description")}
                validateStatus={description.length > 4000 ? "error" : undefined}
                help={<span id="create-agent-description-help">{t("agents.create.characterCount", { count: description.length, limit: 4000 })}</span>}
              >
                <Input.TextArea aria-label={t("agents.create.description")} aria-describedby="create-agent-description-help" aria-invalid={description.length > 4000} value={description} onChange={(event) => setDescription(event.target.value)} rows={5} placeholder={t("agents.create.descriptionPlaceholder")} />
              </Form.Item>
              <Form.Item label={t("agents.create.welcomeMessage")} help={t("agents.create.welcomeHelp")}>
                <Input.TextArea
                  aria-label={t("agents.create.welcomeMessage")}
                  value={spec.identity.welcome_message}
                  onChange={(event) => setSpec((current) => ({ ...current, identity: { ...current.identity, welcome_message: event.target.value } }))}
                  rows={3}
                  placeholder={t("agents.create.welcomePlaceholder")}
                />
              </Form.Item>
              <Form.Item label={t("agents.create.iconUrl")} validateStatus={!isSafeIconUrl(spec.identity.icon_url) ? "error" : undefined} help={!isSafeIconUrl(spec.identity.icon_url) ? t("agents.create.iconError") : t("agents.create.iconHelp")}>
                <Input type="url" aria-label={t("agents.create.iconUrl")} aria-invalid={!isSafeIconUrl(spec.identity.icon_url)} value={spec.identity.icon_url || ""} maxLength={2048} onChange={(event) => setSpec((current) => ({ ...current, identity: { ...current.identity, icon_url: event.target.value || null } }))} placeholder="https://example.com/agent-icon.png" />
              </Form.Item>
            </Form>
          </div>
        )}

        {step === 1 && (
          <div className="agent-create-form">
            <div className="agent-step-title"><span className="agent-step-number">2</span><div><Title id="create-step-1" level={3}>{t("agents.create.steps.behavior")}</Title><Text type="secondary">{t("agents.create.behaviorDescription")}</Text></div></div>
            {models.isError && <Alert type="warning" showIcon title={t("agents.create.modelCatalogError")} description={t("agents.create.modelCatalogFallback")} action={<Button onClick={() => void models.refetch()}>{t("agents.common.retry")}</Button>} />}
            <Form layout="vertical">
              <Alert type="info" showIcon title={t("agents.create.instructionsSafetyTitle")} description={t("agents.create.instructionsSafetyDescription")} />
              <Form.Item label={t("agents.create.instructions")} required validateStatus={!spec.instructions.trim() ? "error" : undefined} help={!spec.instructions.trim() ? t("agents.create.instructionsRequired") : t("agents.create.characterCount", { count: spec.instructions.length, limit: 100000 })}>
                <Input.TextArea aria-label={t("agents.create.instructions")} aria-invalid={!spec.instructions.trim()} value={spec.instructions} rows={8} maxLength={100000} onChange={(event) => setSpec((current) => ({ ...current, instructions: event.target.value }))} placeholder={t("agents.create.instructionsPlaceholder")} />
              </Form.Item>
              <div className="agent-form-grid">
                <Form.Item label={t("agents.create.outcome.goal")} help={t("agents.create.outcome.goalHelp")}>
                  <Input.TextArea aria-label={t("agents.create.outcome.goal")} value={spec.outcome.goal} rows={3} maxLength={4000} onChange={(event) => setSpec((current) => ({ ...current, outcome: { ...current.outcome, goal: event.target.value } }))} placeholder={t("agents.create.outcome.goalPlaceholder")} />
                </Form.Item>
                <Form.Item label={t("agents.create.outcome.successCriteria")} help={t("agents.create.outcome.onePerLine")}>
                  <Input.TextArea aria-label={t("agents.create.outcome.successCriteria")} value={spec.outcome.success_criteria.join("\n")} rows={3} onChange={(event) => setSpec((current) => ({ ...current, outcome: { ...current.outcome, success_criteria: lines(event.target.value, 32) } }))} placeholder={t("agents.create.outcome.successCriteriaPlaceholder")} />
                </Form.Item>
              </div>
              <Form.Item label={t("agents.create.outcome.triggers")} help={t("agents.create.outcome.onePerLine")}>
                <Input.TextArea aria-label={t("agents.create.outcome.triggers")} value={spec.outcome.triggers.join("\n")} rows={2} onChange={(event) => setSpec((current) => ({ ...current, outcome: { ...current.outcome, triggers: lines(event.target.value, 16) } }))} placeholder={t("agents.create.outcome.triggersPlaceholder")} />
              </Form.Item>
              <div className="agent-form-grid">
                <Form.Item label={t("agents.create.outcome.inputs")} help={t("agents.create.outcome.onePerLine")}>
                  <Input.TextArea aria-label={t("agents.create.outcome.inputs")} value={spec.outcome.inputs.join("\n")} rows={2} onChange={(event) => setSpec((current) => ({ ...current, outcome: { ...current.outcome, inputs: lines(event.target.value, 32) } }))} placeholder={t("agents.create.outcome.inputsPlaceholder")} />
                </Form.Item>
                <Form.Item label={t("agents.create.outcome.humanBoundaries")} help={t("agents.create.outcome.onePerLine")}>
                  <Input.TextArea aria-label={t("agents.create.outcome.humanBoundaries")} value={spec.outcome.human_boundaries.join("\n")} rows={2} onChange={(event) => setSpec((current) => ({ ...current, outcome: { ...current.outcome, human_boundaries: lines(event.target.value, 32) } }))} placeholder={t("agents.create.outcome.humanBoundariesPlaceholder")} />
                </Form.Item>
              </div>
              <div className="agent-form-grid">
                <Form.Item label={t("agents.create.provider")} required>
                  <Select aria-label={t("agents.create.provider")} value={spec.model.provider_id || undefined} options={Array.from(new Set(modelRows.map((model) => model.provider))).map((provider) => ({ value: provider, label: provider === "dashscope" ? "DashScope" : provider }))} onChange={(value) => setSpec((current) => ({ ...current, model: { ...current.model, provider_id: value, model_id: "" } }))} />
                </Form.Item>
                <Form.Item label={t("agents.create.model")} required>
                  <Select aria-label={t("agents.create.model")} showSearch value={spec.model.model_id || undefined} options={modelRows.filter((model) => model.provider === spec.model.provider_id).map((model) => ({ value: model.id, label: model.name || model.id }))} onChange={(value) => setSpec((current) => ({ ...current, model: { ...current.model, model_id: value } }))} />
                </Form.Item>
              </div>
              <Form.Item label={t("agents.create.temperature", { value: spec.model.temperature ?? 0.3 })}>
                <Slider aria-label={t("agents.create.temperature", { value: spec.model.temperature ?? 0.3 })} min={0} max={2} step={0.1} value={spec.model.temperature ?? 0.3} onChange={(value) => setSpec((current) => ({ ...current, model: { ...current.model, temperature: value } }))} />
              </Form.Item>
              <Form.Item label={t("agents.create.maxOutputTokens")}>
                <InputNumber aria-label={t("agents.create.maxOutputTokens")} min={1} max={1_000_000} value={spec.model.max_tokens ?? 4096} onChange={(value) => setSpec((current) => ({ ...current, model: { ...current.model, max_tokens: value ?? 4096 } }))} />
              </Form.Item>
              <Alert type="info" showIcon title={t("agents.create.modelNote")} />
            </Form>
          </div>
        )}

        {step === 2 && (
          <div className="agent-create-form">
            <div className="agent-step-title"><span className="agent-step-number">3</span><div><Title id="create-step-2" level={3}>{t("agents.create.steps.start")}</Title><Text type="secondary">{t("agents.create.startDescription")}</Text></div></div>
            <div className="agent-catalog-groups">
              <CatalogGroup title={t("agents.create.catalogs.platformTools")} error={tools.isError ? <CatalogError label={t("agents.create.catalogs.platformTools")} /> : null}>
                {(tools.data ?? []).map((tool) => (
                  <Checkbox key={tool.id} checked={selectedKeys.has(`native:${tool.id}`)} onChange={(event) => toggleCapability({ type: "native", resource_id: tool.id, config: { risk: tool.risk } }, event.target.checked)}>
                    <strong>{capabilityDisplayName(tool.name, tool.title)}</strong><span>{tool.summary || tool.description}</span>
                  </Checkbox>
                ))}
              </CatalogGroup>
              <CatalogGroup title={t("agents.create.catalogs.mcpTools")} error={mcp.isError ? <CatalogError label={t("agents.create.catalogs.mcpTools")} /> : null}>
                {(mcp.data ?? []).map((tool) => (
                  <Checkbox key={tool.tool_id} disabled={!tool.connection_id || !tool.enabled} checked={selectedKeys.has(`mcp:${tool.runtime_name}`)} onChange={(event) => toggleCapability({ type: "mcp", resource_id: tool.runtime_name, resource_version: tool.snapshot_id, schema_hash: tool.schema_hash, config: { connection_id: tool.connection_id, principal_type: tool.principal_type, risk: tool.risk_level } }, event.target.checked)}>
                    <strong>{capabilityDisplayName(tool.runtime_name)}</strong><span>{tool.server_name} · {tool.connection_id ? tool.description : t("agents.create.catalogs.connectCredential")}</span>
                  </Checkbox>
                ))}
              </CatalogGroup>
              <CatalogGroup title={t("agents.create.catalogs.skills")} error={skills.isError ? <CatalogError label={t("agents.create.catalogs.skills")} /> : null}>
                {(skills.data ?? []).map((skill) => {
                  const versionId = skill.version_id || "";
                  return <Checkbox key={`${skill.name}:${versionId}`} disabled={!versionId} checked={selectedKeys.has(`skill:${versionId}`)} onChange={(event) => toggleCapability({ type: "skill", resource_id: versionId, resource_version: versionId, schema_hash: skill.content_hash, config: {} }, event.target.checked)}><strong>{skill.title || skill.name}</strong><span>{skill.description || (versionId ? t("agents.create.catalogs.skillVersion", { version: skill.version || t("agents.studio.capabilities.current") }) : t("agents.create.catalogs.skillNoVersion"))}</span></Checkbox>;
                })}
              </CatalogGroup>
              <CatalogGroup title={t("agents.create.catalogs.connectors")} error={connectors.isError ? <CatalogError label={t("agents.create.catalogs.connectors")} /> : null}>
                {(connectors.data ?? []).map((connector) => {
                  const canBind = connector.provider === "confluence" && Boolean(connector.grant_id && connector.principal_type);
                  return <Checkbox key={connector.provider} disabled={!canBind} checked={selectedKeys.has("connector:confluence_read")} onChange={(event) => toggleCapability({ type: "connector", resource_id: "confluence_read", config: { provider: connector.provider, principal_type: connector.principal_type, grant_id: connector.grant_id, tool_name: "confluence_read", risk: "low" } }, event.target.checked)}><strong>{connector.display_name}</strong><span>{canBind ? t("agents.create.catalogs.connectorReady") : t("agents.create.catalogs.connectorSetup")}</span></Checkbox>;
                })}
              </CatalogGroup>
              <CatalogGroup title={t("agents.create.catalogs.knowledge")} error={datasets.isError ? <CatalogError label={t("agents.create.catalogs.knowledge")} /> : null}>
                {(datasets.data ?? []).map((dataset) => (
                  <Checkbox key={dataset.dataset_id} checked={spec.knowledge.some((binding) => binding.dataset_id === dataset.dataset_id)} onChange={(event) => setSpec((current) => ({ ...current, knowledge: event.target.checked ? [...current.knowledge, { dataset_id: dataset.dataset_id, retrieval_config: { mode: "auto", top_k: 5, threshold: 0.4, include_images: false } }] : current.knowledge.filter((binding) => binding.dataset_id !== dataset.dataset_id) }))}><strong>{dataset.name}</strong><span>{dataset.description || t("agents.create.catalogs.workspaceDataset")}</span></Checkbox>
                ))}
              </CatalogGroup>
            </div>
          </div>
        )}
      </section>

      <div className="agent-create-actions">
        <span><LockKeyhole size={14} /> {t("agents.create.nothingPublished")}</span>
        <div>
          <Button disabled={step === 0} onClick={() => setStep((value) => Math.max(0, value - 1))}>{t("agents.common.back")}</Button>
          <Button icon={<Save size={15} />} disabled={!canSubmit} loading={createMutation.isPending} onClick={submit}>{t("agents.create.saveAsDraft")}</Button>
          {step < 2 ? (
            <Button type="primary" disabled={step === 0 ? !identityValid : !behaviorValid} onClick={() => setStep((value) => Math.min(2, value + 1))}>{t("agents.common.continue")}</Button>
          ) : (
            <Button type="primary" disabled={!canSubmit} loading={createMutation.isPending} onClick={submit}>{t("agents.common.createAgent")}</Button>
          )}
        </div>
      </div>
    </main>
  );
}

function CatalogGroup({ title, error, children }: { title: string; error: React.ReactNode; children: React.ReactNode }) {
  return (
    <section className="agent-catalog-group" aria-label={title}>
      <div className="agent-catalog-heading"><Title level={4}>{title}</Title>{error}</div>
      <div className="agent-catalog-options">{children}</div>
    </section>
  );
}
