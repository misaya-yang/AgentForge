import { useRef, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Plus, RefreshCw } from "lucide-react";
import { listTenantMcpConfigs, saveTenantMcpConfig } from "@/api/tenantMcpGovernance";
import { mcpConfigDraft, validateMcpConfigDraft, type McpConfigDraft, type TenantMcpConfig } from "@/lib/tenantMcpGovernance";
import { ManagementQueryState } from "@/components/ManagementQueryState";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";

function McpEditor({ initial, identity, onClose, onSaved }: { identity: string; initial?: TenantMcpConfig; onClose: () => void; onSaved: (tenant: string) => void }) {
  const { t } = useTranslation();
  const client = useQueryClient();
  const [draft, setDraft] = useState<McpConfigDraft>(() => mcpConfigDraft(initial));
  const [review, setReview] = useState<TenantMcpConfig | null>(null);
  const [validation, setValidation] = useState<string | null>(null);
  const submitting = useRef(false);
  const mutation = useMutation({
    mutationFn: saveTenantMcpConfig,
    retry: false,
    onSuccess: config => onSaved(config.tenant_id),
    onSettled: () => {
      submitting.current = false;
      void client.invalidateQueries({ queryKey: ["tenant-mcp-configs"] });
    },
  });
  const update = (key: keyof McpConfigDraft, value: string) => { setDraft(previous => ({ ...previous, [key]: value })); setValidation(null); };
  const prepare = (event: FormEvent) => {
    event.preventDefault();
    const result = validateMcpConfigDraft(draft, initial);
    if (result.error) { setValidation(t(`tenantMcp.validation.${result.error}`)); return; }
    setReview(result.payload);
  };
  const save = () => {
    if (!review || submitting.current || mutation.isError) return;
    submitting.current = true;
    mutation.mutate({ payload: review, baseline: initial ?? null, identity });
  };
  return <Dialog open onOpenChange={open => { if (!open && !submitting.current) onClose(); }}>
    <DialogContent showCloseButton={!mutation.isPending}>
      <DialogHeader><DialogTitle>{t(review ? "tenantMcp.reviewTitle" : "tenantMcp.editorTitle")}</DialogTitle><DialogDescription>{t("tenantMcp.writeNotice")}</DialogDescription></DialogHeader>
      {review ? <div className="space-y-4">
        <dl className="space-y-3 text-sm">{(["tenant_id", "allowed_servers", "max_connections"] as const).map(field => <div key={field}><dt className="text-muted-foreground">{t(`tenantMcp.fields.${field}`)}</dt><dd className="mt-1 break-all font-mono text-xs">{typeof review[field] === "string" ? review[field] : JSON.stringify(review[field])}</dd></div>)}</dl>
        <p className="text-xs text-muted-foreground">{t("tenantMcp.overridesPreserved")}</p>
        {mutation.isError && <p role="alert" className="rounded-md bg-destructive/10 p-3 text-sm text-destructive">{t("tenantMcp.saveError")}</p>}
        <DialogFooter>{mutation.isError ? <Button variant="outline" onClick={onClose}>{t("tenantMcp.closeRefresh")}</Button> : <><Button variant="outline" disabled={mutation.isPending} onClick={() => setReview(null)}>{t("tenantGovernance.backToEdit")}</Button><Button onClick={save} disabled={mutation.isPending}>{t(mutation.isPending ? "tenantGovernance.saving" : "tenantGovernance.confirmSave")}</Button></>}</DialogFooter>
      </div> : <form onSubmit={prepare} className="space-y-4">
        <div className="space-y-2"><Label htmlFor="mcp-tenant">{t("tenantMcp.fields.tenant_id")}</Label><Input id="mcp-tenant" value={draft.tenant_id} onChange={event => update("tenant_id", event.target.value)} required readOnly={Boolean(initial)} /><p className="text-xs text-muted-foreground">{t("tenantGovernance.tenantHint")}</p></div>
        <div className="space-y-2"><Label htmlFor="mcp-servers">{t("tenantMcp.fields.allowed_servers")}</Label><textarea id="mcp-servers" rows={3} value={draft.allowed_servers} onChange={event => update("allowed_servers", event.target.value)} spellCheck={false} className="w-full rounded-md border border-input bg-background p-2 font-mono text-xs focus-visible:outline-2 focus-visible:outline-ring" /><p className="text-xs text-muted-foreground">{t("tenantMcp.serversHint")}</p></div>
        <div className="space-y-2"><Label htmlFor="mcp-connections">{t("tenantMcp.fields.max_connections")}</Label><Input id="mcp-connections" type="number" min={1} max={100} step={1} required value={draft.max_connections} onChange={event => update("max_connections", event.target.value)} /></div>
        <p className="text-xs text-muted-foreground">{t("tenantMcp.overridesPreserved")}</p>
        {validation && <p role="alert" className="text-sm text-destructive">{validation}</p>}
        <DialogFooter><Button type="button" variant="outline" onClick={onClose}>{t("tenantGovernance.cancel")}</Button><Button type="submit">{t("tenantGovernance.review")}</Button></DialogFooter>
      </form>}
    </DialogContent>
  </Dialog>;
}

export function TenantMcpConfigs({ identity }: { identity: string }) {
  const { t } = useTranslation();
  const [search, setSearch] = useState("");
  const [editor, setEditor] = useState<{ config?: TenantMcpConfig } | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const query = useQuery({ queryKey: ["tenant-mcp-configs", identity], queryFn: ({ signal }) => listTenantMcpConfigs(signal), retry: false, gcTime: 0 });
  const configs = (query.data ?? []).filter(config => config.tenant_id.toLocaleLowerCase().includes(search.toLocaleLowerCase()));
  const unavailable = !query.data || query.isError || query.isFetching;
  return <section className="space-y-4" aria-busy={query.isFetching}>
    <p className="text-sm leading-relaxed text-muted-foreground">{t("tenantMcp.notice")}</p>
    <div className="flex flex-wrap gap-3"><Input className="min-w-0 flex-1" value={search} onChange={event => setSearch(event.target.value)} placeholder={t("tenantGovernance.searchTenant")} aria-label={t("tenantGovernance.searchTenant")} /><Button variant="outline" disabled={query.isFetching} onClick={() => void query.refetch()}><RefreshCw className="mr-2 h-4 w-4" />{t("management.refresh")}</Button><Button disabled={unavailable} onClick={() => { setSaved(null); setEditor({}); }}><Plus className="mr-2 h-4 w-4" />{t("tenantMcp.setConfig")}</Button></div>
    {saved && <p role="status" className="rounded-lg border border-border bg-muted/30 p-3 text-sm">{t("tenantMcp.saved", { tenant: saved })}</p>}
    {query.isError && <ManagementQueryState stale={Boolean(query.data)} retrying={query.isFetching} onRetry={() => void query.refetch()} />}
    {query.isPending ? <p role="status">{t("common.loading")}</p> : query.isError && !query.data ? null : configs.length ? <div className="overflow-hidden rounded-xl border border-border bg-card"><Table><TableHeader><TableRow>{(["tenant_id", "allowed_servers", "max_connections"] as const).map(field => <TableHead key={field}>{t(`tenantMcp.fields.${field}`)}</TableHead>)}<TableHead>{t("tenantGovernance.actions")}</TableHead></TableRow></TableHeader><TableBody>{configs.map(config => <TableRow key={config.tenant_id}><TableCell className="font-mono text-xs">{config.tenant_id}</TableCell><TableCell className="max-w-64 break-words font-mono text-xs">{JSON.stringify(config.allowed_servers)}</TableCell><TableCell>{config.max_connections}</TableCell><TableCell><Button variant="outline" size="sm" disabled={unavailable} aria-label={t("tenantGovernance.editTenant", { tenant: config.tenant_id })} onClick={() => { setSaved(null); setEditor({ config }); }}>{t("tenantGovernance.edit")}</Button></TableCell></TableRow>)}</TableBody></Table></div> : <p className="rounded-xl border border-border bg-card p-5">{t(search ? "tenantGovernance.noMatches" : "tenantMcp.empty")}</p>}
    {editor && <McpEditor identity={identity} initial={editor.config} onClose={() => setEditor(null)} onSaved={tenant => { setSaved(tenant); setEditor(null); }} />}
  </section>;
}
