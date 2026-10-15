import type { PluginCapability, PluginRegistryEntry } from "@/api/plugins";

export type CapabilityChange = "added" | "removed" | "changed" | "unchanged" | "proposed";
export interface CapabilityImpact {
  name: string;
  change: CapabilityChange;
  before?: PluginCapability;
  after?: PluginCapability;
}
const ordered = (values: string[]) => [...new Set(values)].sort();
function descriptor(capability: PluginCapability) {
  // Availability is a policy observation, not a descriptor or a permission grant.
  return [capability.id, capability.name, capability.version, capability.schema_hash,
    capability.category, capability.risk_level, capability.read_only,
    ordered(capability.required_permissions), ordered(capability.declared_permissions)];
}
export function pluginLifecycleImpact(plugin: PluginRegistryEntry, version: string, enabled: boolean) {
  const selected = plugin.versions.find(item => item.version === plugin.lifecycle.selected_version);
  const target = plugin.versions.find(item => item.version === version);
  const knownBaseline = plugin.lifecycle.managed && (!plugin.lifecycle.enabled
    || (plugin.lifecycle.selection_status === "current" && Boolean(selected)));
  const before = knownBaseline && plugin.lifecycle.enabled ? selected?.capabilities ?? [] : [];
  const after = enabled ? target?.capabilities ?? [] : [];
  const beforeByName = new Map(before.map(item => [item.name, item]));
  const afterByName = new Map(after.map(item => [item.name, item]));
  const names = ordered([...beforeByName.keys(), ...afterByName.keys()]);
  const entries: CapabilityImpact[] = names.map(name => {
    const previous = beforeByName.get(name);
    const next = afterByName.get(name);
    return { name, before: previous, after: next, change: !knownBaseline ? "proposed"
      : !previous ? "added" : !next ? "removed"
      : JSON.stringify(descriptor(previous)) === JSON.stringify(descriptor(next)) ? "unchanged" : "changed" };
  });
  return { knownBaseline, targetKnown: !enabled || Boolean(target), entries };
}

/** Approval is bound to every reviewed descriptor and the CAS request identity. */
export function pluginLifecycleReviewKey(plugin: PluginRegistryEntry, version: string, enabled: boolean, tenantId: string, catalogRevision: string) {
  return JSON.stringify([tenantId, catalogRevision, plugin.id, plugin.lifecycle,
    version, enabled, plugin.versions.map(item => [item.version, item.availability,
      ordered(item.required_permissions), ordered(item.declared_permissions),
      item.capabilities.map(capability => [descriptor(capability), capability.available])])]);
}
