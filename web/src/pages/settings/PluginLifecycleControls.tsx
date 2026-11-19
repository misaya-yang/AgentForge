import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { PluginRegistryEntry, UpdatePluginLifecycle } from "@/api/plugins";
import type { PluginLifecycleMutationState } from "./usePluginLifecycleRecovery";
import { PluginImpactReview } from "./PluginImpactReview";
import { pluginLifecycleReviewKey } from "./pluginLifecycleImpact";
import { Button } from "@/components/ui/button";

interface Props {
  plugin: PluginRegistryEntry;
  tenantId: string;
  catalogRevision: string;
  canManage: boolean;
  stale: boolean;
  mutation?: PluginLifecycleMutationState;
  onSave: (payload: UpdatePluginLifecycle, reviewedKey: string) => Promise<boolean>;
}

export function PluginLifecycleControls({ plugin, tenantId, catalogRevision, canManage, stale, mutation, onSave }: Props) {
  const { t } = useTranslation();
  const lifecycle = plugin.lifecycle;
  const [version, setVersion] = useState(lifecycle.selected_version ?? plugin.versions[0]?.version ?? "");
  const [enabled, setEnabled] = useState(lifecycle.enabled);
  const [reviewedKey, setReviewedKey] = useState<string | null>(null);
  const [acknowledgedKey, setAcknowledgedKey] = useState<string | null>(null);
  const reviewKey = pluginLifecycleReviewKey(plugin, version, enabled, tenantId, catalogRevision);
  const reviewing = reviewedKey === reviewKey;
  const acknowledged = reviewing && acknowledgedKey === reviewKey;
  const consent = useRef<string | null>(null);
  const resetReview = () => { consent.current = null; setReviewedKey(null); setAcknowledgedKey(null); };
  useEffect(() => { consent.current = null; setReviewedKey(null); setAcknowledgedKey(null); }, [reviewKey]);
  const pending = mutation?.pending ?? false;
  const message = mutation?.message;
  const failed = mutation?.failed ?? false;
  const needsRefresh = Boolean(mutation);
  const inFlight = useRef(false);
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; consent.current = null; }; }, []);
  const present = plugin.versions.some(item => item.version === version);
  const dirty = !lifecycle.managed || enabled !== lifecycle.enabled || version !== lifecycle.selected_version
    || lifecycle.selection_status === "catalog_changed";
  const blocked = pending || stale || needsRefresh || !version || (enabled && !present);

  const save = async () => {
    if (!mounted.current || inFlight.current || blocked || !acknowledged || consent.current !== reviewKey || !canManage) return;
    inFlight.current = true;
    try {
      const confirmed = await onSave({
        selected_version: version,
        enabled,
        expected_revision: lifecycle.revision,
        expected_catalog_revision: catalogRevision,
        expected_tenant_id: tenantId,
      }, reviewKey);
      if (confirmed && mounted.current) resetReview();
    } finally {
      inFlight.current = false;
    }
  };

  return <section aria-label={t("pluginRegistry.lifecycle.title")} className="space-y-3 border-b border-border bg-muted/20 p-4 sm:px-5">
    <div className="flex flex-wrap items-center gap-x-5 gap-y-2 text-xs">
      <span>{t("pluginRegistry.lifecycle.configuration")}: {t(lifecycle.managed
        ? lifecycle.enabled ? "pluginRegistry.lifecycle.enabled" : "pluginRegistry.lifecycle.disabled"
        : "pluginRegistry.lifecycle.default")}</span>
      <span>{t("pluginRegistry.lifecycle.selection")}: {lifecycle.selected_version ?? t(lifecycle.managed ? "pluginRegistry.lifecycle.hiddenSelection" : "pluginRegistry.lifecycle.default")}</span>
      <span>{t(`pluginRegistry.lifecycle.status.${lifecycle.selection_status}`)}</span>
    </div>
    {lifecycle.updated_at && <p className="text-xs text-muted-foreground">{t("pluginRegistry.lifecycle.updated", { date: new Date(lifecycle.updated_at).toLocaleString(), revision: lifecycle.revision })}</p>}
    {canManage ? <form onSubmit={event => { event.preventDefault(); if (!blocked && dirty) { consent.current = null; setAcknowledgedKey(null); setReviewedKey(reviewKey); } }} className="space-y-3">
      <div className="flex flex-wrap items-end gap-3">
        <label className="min-w-0 space-y-1 text-xs"><span className="block">{t("pluginRegistry.lifecycle.selectVersion")}</span>
          <select value={version} onChange={event => { setVersion(event.target.value); resetReview(); }} disabled={pending || reviewing || stale || needsRefresh} className="h-9 max-w-full rounded-md border border-border bg-background px-3 text-sm">
            {!version && <option value="">{t("pluginRegistry.lifecycle.noVersion")}</option>}
            {version && !present && <option value={version}>{version} ({t("pluginRegistry.lifecycle.unavailable")})</option>}
            {plugin.versions.map(item => <option key={item.version} value={item.version}>{item.version}</option>)}
          </select>
        </label>
        <label className="flex min-h-9 items-center gap-2 text-sm"><input type="checkbox" checked={enabled} disabled={pending || reviewing || stale || needsRefresh} onChange={event => { setEnabled(event.target.checked); resetReview(); }} />{t("pluginRegistry.lifecycle.enableForTenant")}</label>
        {!reviewing && <Button type="submit" variant="outline" disabled={blocked || !dirty}>{t("pluginRegistry.lifecycle.review")}</Button>}
      </div>
      {!present && <p className="text-xs text-warning">{t("pluginRegistry.lifecycle.missingVersion")}</p>}
      {reviewing && <div className="space-y-3 rounded-lg border border-primary/25 bg-background p-3">
        <p className="break-words text-sm">{t("pluginRegistry.lifecycle.confirm", { tenant: tenantId, plugin: plugin.id, version, state: t(enabled ? "pluginRegistry.lifecycle.enabled" : "pluginRegistry.lifecycle.disabled") })}</p>
        <p className="text-xs leading-relaxed text-muted-foreground">{t("pluginRegistry.lifecycle.effectNotice")}</p>
        <PluginImpactReview plugin={plugin} version={version} enabled={enabled} acknowledged={acknowledged} onAcknowledge={value => { consent.current = value ? reviewKey : null; setAcknowledgedKey(consent.current); }} disabled={blocked} />
        <div className="flex gap-2"><Button type="button" disabled={blocked || !acknowledged} onClick={() => void save()}>{t(pending ? "pluginRegistry.lifecycle.saving" : "pluginRegistry.lifecycle.save")}</Button><Button type="button" variant="ghost" disabled={pending} onClick={resetReview}>{t("common.cancel")}</Button></div>
      </div>}
    </form> : <p className="text-xs text-muted-foreground">{t("pluginRegistry.lifecycle.adminOnly")}</p>}
    {message && <p role={failed ? "alert" : "status"} className={`text-sm ${failed ? "text-destructive" : "text-muted-foreground"}`}>{t(message)}</p>}
  </section>;
}
