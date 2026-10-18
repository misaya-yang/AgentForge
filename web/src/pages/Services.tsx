import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
/**
 * Services Page with Tabs
 *
 * Tabs: Services, Providers, Models
 */

import { useState, useMemo, useEffect, useRef } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { ManagementQueryState } from "@/components/ManagementQueryState";
import { useQuery, useQueryClient, useMutation } from "@tanstack/react-query";
import { sensitiveMutationOptions } from "@/lib/consoleStepUp";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Plus, Server, Cloud, Cpu, Search, RefreshCw, Plug, ArrowRight } from "lucide-react";
import { useToast } from "@/hooks/use-toast";
import { useTranslation } from "react-i18next";
import { getErrorMessage } from "@/lib/utils";

import { useManagedModels } from "@/hooks/useManagedModels";
import { useArchitectureStatus, useServices, useHealth } from "@/hooks/useServices";
import { ServiceCard } from "@/components/ServiceCard";
import { ServiceForm } from "@/components/ServiceForm";
import { useAppStore } from "@/store/useAppStore";
import { useAuthStore } from "@/store/useAuthStore";
import { ArchitectureStatusPanel } from "@/components/services/ArchitectureStatusPanel";
import { canViewArchitectureStatus } from "@/components/services/architectureStatusPresentation";

// Provider & Model imports
import { ProviderCard, ProviderForm, ModelTable, ModelForm } from "@/components/llm";
import { ProviderCatalogReview } from "@/components/llm/ProviderCatalogReview";
import * as providersApi from "@/api/providers";
import * as modelsApi from "@/api/models";
import type {
  Provider,
  ProviderCreate,
  ProviderFromTemplateCreate,
  ProviderUpdate,
} from "@/api/providers";
import type { LLMModel, ModelCreate, ModelUpdate } from "@/api/models";

export function ServicesPage() {
  const { toast } = useToast();
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [searchParams, setSearchParams] = useSearchParams();
  const requestedTab = searchParams.get("tab");
  const activeTab = requestedTab === "providers" || requestedTab === "models" ? requestedTab : "services";
  const setActiveTab = (tab: string) => setSearchParams(previous => {
    const next = new URLSearchParams(previous);
    next.set("tab", tab);
    return next;
  });
  const [statusFilter, setStatusFilter] = useState("all");
  const canViewSettings = useAuthStore(state => state.hasPermission("console:settings:view"));
  const [searchInput, setSearchInput] = useState("");
  const [searchQuery, setSearchQuery] = useState("");
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Debounced search to avoid excessive re-renders
  useEffect(() => {
    if (debounceRef.current) {
      clearTimeout(debounceRef.current);
    }
    debounceRef.current = setTimeout(() => {
      setSearchQuery(searchInput);
    }, 300);
    return () => {
      if (debounceRef.current) {
        clearTimeout(debounceRef.current);
      }
    };
  }, [searchInput]);

  // Services state
  const providerReadScope = useAuthenticatedReadScope();
  const servicesQuery = useServices();
  const healthQuery = useHealth();
  const services = useMemo(
    () => servicesQuery.data ?? [],
    [servicesQuery.data]
  );
  const health = healthQuery.data || {};
  const user = useAuthStore((state) => state.user);
  const canViewArchitecture = useMemo(
    () => canViewArchitectureStatus(user?.roles ?? [], user?.permissions ?? []),
    [user?.permissions, user?.roles],
  );
  const architectureQuery = useArchitectureStatus(
    activeTab === "services" && canViewArchitecture
  );
  // Filtered Services
  const filteredServices = useMemo(() => {
    let result = services;
    if (searchQuery) {
      const q = searchQuery.toLowerCase();
      result = result.filter(s =>
        (s.service_id || "").toLowerCase().includes(q) ||
        (s.description || "").toLowerCase().includes(q)
      );
    }
    return result;
  }, [services, searchQuery]);

  const { selectedServiceId, setSelectedServiceId } = useAppStore();

  // Provider state
  const [providerFormOpen, setProviderFormOpen] = useState(false);
  const [editingProvider, setEditingProvider] = useState<Provider | null>(null);
  const [providerTestRevisions, setProviderTestRevisions] = useState<Record<string, number>>({});
  const [deleteProviderOpen, setDeleteProviderOpen] = useState(false);
  const [providerToDelete, setProviderToDelete] = useState<Provider | null>(null);

  // Model state
  const [modelFormOpen, setModelFormOpen] = useState(false);
  const [editingModel, setEditingModel] = useState<LLMModel | null>(null);
  const [deleteModelOpen, setDeleteModelOpen] = useState(false);
  const [modelToDelete, setModelToDelete] = useState<LLMModel | null>(null);

  // Provider queries
  const providersQuery = useQuery({
    ...providersApi.providersOptions(providerReadScope.id, true),
    enabled: providerReadScope.isCurrent(),
  });
  const providerTemplatesQuery = useQuery({
    queryKey: providersApi.providerQueryKeys.templates,
    queryFn: providersApi.listProviderTemplates,
  });
  const providers = useMemo(
    () => providersQuery.data ?? [],
    [providersQuery.data]
  );
  const providerMap = providers.reduce(
    (acc, p) => ({ ...acc, [p.provider_id]: p.display_name }),
    {} as Record<string, string>
  );

  // Filtered Providers
  const filteredProviders = useMemo(() => {
    let result = providers.filter(provider => statusFilter === "all" || provider.is_enabled === (statusFilter === "enabled"));
    if (searchQuery) {
      const q = searchQuery.toLowerCase();
      result = result.filter(p =>
        (p.display_name || "").toLowerCase().includes(q) ||
        (p.provider_id || "").toLowerCase().includes(q) ||
        (p.base_url || "").toLowerCase().includes(q)
      );
    }
    return result;
  }, [providers, searchQuery, statusFilter]);

  // Model queries
  const managedModels = useManagedModels();
  const modelsQuery = managedModels.query;
  const capabilityAdaptersQuery = useQuery({
    queryKey: ["model-capability-adapters"],
    queryFn: modelsApi.listCapabilityAdapters,
  });

  const models = useMemo(() => {
    if (!modelsQuery.data) return [];
    let result = modelsQuery.data.filter(model => statusFilter === "all" || model.is_enabled === (statusFilter === "enabled")).sort((a, b) => {
      // Priority 1: Enabled first
      if (a.is_enabled !== b.is_enabled) {
        return a.is_enabled ? -1 : 1;
      }
      // Priority 2: Sort order (descending)
      if ((a.sort_order || 0) !== (b.sort_order || 0)) {
        return (b.sort_order || 0) - (a.sort_order || 0);
      }
      // Priority 3: Alphabetical by display name
      return a.display_name.localeCompare(b.display_name);
    });

    if (searchQuery) {
      const q = searchQuery.toLowerCase();
      result = result.filter(m =>
        (m.display_name || "").toLowerCase().includes(q) ||
        (m.model_id || "").toLowerCase().includes(q) ||
        (m.provider_id || "").toLowerCase().includes(q)
      );
    }
    return result;
  }, [modelsQuery.data, searchQuery, statusFilter]);

  // Provider mutations
  const createProviderMutation = useMutation({
    ...sensitiveMutationOptions,
    mutationFn: (data: ProviderCreate | ProviderFromTemplateCreate) =>
      typeof data.template_id === "string"
        ? providersApi.createProviderFromTemplate(data as ProviderFromTemplateCreate)
        : providersApi.createProvider(data as ProviderCreate),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["providers"] });
      setProviderFormOpen(false);
      toast({ title: t("services.page.toast.providerCreated") });
    },
    onError: (err: Error) => {
      toast({ title: t("services.page.toast.createFailed"), description: getErrorMessage(err), variant: "destructive" });
    },
  });

  const updateProviderMutation = useMutation({
    ...sensitiveMutationOptions,
    mutationFn: ({ id, data }: { id: string; data: ProviderUpdate }) =>
      providersApi.updateProvider(id, data),
    onSuccess: (_updated, { id }) => {
      // A known successful save invalidates connectivity feedback immediately,
      // even if the subsequent provider-list refresh fails or stays pending.
      setProviderTestRevisions(current => ({ ...current, [id]: (current[id] ?? 0) + 1 }));
      qc.invalidateQueries({ queryKey: ["providers"] });
      setProviderFormOpen(false);
      setEditingProvider(null);
      toast({ title: t("services.page.toast.providerUpdated") });
    },
    onError: (err: Error) => {
      toast({ title: t("services.page.toast.updateFailed"), description: getErrorMessage(err), variant: "destructive" });
    },
  });

  const deleteProviderMutation = useMutation({
    ...sensitiveMutationOptions,
    mutationFn: (id: string) => providersApi.deleteProvider(id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["providers"] });
      qc.invalidateQueries({ queryKey: ["models"] });
      setDeleteProviderOpen(false);
      setProviderToDelete(null);
      toast({ title: t("services.page.toast.providerDeleted") });
    },
    onError: (err: Error) => {
      toast({ title: t("services.page.toast.deleteFailed"), description: getErrorMessage(err), variant: "destructive" });
    },
  });

  const [catalogProvider, setCatalogProvider] = useState<string | null>(null);

  // Model mutations
  const createModelMutation = useMutation({
    mutationFn: (data: ModelCreate) => modelsApi.createModel(data),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["models"] });
      setModelFormOpen(false);
      toast({ title: t("services.page.toast.modelCreated") });
    },
    onError: (err: Error) => {
      toast({ title: t("services.page.toast.createFailed"), description: getErrorMessage(err), variant: "destructive" });
    },
  });

  const updateModelMutation = useMutation({
    mutationFn: ({
      id,
      data,
      providerId,
    }: {
      id: string;
      data: ModelUpdate;
      providerId?: string;
    }) => modelsApi.updateModel(id, data, providerId),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["models"] });
      setModelFormOpen(false);
      setEditingModel(null);
      toast({ title: t("services.page.toast.modelUpdated") });
    },
    onError: (err: Error) => {
      toast({ title: t("services.page.toast.updateFailed"), description: getErrorMessage(err), variant: "destructive" });
    },
  });

  const deleteModelMutation = useMutation({
    mutationFn: (model: LLMModel) => modelsApi.deleteModel(model.model_id, model.provider_id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["models"] });
      setDeleteModelOpen(false);
      setModelToDelete(null);
      toast({ title: t("services.page.toast.modelDeleted") });
    },
    onError: (err: Error) => {
      toast({ title: t("services.page.toast.deleteFailed"), description: getErrorMessage(err), variant: "destructive" });
    },
  });

  const resetCapabilitiesMutation = useMutation({
    mutationFn: ({
      id,
      providerId,
      capabilityRevision,
    }: {
      id: string;
      providerId?: string;
      capabilityRevision: number;
    }) => modelsApi.resetModelCapabilities(id, capabilityRevision, providerId),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["models"] });
      setModelFormOpen(false);
      setEditingModel(null);
      toast({
        title: t("services.page.toast.capabilitiesReset", "Capability overrides cleared"),
      });
    },
    onError: (err: Error) => {
      toast({
        title: t("services.page.toast.updateFailed"),
        description: getErrorMessage(err),
        variant: "destructive",
      });
    },
  });

  // Reset selected service if not in list
  if (selectedServiceId && services.length > 0 && !services.some(s => s.service_id === selectedServiceId)) {
    setTimeout(() => setSelectedServiceId(undefined), 0);
  }

  // Provider handlers
  const handleProviderSubmit = async (
    data: ProviderCreate | ProviderFromTemplateCreate | ProviderUpdate
  ) => {
    if (editingProvider) {
      await updateProviderMutation.mutateAsync({
        id: editingProvider.provider_id,
        data: data as ProviderUpdate,
      });
    } else {
      await createProviderMutation.mutateAsync(data as ProviderCreate);
    }
  };

  const handleEditProvider = (provider: Provider) => {
    setEditingProvider(provider);
    setProviderFormOpen(true);
  };

  const handleDeleteProvider = (provider: Provider) => {
    setProviderToDelete(provider);
    setDeleteProviderOpen(true);
  };

  // Model handlers
  const handleModelSubmit = async (data: ModelCreate | ModelUpdate) => {
    if (editingModel) {
      await updateModelMutation.mutateAsync({
        id: editingModel.model_id,
        data: data as ModelUpdate,
        providerId: editingModel.provider_id,
      });
    } else {
      await createModelMutation.mutateAsync(data as ModelCreate);
    }
  };

  const handleEditModel = (model: LLMModel) => {
    setEditingModel(model);
    setModelFormOpen(true);
  };

  const handleDeleteModel = (model: LLMModel) => {
    setModelToDelete(model);
    setDeleteModelOpen(true);
  };

  return (
    <div className="space-y-4 p-1 pt-2">
      {catalogProvider && <ProviderCatalogReview key={`${catalogProvider}:${providers.find(p => p.provider_id === catalogProvider)?.updated_at}:${user?.user_id}:${providerTestRevisions[catalogProvider] ?? 0}`} provider={catalogProvider} onClose={() => setCatalogProvider(null)} onReadback={() => { void qc.invalidateQueries({queryKey:["models"]}); void qc.invalidateQueries({queryKey:["providers"]}); }} />}
      <div className="flex flex-wrap items-start justify-between gap-4 rounded-xl border border-border bg-card p-5">
        <div className="max-w-2xl"><p className="text-xs font-medium uppercase tracking-wider text-primary">{t("management.eyebrow")}</p><h1 className="mt-2 text-2xl font-semibold tracking-tight">{t("services.page.title")}</h1><p className="mt-2 text-sm leading-relaxed text-muted-foreground">{t("management.servicesIntro")}</p></div>
        <div className="flex flex-wrap gap-2">
          {canViewSettings && <Button asChild variant="outline"><Link to="/settings/connectors"><Plug className="mr-2 h-4 w-4" />{t("management.connectors")}</Link></Button>}
          <Button variant="outline" disabled={servicesQuery.isFetching || providersQuery.isFetching || modelsQuery.isFetching} onClick={() => { void servicesQuery.refetch(); void providersQuery.refetch(); void modelsQuery.refetch(); void healthQuery.refetch(); }}><RefreshCw className="mr-2 h-4 w-4" />{t("management.refresh")}</Button>
        </div>
      </div>
      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        {[
          { key: "services", icon: Server, value: servicesQuery.data ? services.length : "—", note: "registered" },
          { key: "providers", icon: Cloud, value: providersQuery.data ? providers.filter(provider => provider.is_enabled).length : "—", note: "enabled" },
          { key: "models", icon: Cpu, value: modelsQuery.data ? modelsQuery.data.filter(model => model.is_enabled).length : "—", note: "enabled" },
          { key: "credentials", icon: Cloud, value: providersQuery.data ? providers.filter(provider => provider.is_enabled && (provider.has_api_key || provider.allow_environment_credentials)).length : "—", note: "credentialNote" },
        ].map(metric => <div key={metric.key} className="rounded-xl border border-border bg-card p-4"><div className="flex items-center justify-between gap-2 text-xs text-muted-foreground"><span>{t(`management.metrics.${metric.key}`)}</span><metric.icon size={16} aria-hidden="true" /></div><strong className="my-3 block text-2xl font-semibold tabular-nums">{metric.value}</strong><p className="text-xs text-muted-foreground">{t(`management.${metric.note}`)}</p></div>)}
      </div>
      {providersQuery.isSuccess && providers.length === 0 && <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-primary/20 bg-primary/5 p-4"><p className="text-sm">{t("management.firstProvider")}</p><Button onClick={() => { setActiveTab("providers"); setEditingProvider(null); setProviderFormOpen(true); }}>{t("services.page.addProvider")}<ArrowRight className="ml-2 h-4 w-4" /></Button></div>}

      <Tabs value={activeTab} onValueChange={setActiveTab} className="space-y-4">
        <div className="flex flex-col items-stretch justify-between gap-3 border-b border-border/40 pb-3 sm:flex-row sm:items-center">
          <div className="ui-scroll-affordance w-full pb-1 sm:w-auto sm:pb-0">
            <TabsList className="h-auto min-w-max justify-start rounded-lg bg-muted/50 p-1">
              <TabsTrigger
                value="services"
                className="gap-2 rounded-md px-3 py-2 text-muted-foreground transition-colors data-[state=active]:bg-background data-[state=active]:text-foreground data-[state=active]:shadow-xs sm:px-4"
              >
                <Server className="h-4 w-4" />
                {t("services.page.tabs.services")}
              </TabsTrigger>
              <TabsTrigger
                value="providers"
                className="gap-2 rounded-md px-3 py-2 text-muted-foreground transition-colors data-[state=active]:bg-background data-[state=active]:text-foreground data-[state=active]:shadow-xs sm:px-4"
              >
                <Cloud className="h-4 w-4" />
                {t("services.page.tabs.providers")}
              </TabsTrigger>
              <TabsTrigger
                value="models"
                className="gap-2 rounded-md px-3 py-2 text-muted-foreground transition-colors data-[state=active]:bg-background data-[state=active]:text-foreground data-[state=active]:shadow-xs sm:px-4"
              >
                <Cpu className="h-4 w-4" />
                {t("services.page.tabs.models")}
              </TabsTrigger>
            </TabsList>
          </div>

          <div className="flex w-full flex-col gap-2 sm:w-auto sm:flex-row sm:items-center">
            <div className="relative w-full sm:w-60">
              <Search className="pointer-events-none absolute left-2.5 top-2.5 h-4 w-4 text-muted-foreground" />
              <Input
                aria-label={t("services.page.searchPlaceholder", "Search...")}
                placeholder={t("services.page.searchPlaceholder", "Search...")}
                value={searchInput}
                onChange={(e) => setSearchInput(e.target.value)}
                className="h-9 rounded-md border-border/60 bg-background pl-9 transition-colors focus:bg-background"
              />
            </div>

            {activeTab !== "services" && <select className="h-9 rounded-md border border-border bg-background px-3 text-sm" aria-label={t("management.statusFilter")} value={statusFilter} onChange={event => setStatusFilter(event.target.value)}>
              <option value="all">{t("management.allStatuses")}</option><option value="enabled">{t("management.enabled")}</option><option value="disabled">{t("management.disabled")}</option>
            </select>}
            {activeTab === "services" && (
              <div className="w-full sm:w-auto [&_button]:w-full sm:[&_button]:w-auto">
                <ServiceForm
                  onRegistered={() => qc.invalidateQueries({ queryKey: ["services"] })}
                />
              </div>
            )}
            {activeTab === "providers" && (
              <Button
                variant="primary"
                onClick={() => {
                  setEditingProvider(null);
                  setProviderFormOpen(true);
                }}
                className="h-9 w-full px-3 sm:w-auto"
              >
                <Plus className="h-4 w-4 mr-2" />
                {t("services.page.addProvider")}
              </Button>
            )}
            {activeTab === "models" && (
              <Button
                variant="primary"
                onClick={() => {
                  setEditingModel(null);
                  setModelFormOpen(true);
                }}
                disabled={providers.length === 0}
                className="h-9 w-full px-3 sm:w-auto"
              >
                <Plus className="h-4 w-4 mr-2" />
                {t("services.page.addModel")}
              </Button>
            )}
          </div>
        </div>

        <div className="min-h-[500px]">
          {/* Services Tab */}
          <TabsContent value="services" className="mt-0 space-y-4 focus-visible:outline-hidden focus-visible:ring-0">
            {healthQuery.isError && <ManagementQueryState onRetry={() => void healthQuery.refetch()} retrying={healthQuery.isFetching} stale={Boolean(healthQuery.data)} />}
            <ArchitectureStatusPanel
              data={architectureQuery.data}
              loading={architectureQuery.isLoading}
              error={architectureQuery.isError}
              allowed={canViewArchitecture}
              onRefresh={() => void architectureQuery.refetch()}
            />
            {servicesQuery.isError && <ManagementQueryState onRetry={() => void servicesQuery.refetch()} retrying={servicesQuery.isFetching} stale={Boolean(servicesQuery.data)} />}
            {servicesQuery.isError && !servicesQuery.data ? null : servicesQuery.isLoading ? (
              <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
                {[1, 2, 3, 4, 5, 6].map((i) => (
                  <div key={i} className="h-24 rounded-lg bg-muted/20 animate-pulse" />
                ))}
              </div>
            ) : filteredServices.length === 0 ? (
              <div className="flex flex-col items-center justify-center rounded-xl border border-dashed bg-card/40 px-6 py-14 text-center">
                <div className="mb-4 flex h-12 w-12 items-center justify-center rounded-lg bg-muted/50">
                  <Server className="h-6 w-6 text-muted-foreground/60" />
                </div>
                <p className="font-medium text-foreground">{t("services.page.noServices")}</p>
                {searchQuery ? (
                  <p className="mt-1 text-sm text-muted-foreground">{t("services.search.noResults", "No services match your search.")}</p>
                ) : (
                  <p className="mt-1 text-sm text-muted-foreground">{t("services.empty.hint", "Get started by registering your first service.")}</p>
                )}
              </div>
            ) : (
              <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
                {filteredServices.map((s) => (
                  <ServiceCard
                    key={s.service_id}
                    service={s}
                    health={health[s.service_id]}
                    selected={selectedServiceId === s.service_id}
                    onSelect={() => setSelectedServiceId(s.service_id)}
                  />
                ))}
              </div>
            )}
            {selectedServiceId && services.some(s => s.service_id === selectedServiceId) && (
              <div className="fixed bottom-6 right-6 z-50">
                <div className="animate-in rounded-lg border border-border bg-card px-4 py-2 text-sm font-medium text-foreground shadow-sm slide-in-from-bottom-5 fade-in">
                  {t("services.selected", "Selected")}: {selectedServiceId}
                </div>
              </div>
            )}
          </TabsContent>

          {/* Providers Tab */}
          <TabsContent value="providers" className="mt-0 space-y-4 focus-visible:outline-hidden focus-visible:ring-0">
            {providersQuery.isError && <ManagementQueryState onRetry={() => void providersQuery.refetch()} retrying={providersQuery.isFetching} stale={Boolean(providersQuery.data)} />}
            {providersQuery.isError && !providersQuery.data ? null : providersQuery.isLoading ? (
              <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
                {[1, 2, 3, 4, 5, 6].map((i) => (
                  <div key={i} className="h-20 rounded-lg bg-muted/20 animate-pulse" />
                ))}
              </div>
            ) : filteredProviders.length === 0 ? (
              <div className="flex flex-col items-center justify-center rounded-xl border border-dashed bg-card/40 px-6 py-14 text-center">
                <div className="mb-4 flex h-12 w-12 items-center justify-center rounded-lg bg-muted/50">
                  <Cloud className="h-6 w-6 text-muted-foreground/60" />
                </div>
                <p className="font-medium text-foreground">{t("services.page.noProviders")}</p>
                {(searchQuery || statusFilter !== "all") && <Button variant="ghost" className="mt-2" onClick={() => { setSearchInput(""); setStatusFilter("all"); }}>{t("management.clearFilters")}</Button>}
              </div>
            ) : (
              <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
                {filteredProviders.map((p) => (
                  <ProviderCard
                    key={p.provider_id}
                    provider={p}
                    testResetRevision={providerTestRevisions[p.provider_id] ?? 0}
                    onEdit={() => handleEditProvider(p)}
                    onDelete={() => handleDeleteProvider(p)}
                    onSyncModels={() => setCatalogProvider(p.provider_id)}
                  />
                ))}
              </div>
            )}
          </TabsContent>

          {/* Models Tab */}
          <TabsContent value="models" className="mt-0 space-y-4 focus-visible:outline-hidden focus-visible:ring-0">
            {managedModels.notice && <div role={managedModels.notice === "saved" ? "status" : "alert"} className="rounded-lg border p-3 text-sm">
              <p>{t(`models.statusRecovery.${managedModels.notice}`)}</p>
              {managedModels.notice !== "saved" && <Button variant="outline" className="mt-2" disabled={managedModels.busy || modelsQuery.isFetching} onClick={() => void managedModels.review()}>{t("models.statusRecovery.review")}</Button>}
            </div>}
            {modelsQuery.isError && <ManagementQueryState onRetry={() => void modelsQuery.refetch()} retrying={modelsQuery.isFetching} stale={Boolean(modelsQuery.data)} />}
            {modelsQuery.isError && !modelsQuery.data ? null : modelsQuery.isLoading ? (
              <div className="space-y-4">
                {[1, 2, 3, 4, 5].map((i) => (
                  <div key={i} className="h-16 rounded-lg bg-muted/20 animate-pulse" />
                ))}
              </div>
            ) : models.length === 0 ? (
              <div className="flex flex-col items-center justify-center rounded-xl border border-dashed bg-card/40 px-6 py-14 text-center">
                <div className="mb-4 flex h-12 w-12 items-center justify-center rounded-lg bg-muted/50">
                  <Cpu className="h-6 w-6 text-muted-foreground/60" />
                </div>
                <p className="font-medium text-foreground">{t("services.page.noModels")}</p>
                {(searchQuery || statusFilter !== "all") && <Button variant="ghost" className="mt-2" onClick={() => { setSearchInput(""); setStatusFilter("all"); }}>{t("management.clearFilters")}</Button>}
              </div>
            ) : (
              <div className="overflow-hidden rounded-lg border border-border/60 bg-card">
                <ModelTable
                  models={models}
                  providers={providerMap}
                  onEdit={handleEditModel}
                  onDelete={handleDeleteModel}
                  onToggle={managedModels.toggle}
                  loading={!managedModels.ready}
                />
              </div>
            )}
          </TabsContent>
        </div>
      </Tabs>

      {/* Dialogs and Modals remain unchanged in logic but inserted here */}
      <ProviderForm
        open={providerFormOpen}
        onOpenChange={(open) => {
          setProviderFormOpen(open);
          if (!open) setEditingProvider(null);
        }}
        provider={editingProvider}
        templates={providerTemplatesQuery.data || []}
        templatesLoading={providerTemplatesQuery.isLoading}
        onSubmit={handleProviderSubmit}
        loading={createProviderMutation.isPending || updateProviderMutation.isPending}
      />

      <ModelForm
        open={modelFormOpen}
        onOpenChange={(open) => {
          setModelFormOpen(open);
          if (!open) setEditingModel(null);
        }}
        model={editingModel}
        providers={providers}
        providerTemplates={providerTemplatesQuery.data || []}
        providersLoading={providersQuery.isLoading}
        providerTemplatesLoading={providerTemplatesQuery.isLoading}
        capabilityAdapters={capabilityAdaptersQuery.data || []}
        onSubmit={handleModelSubmit}
        onResetCapabilities={
          editingModel
            ? () =>
                resetCapabilitiesMutation
                  .mutateAsync({
                    id: editingModel.model_id,
                    providerId: editingModel.provider_id,
                    capabilityRevision: editingModel.capability_revision,
                  })
                  .then(() => undefined)
            : undefined
        }
        loading={
          createModelMutation.isPending ||
          updateModelMutation.isPending ||
          resetCapabilitiesMutation.isPending
        }
      />

      <AlertDialog open={deleteProviderOpen} onOpenChange={setDeleteProviderOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("services.page.deleteProvider.title")}</AlertDialogTitle>
            <AlertDialogDescription>
              {t("services.page.deleteProvider.description", { name: providerToDelete?.display_name })}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("services.page.deleteProvider.cancel")}</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => providerToDelete && deleteProviderMutation.mutate(providerToDelete.provider_id)}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {deleteProviderMutation.isPending ? t("services.page.deleteProvider.deleting") : t("services.page.deleteProvider.confirm")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog open={deleteModelOpen} onOpenChange={setDeleteModelOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("services.page.deleteModel.title")}</AlertDialogTitle>
            <AlertDialogDescription>
              {t("services.page.deleteModel.description", { name: modelToDelete?.display_name })}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("services.page.deleteModel.cancel")}</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => modelToDelete && deleteModelMutation.mutate(modelToDelete)}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {deleteModelMutation.isPending ? t("services.page.deleteModel.deleting") : t("services.page.deleteModel.confirm")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
