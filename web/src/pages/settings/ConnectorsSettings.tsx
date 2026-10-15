/**
 * Connector catalog settings — manage connector_configs provider definitions.
 *
 * Lists providers, creates/edits definitions (client_secret is write-only),
 * toggles `enabled`, and deletes with the connected-users guard.
 */

import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Plus, Pencil, Trash2, Search, Plug, RefreshCw, ArrowLeft, ShieldCheck } from "lucide-react";
import { Link } from "react-router-dom";
import { ManagementQueryState } from "@/components/ManagementQueryState";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { Badge } from "@/components/ui/badge";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useToast } from "@/hooks/use-toast";
import { useAuthStore } from "@/store/useAuthStore";
import { getErrorMessage } from "@/lib/utils";
import {
  connectorQueryKeys,
  createConnector,
  deleteConnector,
  listConnectors,
  toggleConnector,
  updateConnector,
  type ConnectorMode,
  type ConnectorProvider,
  type ConnectorProviderCreate,
  type ConnectorProviderUpdate,
  type ConnectorMcpToolInfo,
} from "@/api/connectors";

const MODE_LABELS: Record<ConnectorMode, string> = {
  live: "modeLive",
  ingest: "modeIngest",
  both: "modeBoth",
};

interface FormState {
  provider: string;
  displayName: string;
  description: string;
  mode: ConnectorMode;
  enabled: boolean;
  supportsSync: boolean;
  supportsSearch: boolean;
  clientId: string;
  clientSecret: string;
  redirectUri: string;
  iconUrl: string | null;
  extraConfig: Record<string, unknown>;
  authUrl: string;
  tokenUrl: string;
  scopes: string;
  mcpTools: ConnectorMcpToolInfo[];
}

function toForm(row: ConnectorProvider | null): FormState {
  return {
    provider: row?.provider ?? "",
    displayName: row?.display_name ?? "",
    description: row?.description ?? "",
    mode: row?.mode ?? "live",
    enabled: row?.enabled ?? true,
    supportsSync: row?.supports_sync ?? false,
    supportsSearch: row?.supports_search ?? true,
    clientId: row?.auth?.client_id ?? "",
    clientSecret: "",
    redirectUri: row?.auth?.redirect_uri ?? "",
    iconUrl: row?.icon_url ?? null,
    extraConfig: row?.extra_config ?? {},
    authUrl: row?.auth?.auth_url ?? "",
    tokenUrl: row?.auth?.token_url ?? "",
    scopes: row?.auth?.scopes ?? "",
    mcpTools: row?.mcp_tools ?? [],
  };
}

function fromForm(form: FormState): ConnectorProviderCreate {
  const auth = {
    client_id: form.clientId,
    client_secret: form.clientSecret || null,
    auth_url: form.authUrl,
    token_url: form.tokenUrl,
    scopes: form.scopes,
    redirect_uri: form.redirectUri,
  };
  return {
    provider: form.provider.trim(),
    display_name: form.displayName.trim(),
    description: form.description || null,
    icon_url: form.iconUrl,
    extra_config: form.extraConfig,
    mode: form.mode,
    enabled: form.enabled,
    supports_sync: form.supportsSync,
    supports_search: form.supportsSearch,
    auth,
    mcp_tools: form.mcpTools,
  };
}

export function ConnectorsSettingsPage() {
  const identity = useAuthStore(state => JSON.stringify([state.user?.user_id, state.user?.roles, state.user?.permissions]));
  return <ConnectorCatalogWorkspace key={identity} />;
}

function ConnectorCatalogWorkspace() {
  const { t } = useTranslation();
  const { toast } = useToast();
  const userId = useAuthStore(state => state.user?.user_id);
  const qc = useQueryClient();
  const canEditSettings = useAuthStore((state) => state.hasPermission("console:settings:edit"));
  const isTenantAdmin = useAuthStore((state) =>
    ["admin", "tenant_admin", "superadmin", "super_admin"].some((role) => state.hasRole(role)),
  );
  const canEdit = canEditSettings && isTenantAdmin;
  const canViewServices = useAuthStore(state => state.hasPermission("console:services:view"));

  const [dialogOpen, setDialogOpen] = useState(false);
  const [search, setSearch] = useState("");
  const [status, setStatus] = useState("all");
  const [template, setTemplate] = useState<ConnectorProvider | null>(null);
  const [editing, setEditing] = useState<ConnectorProvider | null>(null);
  const [form, setForm] = useState<FormState>(() => toForm(null));

  const catalogQuery = useQuery({
    queryKey: [...connectorQueryKeys.list(), userId],
    enabled: Boolean(userId),
    gcTime: 0,
    queryFn: listConnectors,
  });

  const { data: connectors = [], isLoading } = catalogQuery;

  const invalidate = () => qc.invalidateQueries({ queryKey: connectorQueryKeys.all });

  const saveMutation = useMutation({
    mutationFn: async (formState: FormState) => {
      const payload = fromForm(formState);
      if (editing) {
        const updates: ConnectorProviderUpdate = {
          display_name: payload.display_name,
          description: payload.description,
          mode: payload.mode,
          enabled: payload.enabled,
          supports_sync: payload.supports_sync,
          supports_search: payload.supports_search,
          auth: payload.auth,
          mcp_tools: payload.mcp_tools,
        };
        return updateConnector(editing.provider, updates);
      }
      return createConnector(payload);
    },
    onSuccess: () => {
      invalidate();
      setDialogOpen(false);
      setForm(toForm(null));
      setEditing(null);
      setTemplate(null);
      toast({ title: t("settings.connectors.updateSuccess") });
    },
    onError: (error: unknown) => {
      toast({ title: getErrorMessage(error) ?? t("settings.connectors.failed"), variant: "destructive" });
    },
  });

  const toggleMutation = useMutation({
    mutationFn: (row: ConnectorProvider) => toggleConnector(row.provider, !row.enabled),
    onSuccess: () => invalidate(),
    onError: (error: unknown) => {
      toast({ title: getErrorMessage(error) ?? t("settings.connectors.failed"), variant: "destructive" });
    },
  });

  const deleteMutation = useMutation({
    mutationFn: (row: ConnectorProvider) => deleteConnector(row.provider),
    onSuccess: () => {
      invalidate();
      toast({ title: t("settings.connectors.deleteSuccess") });
    },
    onError: (error: unknown) => {
      const message = getErrorMessage(error) ?? "";
      toast({
        title: message.includes("connected users")
          ? t("settings.connectors.deleteGuard")
          : message || t("settings.connectors.failed"),
        variant: "destructive",
      });
    },
  });

  const openCreate = () => {
    setTemplate(null);
    setEditing(null);
    setForm(toForm(null));
    setDialogOpen(true);
  };

  const openOverride = (row: ConnectorProvider) => {
    setEditing(null);
    setTemplate(row);
    setForm({ ...toForm(row), enabled: false });
    setDialogOpen(true);
  };

  const openEdit = (row: ConnectorProvider) => {
    setTemplate(null);
    setEditing(row);
    setForm(toForm(row));
    setDialogOpen(true);
  };

  const set = <K extends keyof FormState>(key: K, value: FormState[K]) =>
    setForm((prev) => ({ ...prev, [key]: value }));

  const setTool = (index: number, field: keyof ConnectorMcpToolInfo, value: string) => {
    setForm((prev) => {
      const mcpTools = prev.mcpTools.map((tool, i) =>
        i === index ? { ...tool, [field]: value } : tool
      );
      return { ...prev, mcpTools };
    });
  };

  const addTool = () => set("mcpTools", [...form.mcpTools, { name: "", description: "" }]);
  const removeTool = (index: number) =>
    set(
      "mcpTools",
      form.mcpTools.filter((_, i) => i !== index)
    );

  const sorted = useMemo(
    () => connectors.filter(row => {
      const matchesSearch = `${row.provider} ${row.display_name} ${row.description || ""}`.toLocaleLowerCase().includes(search.trim().toLocaleLowerCase());
      return matchesSearch && (status === "all" || row.enabled === (status === "enabled"));
    }).sort((a, b) => a.display_name.localeCompare(b.display_name)),
    [connectors, search, status]
  );

  return (
    <div className="space-y-6">
      {canViewServices && <Link className="inline-flex items-center gap-2 text-sm text-muted-foreground hover:text-foreground" to="/services?tab=providers"><ArrowLeft size={15} />{t("management.backToServices")}</Link>}
      <div className="flex flex-wrap items-start justify-between gap-4 rounded-xl border border-border bg-card p-5">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight text-foreground">{t("settings.connectors.title")}</h1>
          <p className="mt-2 max-w-2xl text-sm leading-relaxed text-muted-foreground">{t("management.connectorsIntro")}</p>
        </div>
        {canEdit ? (
          <Button onClick={openCreate} disabled={saveMutation.isPending || toggleMutation.isPending || deleteMutation.isPending}>
            <Plus className="mr-2 h-4 w-4" />
            {t("settings.connectors.newConnector")}
          </Button>
        ) : null}
      </div>

      <div className="grid gap-3 sm:grid-cols-3">
        {[
          { label: "catalog", value: catalogQuery.data ? connectors.length : "—" },
          { label: "enabled", value: catalogQuery.data ? connectors.filter(row => row.enabled).length : "—" },
          { label: "searchable", value: catalogQuery.data ? connectors.filter(row => row.enabled && row.supports_search).length : "—" },
        ].map(metric => <div className="rounded-xl border border-border bg-card p-4" key={metric.label}><p className="text-xs text-muted-foreground">{t(`management.${metric.label}`)}</p><strong className="mt-3 block text-2xl font-semibold tabular-nums">{metric.value}</strong></div>)}
      </div>
      <div className="flex items-start gap-3 rounded-lg border border-border bg-muted/30 p-4"><ShieldCheck className="mt-0.5 h-4 w-4 shrink-0 text-primary" /><p className="text-sm text-muted-foreground">{t(canEdit ? "management.catalogNotice" : "management.readOnlyNotice")}</p></div>
      <Card>
        <CardHeader>
          <CardTitle className="text-lg text-foreground">{t("settings.connectors.listTitle")}</CardTitle>
          <CardDescription>{t("management.catalogDescription")}</CardDescription>
          <div className="flex flex-wrap items-center gap-2 pt-3">
            <div className="relative min-w-0 flex-1"><Search className="pointer-events-none absolute left-3 top-2.5 h-4 w-4 text-muted-foreground" /><Input value={search} onChange={event => setSearch(event.target.value)} aria-label={t("management.searchConnectors")} placeholder={t("management.searchConnectors")} className="pl-9" /></div>
            <select className="h-9 rounded-md border border-border bg-background px-3 text-sm" aria-label={t("management.statusFilter")} value={status} onChange={event => setStatus(event.target.value)}><option value="all">{t("management.allStatuses")}</option><option value="enabled">{t("management.enabled")}</option><option value="disabled">{t("management.disabled")}</option></select>
            <Button variant="outline" size="icon" aria-label={t("management.refresh")} onClick={() => void catalogQuery.refetch()} disabled={catalogQuery.isFetching}><RefreshCw size={16} className={catalogQuery.isFetching ? "animate-spin" : undefined} /></Button>
          </div>
        </CardHeader>
        <CardContent>
          {catalogQuery.isError && <ManagementQueryState onRetry={() => void catalogQuery.refetch()} retrying={catalogQuery.isFetching} stale={Boolean(catalogQuery.data)} />}
          {catalogQuery.isError && !catalogQuery.data ? null : isLoading ? (
            <p className="text-sm text-muted-foreground">{t("common.loading")}</p>
          ) : sorted.length === 0 ? (
            <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border p-10 text-center"><Plug size={28} className="text-muted-foreground" /><p className="text-sm text-muted-foreground">{t(search || status !== "all" ? "management.noMatches" : "settings.connectors.empty")}</p>{search || status !== "all" ? <Button variant="outline" onClick={() => { setSearch(""); setStatus("all"); }}>{t("management.clearFilters")}</Button> : canEdit && <Button onClick={openCreate}>{t("settings.connectors.newConnector")}</Button>}</div>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>{t("settings.connectors.column.provider")}</TableHead>
                  <TableHead>{t("settings.connectors.column.displayName")}</TableHead>
                  <TableHead>{t("settings.connectors.column.mode")}</TableHead>
                  <TableHead>{t("settings.connectors.column.status")}</TableHead>
                  <TableHead className="text-right">{t("settings.connectors.column.actions")}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {sorted.map((row) => (
                  <TableRow key={row.provider}>
                    <TableCell className="font-medium text-foreground"><span className="font-mono">{row.provider}</span><Badge variant="outline" className="mt-1 block w-fit">{t(row.tenant_id === "" ? "connectorTemplates.global" : "connectorTemplates.tenant")}</Badge></TableCell>
                    <TableCell className="text-foreground"><span className="font-medium">{row.display_name}</span>{row.description && <p className="mt-1 max-w-sm text-xs text-muted-foreground">{row.description}</p>}</TableCell>
                    <TableCell>
                      <Badge variant="outline" className="text-foreground">{t(`settings.connectors.${MODE_LABELS[row.mode]}`)}</Badge>
                    </TableCell>
                    <TableCell>
                      <Switch
                        checked={row.enabled}
                        disabled={!canEdit || row.tenant_id === "" || saveMutation.isPending || toggleMutation.isPending || deleteMutation.isPending}
                        onCheckedChange={() => canEdit && row.tenant_id !== "" && toggleMutation.mutate(row)}
                        aria-label={`${t("settings.connectors.enabled")}: ${row.display_name}`}
                      />
                    </TableCell>
                    <TableCell className="text-right">
                      <div className="flex justify-end gap-1">
                        {canEdit && row.tenant_id === "" ? (
                          <Button variant="outline" size="sm" disabled={saveMutation.isPending || toggleMutation.isPending || deleteMutation.isPending} onClick={() => openOverride(row)}>{t("connectorTemplates.createOverride")}</Button>
                        ) : canEdit ? (
                        <>
                        <Button
                          variant="ghost"
                          size="sm"
                          disabled={saveMutation.isPending || toggleMutation.isPending || deleteMutation.isPending}
                          onClick={() => openEdit(row)}
                          aria-label={`${t("settings.connectors.editConnector")}: ${row.display_name}`}
                          title={t("settings.connectors.editConnector")}
                        >
                          <Pencil className="h-4 w-4" />
                        </Button>
                        <AlertDialog>
                          <AlertDialogTrigger asChild>
                            <Button
                              variant="ghost"
                              size="sm"
                              disabled={saveMutation.isPending || toggleMutation.isPending || deleteMutation.isPending}
                              aria-label={`${t("settings.connectors.deleteConnector")}: ${row.display_name}`}
                              title={t("settings.connectors.deleteConnector")}
                            >
                              <Trash2 className="h-4 w-4" />
                            </Button>
                          </AlertDialogTrigger>
                          <AlertDialogContent>
                            <AlertDialogHeader>
                              <AlertDialogTitle>{t("settings.connectors.deleteConfirm")}</AlertDialogTitle>
                              <AlertDialogDescription>
                                {row.display_name} ({row.provider})
                                <span className="mt-2 block">{t("connectorTemplates.deleteNotice")}</span>
                              </AlertDialogDescription>
                            </AlertDialogHeader>
                            <AlertDialogFooter>
                              <AlertDialogCancel>{t("settings.connectors.cancel")}</AlertDialogCancel>
                              <AlertDialogAction disabled={deleteMutation.isPending} onClick={() => deleteMutation.mutate(row)}>
                                {t("settings.connectors.deleteConnector")}
                              </AlertDialogAction>
                            </AlertDialogFooter>
                          </AlertDialogContent>
                        </AlertDialog>
                        </>
                        ) : null}
                      </div>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>

      <Dialog open={dialogOpen} onOpenChange={(open) => { if (saveMutation.isPending) return; setDialogOpen(open); if (!open) { setForm(toForm(null)); setEditing(null); setTemplate(null); } }}>
        <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>
              {template ? t("connectorTemplates.createOverride") : editing ? t("settings.connectors.editConnector") : t("settings.connectors.newConnector")}
            </DialogTitle>
            <DialogDescription>{t(template ? "connectorTemplates.overrideNotice" : "settings.connectors.description")}</DialogDescription>
          </DialogHeader>
          <form id="connector-catalog-form" className="space-y-4" onSubmit={(event) => { event.preventDefault(); if (canEdit && !saveMutation.isPending && form.provider.trim() && form.displayName.trim()) saveMutation.mutate(form); }}>
            <div className="space-y-2">
              <Label htmlFor="connector-provider">{t("settings.connectors.field.provider")}</Label>
              <Input id="connector-provider" required
                value={form.provider}
                disabled={!!editing || !!template}
                placeholder={t("settings.connectors.field.providerPlaceholder")}
                onChange={(e) => set("provider", e.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="connector-displayName">{t("settings.connectors.field.displayName")}</Label>
              <Input id="connector-displayName" required value={form.displayName} onChange={(e) => set("displayName", e.target.value)} />
            </div>
            <div className="space-y-2">
              <Label htmlFor="connector-description">{t("settings.connectors.field.description")}</Label>
              <Textarea id="connector-description"
                value={form.description}
                onChange={(e) => set("description", e.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="connector-mode">{t("settings.connectors.field.mode")}</Label>
              <Select value={form.mode} onValueChange={(v) => set("mode", v as ConnectorMode)}>
                <SelectTrigger id="connector-mode" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {(Object.keys(MODE_LABELS) as ConnectorMode[]).map((mode) => (
                    <SelectItem key={mode} value={mode}>
                      {t(`settings.connectors.${MODE_LABELS[mode]}`)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <p className="text-xs text-muted-foreground">{t("settings.connectors.field.modeHint")}</p>
            </div>
            <div className="flex flex-wrap items-center gap-4">
              <Label className="flex items-center gap-2">
                <Switch checked={form.enabled} onCheckedChange={(v) => set("enabled", v)} />
                {t("settings.connectors.field.enabled")}
              </Label>
              <Label className="flex items-center gap-2">
                <Switch checked={form.supportsSync} onCheckedChange={(v) => set("supportsSync", v)} />
                {t("settings.connectors.field.supportsSync")}
              </Label>
              <Label className="flex items-center gap-2">
                <Switch checked={form.supportsSearch} onCheckedChange={(v) => set("supportsSearch", v)} />
                {t("settings.connectors.field.supportsSearch")}
              </Label>
            </div>

            <div className="space-y-2 border-t pt-4">
              <h4 className="text-sm font-medium">{t("settings.connectors.field.authUrl")}</h4>
              <div className="space-y-2">
                <div className="grid grid-cols-2 gap-2">
                  <div className="space-y-1">
                    <Label htmlFor="connector-clientId">{t("settings.connectors.field.clientId")}</Label>
                    <Input id="connector-clientId" value={form.clientId} onChange={(e) => set("clientId", e.target.value)} />
                  </div>
                  <div className="space-y-1">
                    <Label htmlFor="connector-clientSecret">{t("settings.connectors.field.clientSecret")}</Label>
                    <Input id="connector-clientSecret" autoComplete="new-password"
                      type="password"
                      value={form.clientSecret}
                      placeholder={editing ? t("settings.connectors.field.clientSecretPlaceholder") : ""}
                      onChange={(e) => set("clientSecret", e.target.value)}
                    />
                  </div>
                </div>
                <div className="space-y-1">
                  <Label htmlFor="connector-authUrl">{t("settings.connectors.field.authUrl")}</Label>
                  <Input id="connector-authUrl" type="url" value={form.authUrl} onChange={(e) => set("authUrl", e.target.value)} />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="connector-tokenUrl">{t("settings.connectors.field.tokenUrl")}</Label>
                  <Input id="connector-tokenUrl" type="url" value={form.tokenUrl} onChange={(e) => set("tokenUrl", e.target.value)} />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="connector-redirectUri">{t("connectorTemplates.redirectUri")}</Label>
                  <Input id="connector-redirectUri" type="url" value={form.redirectUri} onChange={(e) => set("redirectUri", e.target.value)} />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="connector-scopes">{t("settings.connectors.field.scopes")}</Label>
                  <Input id="connector-scopes" value={form.scopes} onChange={(e) => set("scopes", e.target.value)} />
                </div>
              </div>
            </div>

            <div className="space-y-2 border-t pt-4">
              <div className="flex items-center justify-between">
                <h4 className="text-sm font-medium">{t("settings.connectors.field.mcpTools")}</h4>
                <Button type="button" variant="outline" size="sm" onClick={addTool}>
                  <Plus className="mr-1 h-3 w-3" />
                  {t("settings.connectors.newConnector")}
                </Button>
              </div>
              <p className="text-xs text-muted-foreground">{t("settings.connectors.field.mcpToolsHint")}</p>
              {form.mcpTools.map((tool, index) => (
                <div key={index} className="grid grid-cols-[1fr_auto] gap-2">
                  <div className="grid grid-cols-2 gap-2">
                    <Input
                      aria-label={t("settings.connectors.field.toolName")} value={tool.name ?? ""}
                      placeholder={t("settings.connectors.field.toolName")}
                      onChange={(e) => setTool(index, "name", e.target.value)}
                    />
                    <Input
                      aria-label={t("settings.connectors.field.toolDescription")} value={tool.description ?? ""}
                      placeholder={t("settings.connectors.field.toolDescription")}
                      onChange={(e) => setTool(index, "description", e.target.value)}
                    />
                  </div>
                  <Button type="button" variant="ghost" size="sm" onClick={() => removeTool(index)} aria-label={t("management.removeTool", { index: index + 1 })}>
                    <Trash2 className="h-4 w-4" />
                  </Button>
                </div>
              ))}
            </div>
          </form>
          <DialogFooter>
            <Button variant="outline" disabled={saveMutation.isPending} onClick={() => { setDialogOpen(false); setForm(toForm(null)); setEditing(null); setTemplate(null); }}>
              {t("settings.connectors.cancel")}
            </Button>
            <Button type="submit" form="connector-catalog-form" disabled={!canEdit || saveMutation.isPending || !form.provider.trim() || !form.displayName.trim()}>
              {saveMutation.isPending ? t("settings.connectors.saving") : t("settings.connectors.save")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
