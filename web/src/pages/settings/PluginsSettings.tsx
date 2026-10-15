import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { ArrowLeft, Package, RefreshCw, Search, ShieldCheck, ChevronDown, Layers } from "lucide-react";
import { getPluginRegistry, type PluginAvailability } from "@/api/plugins";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import { usePluginLifecycleRecovery } from "./usePluginLifecycleRecovery";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ManagementQueryState } from "@/components/ManagementQueryState";
import { PluginLifecycleControls } from "./PluginLifecycleControls";

function PermissionList({ label, permissions, empty }: { label: string; permissions: string[]; empty: string }) {
  return <div className="min-w-0"><dt className="mb-2 text-xs text-muted-foreground">{label}</dt><dd className="flex flex-wrap gap-1.5">{permissions.length ? permissions.map(permission => <span key={permission} className="max-w-full break-all rounded border border-border bg-muted/40 px-2 py-1 font-mono text-[11px]">{permission}</span>) : <span className="text-xs text-muted-foreground">{empty}</span>}</dd></div>;
}

function AvailabilityBadge({ value }: { value: PluginAvailability }) {
  const { t } = useTranslation();
  const color = value === "available" ? "border-success/25 bg-success/10 text-success" : value === "partially_available" ? "border-warning/25 bg-warning/10 text-warning" : "border-border bg-muted text-muted-foreground";
  return <span className={`inline-flex rounded-md border px-2 py-1 text-xs font-medium ${color}`}>{t(`pluginRegistry.status.${value}`)}</span>;
}

export function PluginsSettingsPage() {
  const scope = useAuthenticatedReadScope();
  return scope.isCurrent() ? <PluginsSettings key={scope.id} scopeId={scope.id} isCurrent={scope.isCurrent} /> : null;
}

function PluginsSettings({ scopeId, isCurrent }: { scopeId: string; isCurrent: () => boolean }) {
  const { t } = useTranslation();
  const client = useQueryClient();
  const queryKey = ["plugin-registry", scopeId];
  const [search, setSearch] = useState("");
  const [refreshGeneration, setRefreshGeneration] = useState(0);
  const [availability, setAvailability] = useState("all");
  const query = useQuery({
    queryKey,
    queryFn: getPluginRegistry,
    staleTime: 30_000,
    gcTime: 0,
    retry: false,
  });
  const filtered = useMemo(() => {
    const needle = search.trim().toLocaleLowerCase();
    return (query.data?.plugins || []).map(plugin => ({ ...plugin, versions: plugin.versions.filter(version => {
      const text = `${plugin.id} ${version.version} ${version.capabilities.map(capability => `${capability.id} ${capability.name} ${capability.category}`).join(" ")}`.toLocaleLowerCase();
      return text.includes(needle) && (availability === "all" || version.availability === availability);
    }) })).filter(plugin => plugin.versions.length > 0 || (plugin.lifecycle.selection_status === "version_unavailable" && plugin.id.toLocaleLowerCase().includes(needle) && availability === "all"));
  }, [query.data, search, availability]);
  const recovery = usePluginLifecycleRecovery({
    isCurrent,
    registry: () => client.getQueryData(queryKey),
    registryReady: () => {
      const state = client.getQueryState(queryKey);
      return state?.status === "success" && state.fetchStatus === "idle";
    },
    refreshRegistry: async () => {
      const previousUpdates = client.getQueryState(queryKey)?.dataUpdateCount ?? 0;
      const result = await query.refetch();
      if (!isCurrent() || !result.isSuccess || result.fetchStatus !== "idle"
        || (client.getQueryState(queryKey)?.dataUpdateCount ?? 0) <= previousUpdates) return false;
      setRefreshGeneration(value => value + 1);
      return true;
    },
    invalidateTools: () => { void client.invalidateQueries({ queryKey: ["agent-catalog", "tools"] }); },
  });
  const refresh = recovery.refresh;
  const versions = query.data?.plugins.flatMap(plugin => plugin.versions) || [];
  const availableCapabilities = versions.reduce((total, version) => total + version.capabilities.filter(capability => capability.available).length, 0);

  return <div className="mx-auto max-w-6xl space-y-5 pb-6">
    <Link to="/settings" className="inline-flex items-center gap-2 text-sm text-muted-foreground hover:text-foreground"><ArrowLeft size={15} />{t("pluginRegistry.back")}</Link>
    <header className="flex flex-wrap items-start justify-between gap-4 rounded-xl border border-border bg-card p-5 sm:p-6">
      <div className="max-w-2xl"><p className="flex items-center gap-2 text-xs font-medium uppercase tracking-wider text-primary"><Package size={14} />{t("pluginRegistry.eyebrow")}</p><h1 className="mt-3 text-2xl font-semibold tracking-tight">{t("pluginRegistry.title")}</h1><p className="mt-2 text-sm leading-relaxed text-muted-foreground">{t("pluginRegistry.description")}</p></div>
      <Button variant="outline" onClick={() => void refresh()} disabled={query.isFetching}><RefreshCw className={`mr-2 h-4 w-4 ${query.isFetching ? "animate-spin" : ""}`} />{t("management.refresh")}</Button>
    </header>
    <div className="flex items-start gap-3 rounded-lg border border-border bg-muted/30 p-4"><ShieldCheck className="mt-0.5 h-4 w-4 shrink-0 text-primary" /><p className="text-sm leading-relaxed text-muted-foreground">{t("pluginRegistry.policyNotice")}</p></div>
    {query.isError && <ManagementQueryState onRetry={() => void refresh()} retrying={query.isFetching} stale={Boolean(query.data)} />}
    <section aria-label={t("pluginRegistry.summary")} className="grid gap-3 sm:grid-cols-3">
      {[
        { label: "plugins", value: query.data?.plugins.length },
        { label: "versions", value: query.data ? versions.length : undefined },
        { label: "capabilities", value: query.data ? availableCapabilities : undefined },
      ].map(metric => <div className="rounded-xl border border-border bg-card p-4" key={metric.label}><p className="text-xs text-muted-foreground">{t(`pluginRegistry.metrics.${metric.label}`)}</p><strong className="mt-3 block text-2xl font-semibold tabular-nums">{metric.value ?? "—"}</strong></div>)}
    </section>
    <section aria-label={t("pluginRegistry.title")} aria-busy={query.isFetching} className="space-y-4">
      <div className="flex flex-wrap items-center gap-3">
        <div className="relative min-w-0 flex-1"><Search className="pointer-events-none absolute left-3 top-2.5 h-4 w-4 text-muted-foreground" /><Input value={search} onChange={event => setSearch(event.target.value)} className="pl-9" placeholder={t("pluginRegistry.search")} aria-label={t("pluginRegistry.search")} /></div>
        <select value={availability} onChange={event => setAvailability(event.target.value)} aria-label={t("pluginRegistry.filter")} className="h-9 rounded-md border border-border bg-background px-3 text-sm"><option value="all">{t("pluginRegistry.allStatuses")}</option>{(["available", "partially_available", "policy_blocked"] as const).map(status => <option value={status} key={status}>{t(`pluginRegistry.status.${status}`)}</option>)}</select>
      </div>
      {query.isPending ? <div role="status" className="rounded-xl border border-border bg-card p-12 text-center text-sm text-muted-foreground">{t("common.loading")}</div> : query.isError && !query.data ? null : filtered.length ? filtered.map(plugin => <article key={plugin.id} className="overflow-hidden rounded-xl border border-border bg-card">
        <div className="flex flex-wrap items-center gap-3 border-b border-border p-4 sm:px-5"><span className="rounded-lg bg-primary/10 p-2 text-primary"><Package size={19} /></span><h2 className="min-w-0 flex-1 break-all font-mono text-sm font-semibold">{plugin.id}</h2><span className="text-xs text-muted-foreground">{t("pluginRegistry.versionCount", { count: plugin.versions.length })}</span></div>
        {query.data && <PluginLifecycleControls key={`${scopeId}:${query.data.tenant_id}:${plugin.id}:${plugin.lifecycle.revision}:${query.data.catalog_revision_token}:${refreshGeneration}`}
          plugin={query.data.plugins.find(item => item.id === plugin.id) ?? plugin} tenantId={query.data.tenant_id}
          catalogRevision={query.data.catalog_revision_token} canManage={query.data.can_manage}
          stale={query.isError || query.isFetching} mutation={recovery.mutations.get(plugin.id)}
          onSave={(payload, reviewedKey) => recovery.save(plugin.id, payload, reviewedKey)} />}
        <div className="divide-y divide-border">{plugin.versions.map(version => <details key={version.version} className="group p-4 sm:px-5">
          <summary className="flex cursor-pointer list-none flex-wrap items-center gap-3 rounded-md focus-visible:outline-2 focus-visible:outline-ring"><Layers size={16} className="text-muted-foreground" /><span className="min-w-0 flex-1 break-all font-mono text-sm">{version.version}</span><AvailabilityBadge value={version.availability} /><ChevronDown size={16} className="text-muted-foreground transition-transform group-open:rotate-180" /></summary>
          <div className="mt-5 space-y-5"><p className="text-xs text-muted-foreground">{t("pluginRegistry.versionNote")}</p>
            <dl className="grid gap-4 sm:grid-cols-2"><PermissionList label={t("pluginRegistry.requiredPermissions")} permissions={version.required_permissions} empty={t("pluginRegistry.noPermissions")} /><PermissionList label={t("pluginRegistry.declaredPermissions")} permissions={version.declared_permissions} empty={t("pluginRegistry.noPermissions")} /></dl>
            <div className="space-y-3"><h3 className="text-sm font-semibold">{t("pluginRegistry.capabilities")}</h3>{version.capabilities.map(capability => <section key={capability.id} className="space-y-4 rounded-lg border border-border bg-muted/20 p-4">
              <div className="flex flex-wrap items-center justify-between gap-2"><div className="min-w-0"><h4 className="break-all text-sm font-medium">{capability.name}</h4><p className="mt-1 break-all font-mono text-[11px] text-muted-foreground">{capability.id}</p></div><AvailabilityBadge value={capability.available ? "available" : "policy_blocked"} /></div>
              <dl className="grid grid-cols-2 gap-3 text-xs sm:grid-cols-4">{[["category", capability.category], ["risk", capability.risk_level], ["effect", t(capability.read_only ? "pluginRegistry.readOnly" : "pluginRegistry.mayWrite")], ["version", capability.version]].map(([label,value]) => <div key={label}><dt className="text-muted-foreground">{t(`pluginRegistry.${label}`)}</dt><dd className="mt-1 break-all">{value}</dd></div>)}</dl>
              <dl className="grid gap-4 sm:grid-cols-2"><PermissionList label={t("pluginRegistry.requiredPermissions")} permissions={capability.required_permissions} empty={t("pluginRegistry.noPermissions")} /><PermissionList label={t("pluginRegistry.declaredPermissions")} permissions={capability.declared_permissions} empty={t("pluginRegistry.noPermissions")} /></dl>
              <dl><dt className="text-xs text-muted-foreground">{t("pluginRegistry.schemaHash")}</dt><dd className="mt-1 break-all font-mono text-[11px]">{capability.schema_hash}</dd></dl>
            </section>)}</div>
          </div>
        </details>)}</div>
      </article>) : <div className="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border bg-card px-6 py-14 text-center"><Package size={30} className="text-muted-foreground" /><h2 className="text-base font-semibold">{t(search || availability !== "all" ? "pluginRegistry.noMatches" : "pluginRegistry.empty")}</h2><p className="max-w-xl text-sm leading-relaxed text-muted-foreground">{t(search || availability !== "all" ? "pluginRegistry.filterHint" : "pluginRegistry.emptyHint")}</p>{(search || availability !== "all") && <Button variant="outline" onClick={() => { setSearch(""); setAvailability("all"); }}>{t("management.clearFilters")}</Button>}</div>}
      {query.data && <p className="break-all text-xs text-muted-foreground">{t("pluginRegistry.revision", { revision: query.data.catalog_revision_token })} · {query.data.schema_version}</p>}
    </section>
  </div>;
}
