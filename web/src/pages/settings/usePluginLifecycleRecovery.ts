import { useRef, useState } from "react";
import { isAxiosError } from "axios";
import { updatePluginLifecycle, type PluginRegistry, type UpdatePluginLifecycle } from "@/api/plugins";
import { pluginLifecycleReviewKey } from "./pluginLifecycleImpact";

export interface PluginLifecycleMutationState {
  attempt: number;
  pending: boolean;
  message: string;
  failed: boolean;
}
interface Options {
  isCurrent: () => boolean;
  registry: () => PluginRegistry | undefined;
  registryReady: () => boolean;
  refreshRegistry: () => Promise<boolean>;
  invalidateTools: () => void;
}

/** Page-owned write fences survive filtered cards and registry-key remounts. */
export function usePluginLifecycleRecovery(options: Options) {
  const owner = useRef(new Map<string, PluginLifecycleMutationState>());
  const [mutations, setMutations] = useState(owner.current);
  const attempt = useRef(0);
  const read = useRef(0);
  const replace = (next: typeof mutations) => { owner.current = next; setMutations(next); };
  const refresh = async () => {
    if (!options.isCurrent()) return false;
    const generation = ++read.current;
    // A read begun before a PUT settled cannot resolve that PUT's outcome.
    const settled = [...owner.current].filter(([, state]) => !state.pending);
    let success = false;
    try { success = await options.refreshRegistry(); } catch { /* Keep the write fence. */ }
    if (!options.isCurrent() || generation !== read.current || !success) return false;
    const next = new Map(owner.current);
    for (const [id, state] of settled) {
      if (next.get(id)?.attempt === state.attempt && !next.get(id)?.pending) next.delete(id);
    }
    replace(next);
    return true;
  };
  const reviewGeneration = read.current;
  const save = async (pluginId: string, payload: UpdatePluginLifecycle, reviewedKey: string) => {
    if (!options.isCurrent() || !options.registryReady() || reviewGeneration !== read.current || owner.current.has(pluginId)) return false;
    const registry = options.registry();
    const plugin = registry?.plugins.find(item => item.id === pluginId);
    if (!registry?.can_manage || !plugin || registry.tenant_id !== payload.expected_tenant_id
      || registry.catalog_revision_token !== payload.expected_catalog_revision
      || plugin.lifecycle.revision !== payload.expected_revision
      || pluginLifecycleReviewKey(plugin, payload.selected_version, payload.enabled,
        registry.tenant_id, registry.catalog_revision_token) !== reviewedKey) return false;
    const identity = ++attempt.current;
    const update = (pending: boolean, message: string, failed: boolean) => {
      if (!options.isCurrent() || owner.current.get(pluginId)?.attempt !== identity) return;
      replace(new Map(owner.current).set(pluginId, { attempt: identity, pending, message, failed }));
    };
    replace(new Map(owner.current).set(pluginId, { attempt: identity, pending: true, message: "pluginRegistry.lifecycle.saving", failed: false }));
    try {
      await updatePluginLifecycle(pluginId, payload);
    } catch (error) {
      const status = isAxiosError(error) ? error.response?.status : undefined;
      update(false, status === 409 ? "pluginRegistry.lifecycle.conflict"
        : status === 403 ? "pluginRegistry.lifecycle.forbidden" : "pluginRegistry.lifecycle.uncertain", true);
      return false;
    }
    if (!options.isCurrent()) return false;
    update(false, "pluginRegistry.lifecycle.saved", false);
    options.invalidateTools();
    if (!await refresh()) update(false, "pluginRegistry.lifecycle.savedRefreshFailed", false);
    return true;
  };
  return { mutations, refresh, save };
}
