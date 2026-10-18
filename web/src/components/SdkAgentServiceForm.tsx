import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { SdkCapabilityPicker } from "@/components/SdkCapabilityPicker";
import type { SdkCapabilityOption } from "@/lib/sdkCapabilitySelection";
import { SdkKnowledgePicker } from "@/components/SdkKnowledgePicker";
import { useAuthStore } from "@/store/useAuthStore";
import type { SdkKnowledgeOption } from "@/lib/sdkKnowledgeSelection";
import { registerSdkService } from "@/api/gateway";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { buildSdkAgentDefinition, type AgentSdk } from "@/lib/sdkAgentConfig";

interface SdkAgentServiceFormProps {
  onRegistered: () => void;
  onCancel: () => void;
  onBusyChange: (busy: boolean) => void;
}

export function SdkAgentServiceForm(props: SdkAgentServiceFormProps) {
  const user = useAuthStore((state) => state.user);
  // Revoke transient keys/catalog/selections when account or authorization changes.
  const scope = JSON.stringify([user?.user_id, user?.permissions, user?.roles, user?.tier]);
  return <SdkAgentServiceFields key={scope} {...props} />;
}

function SdkAgentServiceFields({ onRegistered, onCancel, onBusyChange }: SdkAgentServiceFormProps) {
  const { t } = useTranslation();
  const [sdk, setSdk] = useState<AgentSdk>("openai_agents");
  const [serviceId, setServiceId] = useState("");
  const [name, setName] = useState("");
  const [model, setModel] = useState("");
  const [instructions, setInstructions] = useState("");
  const [maxTurns, setMaxTurns] = useState("8");
  const [apiKey, setApiKey] = useState("");
  const [saving, setSaving] = useState(false);
  const pending = useRef(false);
  const [submitted, setSubmitted] = useState(false);
  const canViewKnowledge = useAuthStore((state) => state.hasPermission("knowledge:dataset:view"));
  const [knowledgeDatasetIds, setKnowledgeDatasetIds] = useState<string[]>([]);
  const [visibleDatasets, setVisibleDatasets] = useState<SdkKnowledgeOption[]>([]);
  const [capabilityIds, setCapabilityIds] = useState<string[]>([]);
  const [visibleCapabilities, setVisibleCapabilities] = useState<SdkCapabilityOption[]>([]);
  const active = useRef(true);
  useEffect(() => {
    active.current = true;
    return () => { active.current = false; onBusyChange(false); };
  }, [onBusyChange]);
  const [error, setError] = useState<string | null>(null);

  async function submit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (pending.current || submitted) return;
    setError(null);
    let definition: ReturnType<typeof buildSdkAgentDefinition>;
    try {
      definition = buildSdkAgentDefinition({
        sdk, serviceId, name, model, instructions, maxTurns: Number(maxTurns), apiKey,
        knowledgeDatasetIds, capabilityIds,
      }, visibleDatasets, visibleCapabilities);
    } catch (error) {
      const key = error instanceof Error && error.message === "invalid_sdk_knowledge_selection"
        ? "services.sdkAgent.knowledge.invalid"
        : error instanceof Error && error.message === "invalid_sdk_capability_selection"
          ? "services.sdkAgent.capabilities.invalid" : "services.sdkAgent.invalidFields";
      setError(t(key));
      return;
    }
    pending.current = true;
    setSaving(true);
    onBusyChange(true);
    // Never cache the provider key or echo a transport error containing it.
    setApiKey("");
    try {
      setSubmitted(true);
      await registerSdkService(definition);
      if (!active.current) return;
      onBusyChange(false);
      onRegistered();
    } catch (error) {
      const reason = error instanceof Error ? error.message : "";
      const key = reason === "sdk_registration_unavailable" ? "registrationUnavailable"
        : reason === "sdk_registration_conflict" ? "registrationConflict" : "registerFailed";
      if (active.current) setError(t(`services.sdkAgent.${key}`, { serviceId: serviceId.trim() }));
    } finally {
      // An attempted registration is never replayed from this form.
      // Keep the ref fenced even before the submitted state is rendered.
      if (active.current) { setSaving(false); onBusyChange(false); }
    }
  }

  return (
    <form onSubmit={submit} className="space-y-4" autoComplete="off" aria-busy={saving}>
      <p className="text-sm text-muted-foreground">{t("services.sdkAgent.description")}</p>
      <p id="sdk-runtime-hint" className="rounded-lg border bg-muted/30 p-3 text-xs text-muted-foreground">{t("services.sdkAgent.runtimeHint")}</p>
      <fieldset disabled={saving || submitted} className="space-y-2">
        <legend className="mb-2 text-sm font-medium">{t("services.sdkAgent.ecosystem")}</legend>
        <div className="grid gap-2 sm:grid-cols-2">
          {([ ["openai_agents", "OpenAI Agents SDK"], ["claude_agent_sdk", "Claude Agent SDK (Anthropic)"] ] as const).map(([value, label]) => (
            <label key={value} className={`flex cursor-pointer items-center gap-2 rounded-lg border p-3 text-sm ${sdk === value ? "border-primary bg-primary/5" : "border-border"}`}>
              <input type="radio" name="agent-sdk" value={value} checked={sdk === value} onChange={() => {
                setSdk(value); setApiKey(""); setModel(""); setError(null);
              }} className="accent-primary" />
              {label}
            </label>
          ))}
        </div>
      </fieldset>
      <div className="grid gap-4 sm:grid-cols-2">
        <div className="space-y-2">
          <Label htmlFor="sdk-service-id">{t("services.sdkAgent.serviceId")}</Label>
          <Input id="sdk-service-id" value={serviceId} onChange={(e) => setServiceId(e.target.value)} placeholder="my-sdk-agent" pattern="[a-z0-9][a-z0-9_-]*" maxLength={128} required disabled={saving || submitted} />
        </div>
        <div className="space-y-2">
          <Label htmlFor="sdk-service-name">{t("services.sdkAgent.name")}</Label>
          <Input id="sdk-service-name" value={name} onChange={(e) => setName(e.target.value)} required disabled={saving || submitted} />
        </div>
        <div className="space-y-2">
          <Label htmlFor="sdk-model">{t("services.sdkAgent.model")}</Label>
          <Input id="sdk-model" value={model} onChange={(e) => setModel(e.target.value)} aria-describedby="sdk-model-hint" required disabled={saving || submitted} />
          <p id="sdk-model-hint" className="text-xs text-muted-foreground">{t("services.sdkAgent.modelHint")}</p>
        </div>
        <div className="space-y-2">
          <Label htmlFor="sdk-max-turns">{t("services.sdkAgent.maxTurns")}</Label>
          <Input id="sdk-max-turns" type="number" min={1} max={32} step={1} value={maxTurns} onChange={(e) => setMaxTurns(e.target.value)} required disabled={saving || submitted} />
        </div>
      </div>
      <div className="space-y-2">
        <Label htmlFor="sdk-instructions">{t("services.sdkAgent.instructions")}</Label>
        <Textarea id="sdk-instructions" value={instructions} onChange={(e) => setInstructions(e.target.value)} rows={3} disabled={saving || submitted} />
      </div>
      <div className="space-y-2">
        <Label htmlFor="sdk-provider-key">{t("services.sdkAgent.apiKey")}</Label>
        <Input id="sdk-provider-key" type="password" autoComplete="new-password" spellCheck={false} autoCapitalize="none" value={apiKey} onChange={(e) => setApiKey(e.target.value)} aria-describedby="sdk-runtime-hint sdk-key-hint" required disabled={saving || submitted} />
        <p id="sdk-key-hint" className="text-xs text-muted-foreground">{t("services.sdkAgent.keyHint")}</p>
      </div>
      <SdkCapabilityPicker disabled={saving || submitted} canView={true}
        selected={capabilityIds} onChange={setCapabilityIds} onCatalogChange={setVisibleCapabilities} />
      <SdkKnowledgePicker disabled={saving || submitted} canView={canViewKnowledge}
        selected={knowledgeDatasetIds} onChange={setKnowledgeDatasetIds} onCatalogChange={setVisibleDatasets} />
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      <div className="flex justify-end gap-2">
        <Button type="button" variant="outline" onClick={onCancel} disabled={saving}>{t("common.cancel")}</Button>
        <Button type="submit" variant="primary" disabled={saving || submitted}>{saving ? t("services.registering") : t("services.register")}</Button>
      </div>
    </form>
  );
}
