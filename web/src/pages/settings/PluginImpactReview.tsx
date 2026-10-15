import { useTranslation } from "react-i18next";
import type { PluginCapability, PluginRegistryEntry } from "@/api/plugins";
import { pluginLifecycleImpact } from "./pluginLifecycleImpact";

function Descriptor({ value }: { value?: PluginCapability }) {
  const { t } = useTranslation();
  if (!value) return <p className="text-xs text-muted-foreground">{t("pluginRegistry.impact.absent")}</p>;
  return <dl className="space-y-1 break-all text-xs">
    <div><dt>{t("pluginRegistry.impact.identity")}</dt><dd className="font-mono">{value.id} · {value.version && value.version !== "null" ? value.version : t("pluginRegistry.impact.unspecifiedVersion")}</dd></div>
    <div><dt>{t("pluginRegistry.schemaHash")}</dt><dd className="font-mono">{value.schema_hash}</dd></div>
    <div><dt>{t("pluginRegistry.category")}</dt><dd>{value.category}</dd></div>
    <div><dt>{t("pluginRegistry.risk")}</dt><dd>{value.risk_level}</dd></div>
    <div><dt>{t("pluginRegistry.effect")}</dt><dd>{t(value.read_only ? "pluginRegistry.readOnly" : "pluginRegistry.mayWrite")}</dd></div>
    <div><dt>{t("pluginRegistry.requiredPermissions")}</dt><dd>{value.required_permissions.join(", ") || t("pluginRegistry.noPermissions")}</dd></div>
    <div><dt>{t("pluginRegistry.declaredPermissions")}</dt><dd>{value.declared_permissions.join(", ") || t("pluginRegistry.noPermissions")}</dd></div>
    <div><dt>{t("pluginRegistry.impact.policyObservation")}</dt><dd>{t(value.available ? "pluginRegistry.status.available" : "pluginRegistry.status.policy_blocked")}</dd></div>
  </dl>;
}

export function PluginImpactReview({ plugin, version, enabled, acknowledged, onAcknowledge, disabled }: {
  plugin: PluginRegistryEntry; version: string; enabled: boolean; acknowledged: boolean;
  onAcknowledge: (value: boolean) => void; disabled: boolean;
}) {
  const { t } = useTranslation();
  const impact = pluginLifecycleImpact(plugin, version, enabled);
  return <section aria-label={t("pluginRegistry.impact.title")} className="space-y-3">
    <h3 className="text-sm font-semibold">{t("pluginRegistry.impact.title")}</h3>
    <p className="text-xs text-muted-foreground">{t("pluginRegistry.impact.definitions")}</p>
    {!impact.knownBaseline && <p role="status" className="text-sm text-warning">{t("pluginRegistry.impact.unknownBaseline")}</p>}
    {!impact.targetKnown && <p role="alert" className="text-sm text-warning">{t("pluginRegistry.impact.unknownTarget")}</p>}
    {impact.entries.length ? <ul className="max-h-96 space-y-3 overflow-y-auto">{impact.entries.map(entry => <li key={entry.name} className="space-y-2 rounded border border-border p-3">
      <p className="break-all text-sm font-medium">{entry.name} · {t(`pluginRegistry.impact.${entry.change}`)}</p>
      <div className="grid gap-3 sm:grid-cols-2"><div><h4 className="mb-2 text-xs font-semibold">{t(impact.knownBaseline ? "pluginRegistry.impact.before" : "pluginRegistry.impact.unknown")}</h4>{impact.knownBaseline && <Descriptor value={entry.before} />}</div>
        <div><h4 className="mb-2 text-xs font-semibold">{t("pluginRegistry.impact.after")}</h4><Descriptor value={entry.after} /></div></div>
    </li>)}</ul> : <p className="text-xs">{t(enabled ? "pluginRegistry.impact.empty" : "pluginRegistry.impact.disabled")}</p>}
    <p className="text-xs text-muted-foreground">{t("pluginRegistry.impact.scope")}</p>
    <label className="flex items-start gap-2 text-sm"><input type="checkbox" checked={acknowledged} disabled={disabled || !impact.targetKnown} onChange={event => onAcknowledge(event.target.checked)} />{t("pluginRegistry.impact.acknowledge")}</label>
  </section>;
}
