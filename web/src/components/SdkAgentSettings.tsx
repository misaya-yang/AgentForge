import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { getService, updateService } from "@/api/gateway";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { buildSdkAgentSettingsPatch, readSdkAgentSettings, sdkAgentSettingsMatch, type SavedSdkAgentSettings, type SdkAgentSettings as Settings } from "@/lib/sdkAgentSettings";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";

/** This editor targets the native Gateway's write-only official SDK service contract. */
export function SdkAgentSettings({ serviceId }: { serviceId: string }) {
  const token = useAuthStore(state => state.token);
  const authority = useAuthStore(state => sdkAuthorityScope(state.user));
  const canEdit = useAuthStore(state => state.hasPermission("console:services:edit"));
  // Never place a credential in a React key. Remount before exposing another authority's draft.
  const visit = useRef({ serviceId, token, authority, epoch: 0 });
  if (visit.current.serviceId !== serviceId || visit.current.token !== token || visit.current.authority !== authority) {
    visit.current = { serviceId, token, authority, epoch: visit.current.epoch + 1 };
  }
  return <SdkAgentSettingsFields key={visit.current.epoch} serviceId={serviceId} canEdit={canEdit} />;
}

export function SdkAgentSettingsFields({ serviceId, canEdit }: { serviceId: string; canEdit: boolean }) {
  const { t } = useTranslation();
  const [saved, setSaved] = useState<SavedSdkAgentSettings | null>(null);
  const [draft, setDraft] = useState<Settings>({ model: "", instructions: "", max_turns: 8 });
  const [replacementKey, setReplacementKey] = useState("");
  const [status, setStatus] = useState<"loading" | "ready" | "saving" | "saved" | "unconfirmed" | "failed">("loading");
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(false);
  const pending = useRef(false);
  const generation = useRef(0);
  const owner = useRef({ token: useAuthStore.getState().token, authority: sdkAuthorityScope(useAuthStore.getState().user) });
  const isCurrent = () => mounted.current && owner.current.token === useAuthStore.getState().token &&
    owner.current.authority === sdkAuthorityScope(useAuthStore.getState().user);

  async function reload() {
    if (pending.current || !isCurrent()) return;
    const epoch = ++generation.current;
    setStatus("loading"); setError(null); setReplacementKey("");
    try {
      const next = readSdkAgentSettings(await getService(serviceId), serviceId);
      if (!isCurrent() || generation.current !== epoch) return;
      setSaved(next); setDraft({ ...next.settings }); setStatus("ready");
    } catch {
      if (isCurrent() && generation.current === epoch) {
        setStatus("failed"); setError(t("services.sdkSettings.readFailed", "Couldn't read the saved SDK settings. Reload before editing."));
      }
    }
  }
  useEffect(() => {
    mounted.current = true;
    void reload();
    return () => { mounted.current = false; generation.current += 1; };
    // This component is remounted for service and authority changes by its boundary above.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function save(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (pending.current || !isCurrent() || !canEdit || !saved || (status !== "ready" && status !== "saved")) return;
    let patch: Record<string, unknown>;
    try { patch = buildSdkAgentSettingsPatch(saved, draft, replacementKey); }
    catch {
      setError(t("services.sdkSettings.invalid", "Check the model, instructions, turn limit and replacement key. No changes were sent.")); return;
    }
    pending.current = true;
    const epoch = ++generation.current;
    const submitted = { ...draft };
    setReplacementKey(""); setError(null); setStatus("saving");
    const current = () => isCurrent() && generation.current === epoch;
    let acknowledged = false;
    try {
      await updateService(serviceId, patch);
      acknowledged = true;
      if (!current()) return;
      const observed = readSdkAgentSettings(await getService(serviceId), serviceId);
      if (!current()) return;
      if (observed.framework !== saved.framework || !sdkAgentSettingsMatch(observed, submitted)) {
        setStatus("unconfirmed"); setError(t("services.sdkSettings.mismatch", "The saved settings differ from this submission. Reload and review before making another change.")); return;
      }
      setSaved(observed); setDraft({ ...observed.settings }); setStatus("saved");
    } catch {
      if (current()) {
        setStatus("unconfirmed");
        setError(acknowledged
          ? t("services.sdkSettings.readbackFailed", "The update was accepted, but its saved settings couldn't be read back. Reload to confirm. Do not resend the key automatically.")
          : t("services.sdkSettings.writeUnknown", "The update wasn't confirmed. It may have been saved. Reload and review before trying again; the replacement key has been cleared."));
      }
    } finally { pending.current = false; }
  }
  const disabled = !canEdit || status === "loading" || status === "saving" || status === "failed" || status === "unconfirmed";
  const change = (value: Partial<Settings>) => { setDraft(previous => ({ ...previous, ...value })); setStatus("ready"); setError(null); };
  return <section className="rounded-lg border p-4 space-y-3" aria-label={t("services.sdkSettings.title", "Official SDK agent settings")}>
    <h3 className="font-medium">{t("services.sdkSettings.title", "Official SDK agent settings")}</h3>
    <p className="text-xs text-muted-foreground">{t("services.sdkSettings.boundary", "Native Gateway SDK services only. Changes affect subsequent admissions; existing runs keep their admitted configuration. Saving doesn't test provider availability.")}</p>
    {saved && <p className="text-sm">{saved.framework === "openai_agents" ? "OpenAI Agents SDK" : "Claude Agent SDK (Anthropic)"} · {t("services.sdkSettings.frameworkFixed", "Framework is fixed for this editor")}</p>}
    {saved && <p className="text-xs text-muted-foreground">{t("services.sdkSettings.savedValues", "Last read from server")}: {saved.settings.model} · {saved.settings.max_turns} {t("services.sdkSettings.turns", "turns")} · {saved.keyConfigured ? t("services.sdkSettings.keyConfigured", "Provider key configured") : t("services.sdkSettings.keyUnknown", "Provider key configuration not confirmed")}</p>}
    {status === "loading" && <p role="status">{t("common.loading")}</p>}
    <form onSubmit={save} autoComplete="off" className="space-y-3" aria-busy={status === "saving"}>
      <fieldset disabled={disabled} className="space-y-3">
        <legend className="text-sm font-medium">{t("services.sdkSettings.draft", "Editable draft")}</legend>
        <div><Label htmlFor="sdk-edit-model">{t("services.sdkAgent.model")}</Label><Input id="sdk-edit-model" value={draft.model} onChange={event => change({ model: event.target.value })} required /></div>
        <div><Label htmlFor="sdk-edit-instructions">{t("services.sdkAgent.instructions")}</Label><Textarea id="sdk-edit-instructions" value={draft.instructions} onChange={event => change({ instructions: event.target.value })} rows={4} /></div>
        <p className="text-xs text-muted-foreground">{t("services.sdkSettings.instructionsDefault", "Blank instructions use the server's default: You are a helpful assistant.")}</p>
        <div><Label htmlFor="sdk-edit-turns">{t("services.sdkAgent.maxTurns")}</Label><Input id="sdk-edit-turns" type="number" min={1} max={32} step={1} value={draft.max_turns} onChange={event => change({ max_turns: Number(event.target.value) })} required /></div>
        <div><Label htmlFor="sdk-edit-key">{t("services.sdkSettings.replaceKey", "Replacement provider key (optional)")}</Label><Input id="sdk-edit-key" type="password" autoComplete="new-password" spellCheck={false} autoCapitalize="none" value={replacementKey} onChange={event => { setReplacementKey(event.target.value); setStatus("ready"); }} aria-describedby="sdk-edit-key-hint" /></div>
        <p id="sdk-edit-key-hint" className="text-xs text-muted-foreground">{t("services.sdkSettings.keyHint", "Leave blank to keep the server-side key. Existing keys are never loaded. A replacement is sent only when you save and is then cleared from this form.")}</p>
      </fieldset>
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      {status === "saved" && <p role="status">{t("services.sdkSettings.confirmed", "Settings accepted and read back. Provider key values remain write-only; this is not a connection test.")}</p>}
      {!canEdit && <p>{t("services.sdkSettings.readOnly", "You need service-edit permission to save these settings.")}</p>}
      <div className="flex flex-wrap gap-2">
        <Button type="submit" disabled={disabled || !saved}>{status === "saving" ? t("services.sdkSettings.saving", "Saving and confirming…") : t("services.sdkSettings.save", "Save SDK settings")}</Button>
        <Button type="button" variant="outline" disabled={status === "saving" || status === "loading"} onClick={() => void reload()}>{t("services.sdkSettings.reload", "Reload saved settings (discard draft)")}</Button>
      </div>
    </form>
  </section>;
}
