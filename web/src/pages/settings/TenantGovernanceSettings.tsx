import { useRef, useState, type FormEvent } from "react";
import { Link, Navigate } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { ArrowLeft, Plus, RefreshCw, ShieldCheck } from "lucide-react";
import { getToolAuditLog, listTenantToolPolicies, saveTenantToolPolicy, type TenantToolPolicy, type ToolAuditFilters } from "@/api/tenantGovernance";
import { canAdministerTenantPolicies, policyDraft, validatePolicyDraft, type ToolPolicyDraft, type ToolPolicyPayload } from "@/lib/tenantGovernance";
import { useAuthStore } from "@/store/useAuthStore";
import { TenantMcpConfigs } from "./TenantMcpConfigs";
import { ConfigChangeAudit } from "./ConfigChangeAudit";
import { ManagementQueryState } from "@/components/ManagementQueryState";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";

const listFields = ["allowed_tools", "blocked_tools", "allowed_categories"] as const;
const blankFilters: ToolAuditFilters = { tenant_id: "", user_id: "", tool_name: "" };
const panel = "rounded-xl border border-border bg-card p-4 sm:p-5";

function PolicyEditor({ initial, onClose, onSaved }: { initial?: TenantToolPolicy; onClose: () => void; onSaved: (tenant: string) => void }) {
  const { t } = useTranslation();
  const client = useQueryClient();
  const [draft, setDraft] = useState<ToolPolicyDraft>(() => policyDraft(initial));
  const [review, setReview] = useState<ToolPolicyPayload | null>(null);
  const [validation, setValidation] = useState<string | null>(null);
  const submitting = useRef(false);
  const mutation = useMutation({
    mutationFn: saveTenantToolPolicy,
    retry: false,
    onSuccess: policy => onSaved(policy.tenant_id),
    onSettled: () => {
      submitting.current = false;
      // A failed write response can follow a committed write. Reload authority either way.
      void client.invalidateQueries({ queryKey: ["tenant-tool-policies"] });
    },
  });
  const update = (key: keyof ToolPolicyDraft, value: string) => {
    setDraft(previous => ({ ...previous, [key]: value }));
    setValidation(null);
    mutation.reset();
  };
  const prepare = (event: FormEvent) => {
    event.preventDefault();
    const result = validatePolicyDraft(draft);
    if (result.error) { setValidation(t(`tenantGovernance.validation.${result.error}`)); return; }
    setReview(result.payload);
  };
  const save = () => {
    if (!review || submitting.current) return;
    submitting.current = true;
    mutation.mutate(review);
  };
  return <Dialog open onOpenChange={open => { if (!open && !submitting.current) onClose(); }}>
    <DialogContent showCloseButton={!mutation.isPending}>
      <DialogHeader><DialogTitle>{t(review ? "tenantGovernance.reviewTitle" : "tenantGovernance.editorTitle")}</DialogTitle><DialogDescription>{t("tenantGovernance.writeNotice")}</DialogDescription></DialogHeader>
      {review ? <div className="space-y-4">
        <dl className="space-y-3 text-sm">{Object.entries(review).map(([key, value]) => <div key={key}><dt className="text-muted-foreground">{t(`tenantGovernance.fields.${key}`)}</dt><dd className="mt-1 break-all font-mono text-xs">{typeof value === "string" ? value : JSON.stringify(value)}</dd></div>)}</dl>
        {mutation.isError && <p role="alert" className="rounded-md bg-destructive/10 p-3 text-sm text-destructive">{t("tenantGovernance.saveError")}</p>}
        <DialogFooter><Button variant="outline" disabled={mutation.isPending} onClick={() => { setReview(null); mutation.reset(); }}>{t("tenantGovernance.backToEdit")}</Button><Button onClick={save} disabled={mutation.isPending || mutation.isError}>{t(mutation.isPending ? "tenantGovernance.saving" : "tenantGovernance.confirmSave")}</Button></DialogFooter>
      </div> : <form onSubmit={prepare} className="space-y-4">
        <div className="space-y-2"><Label htmlFor="policy-tenant">{t("tenantGovernance.fields.tenant_id")}</Label><Input id="policy-tenant" value={draft.tenant_id} onChange={event => update("tenant_id", event.target.value)} required readOnly={Boolean(initial)} /><p className="text-xs text-muted-foreground">{t("tenantGovernance.tenantHint")}</p></div>
        <p className="text-xs leading-relaxed text-muted-foreground">{t("tenantGovernance.listHint")}</p>
        {listFields.map(field => <div key={field} className="space-y-2"><Label htmlFor={`policy-${field}`}>{t(`tenantGovernance.fields.${field}`)}</Label><textarea id={`policy-${field}`} value={draft[field]} onChange={event => update(field, event.target.value)} rows={2} spellCheck={false} className="w-full rounded-md border border-input bg-background p-2 font-mono text-xs focus-visible:outline-2 focus-visible:outline-ring" /></div>)}
        <div className="space-y-2"><Label htmlFor="policy-rate">{t("tenantGovernance.fields.max_calls_per_minute")}</Label><Input id="policy-rate" type="number" min={1} max={10000} step={1} required value={draft.max_calls_per_minute} onChange={event => update("max_calls_per_minute", event.target.value)} /></div>
        {validation && <p role="alert" className="text-sm text-destructive">{validation}</p>}
        <DialogFooter><Button type="button" variant="outline" onClick={onClose}>{t("tenantGovernance.cancel")}</Button><Button type="submit">{t("tenantGovernance.review")}</Button></DialogFooter>
      </form>}
    </DialogContent>
  </Dialog>;
}

function ToolPolicies({ identity }: { identity: string }) {
  const { t } = useTranslation();
  const [search, setSearch] = useState("");
  const [editor, setEditor] = useState<{ policy?: TenantToolPolicy } | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const query = useQuery({ queryKey: ["tenant-tool-policies", identity], queryFn: ({ signal }) => listTenantToolPolicies(signal), retry: false, gcTime: 0 });
  const policies = (query.data ?? []).filter(policy => policy.tenant_id.toLocaleLowerCase().includes(search.toLocaleLowerCase()));
  return <section className="space-y-4" aria-busy={query.isFetching}>
    <div className="flex flex-wrap gap-3"><Input className="min-w-0 flex-1" value={search} onChange={event => setSearch(event.target.value)} placeholder={t("tenantGovernance.searchTenant")} aria-label={t("tenantGovernance.searchTenant")} /><Button variant="outline" disabled={query.isFetching} onClick={() => void query.refetch()}><RefreshCw className="mr-2 h-4 w-4" />{t("management.refresh")}</Button><Button disabled={!query.data || query.isError || query.isFetching} onClick={() => { setSaved(null); setEditor({}); }}><Plus className="mr-2 h-4 w-4" />{t("tenantGovernance.setPolicy")}</Button></div>
    {saved && <p role="status" className="rounded-lg border border-border bg-muted/30 p-3 text-sm">{t("tenantGovernance.saved", { tenant: saved })}</p>}
    {query.isError && <ManagementQueryState stale={Boolean(query.data)} retrying={query.isFetching} onRetry={() => void query.refetch()} />}
    {query.isPending ? <p role="status" className={panel}>{t("common.loading")}</p> : query.isError && !query.data ? null : policies.length ? <div className="overflow-hidden rounded-xl border border-border bg-card"><Table><TableHeader><TableRow><TableHead>{t("tenantGovernance.fields.tenant_id")}</TableHead>{listFields.map(field => <TableHead key={field}>{t(`tenantGovernance.fields.${field}`)}</TableHead>)}<TableHead>{t("tenantGovernance.fields.max_calls_per_minute")}</TableHead><TableHead>{t("tenantGovernance.actions")}</TableHead></TableRow></TableHeader><TableBody>{policies.map(policy => <TableRow key={policy.tenant_id}>
      <TableCell className="font-mono text-xs">{policy.tenant_id}</TableCell>{listFields.map(field => <TableCell key={field} className="max-w-64 break-words font-mono text-xs">{JSON.stringify(policy[field])}</TableCell>)}<TableCell>{policy.max_calls_per_minute ?? "—"}</TableCell><TableCell><Button variant="outline" size="sm" disabled={query.isError || query.isFetching} aria-label={t("tenantGovernance.editTenant", { tenant: policy.tenant_id })} onClick={() => { setSaved(null); setEditor({ policy }); }}>{t("tenantGovernance.edit")}</Button></TableCell>
    </TableRow>)}</TableBody></Table></div> : <div className={panel}><h2 className="font-medium">{t(search ? "tenantGovernance.noMatches" : "tenantGovernance.noPolicies")}</h2><p className="mt-2 text-sm text-muted-foreground">{t("tenantGovernance.emptyPolicyHint")}</p></div>}
    {editor && <PolicyEditor initial={editor.policy} onClose={() => setEditor(null)} onSaved={tenant => { setSaved(tenant); setEditor(null); }} />}
  </section>;
}

function ToolAudit({ identity }: { identity: string }) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState<ToolAuditFilters>(blankFilters);
  const [filters, setFilters] = useState<ToolAuditFilters>(blankFilters);
  const [offset, setOffset] = useState(0);
  const query = useQuery({ queryKey: ["tenant-tool-audit", identity, filters, offset], queryFn: ({ signal }) => getToolAuditLog(filters, offset, signal), retry: false, gcTime: 0 });
  const apply = (event: FormEvent) => { event.preventDefault(); setOffset(0); setFilters({ ...draft }); };
  return <section className="space-y-4" aria-busy={query.isFetching}>
    <p className="text-sm leading-relaxed text-muted-foreground">{t("tenantGovernance.auditNotice")}</p>
    <form onSubmit={apply} className={`${panel} grid gap-3 sm:grid-cols-3`}>
      {(["tenant_id", "user_id", "tool_name"] as const).map(field => <div key={field} className="space-y-2"><Label htmlFor={`audit-${field}`}>{t(`tenantGovernance.fields.${field}`)}</Label><Input id={`audit-${field}`} value={draft[field]} onChange={event => setDraft(previous => ({ ...previous, [field]: event.target.value }))} /></div>)}
      <div className="flex flex-wrap gap-2 sm:col-span-3"><Button type="submit" disabled={query.isFetching}>{t("tenantGovernance.applyFilters")}</Button><Button type="button" variant="outline" onClick={() => { setDraft(blankFilters); setFilters(blankFilters); setOffset(0); }}>{t("management.clearFilters")}</Button><Button type="button" variant="outline" disabled={query.isFetching} onClick={() => void query.refetch()}><RefreshCw className="mr-2 h-4 w-4" />{t("management.refresh")}</Button></div>
    </form>
    {query.isError && <ManagementQueryState stale={Boolean(query.data)} retrying={query.isFetching} onRetry={() => void query.refetch()} />}
    {query.isPending ? <p role="status" className={panel}>{t("common.loading")}</p> : query.isError && !query.data ? null : query.data && <>
      <div className="flex flex-wrap items-center justify-between gap-3 text-sm"><span role="status">{t("tenantGovernance.auditCount", { total: query.data.total, page: Math.floor(offset / 25) + 1 })}</span><div className="flex gap-2"><Button variant="outline" size="sm" disabled={!offset || query.isFetching} onClick={() => setOffset(previous => Math.max(0, previous - 25))}>{t("tenantGovernance.previous")}</Button><Button variant="outline" size="sm" disabled={query.isFetching || query.isError || offset + 25 >= query.data.total} onClick={() => setOffset(previous => previous + 25)}>{t("tenantGovernance.next")}</Button></div></div>
      {query.data.items.length ? <div className="overflow-hidden rounded-xl border border-border bg-card"><Table><TableHeader><TableRow>{["created_at", "tenant_id", "user_id", "tool_name", "output_status", "latency_ms", "request_id"].map(field => <TableHead key={field}>{t(`tenantGovernance.fields.${field}`)}</TableHead>)}</TableRow></TableHeader><TableBody>{query.data.items.map((entry, index) => <TableRow key={`${entry.id}:${index}`}><TableCell className="whitespace-nowrap text-xs">{entry.created_at ?? "—"}</TableCell><TableCell className="max-w-48 break-all font-mono text-xs">{entry.tenant_id}</TableCell><TableCell className="max-w-48 break-all font-mono text-xs">{entry.user_id}</TableCell><TableCell className="max-w-64 break-all text-xs"><span className="font-medium">{entry.tool_name}</span><span className="mt-1 block text-muted-foreground">{entry.tool_type}</span></TableCell><TableCell>{entry.output_status}</TableCell><TableCell>{entry.latency_ms ?? "—"}</TableCell><TableCell className="max-w-48 break-all font-mono text-xs">{entry.request_id ?? "—"}</TableCell></TableRow>)}</TableBody></Table></div> : <p className={panel}>{t("tenantGovernance.noAudit")}</p>}
    </>}
  </section>;
}

function GovernanceContent({ identity }: { identity: string }) {
  const { t } = useTranslation();
  return <div className="mx-auto max-w-6xl space-y-5 pb-6">
    <Link to="/settings" className="inline-flex items-center gap-2 text-sm text-muted-foreground hover:text-foreground"><ArrowLeft size={15} />{t("tenantGovernance.back")}</Link>
    <header className={panel}><p className="flex items-center gap-2 text-xs font-medium uppercase tracking-wider text-primary"><ShieldCheck size={14} />{t("tenantGovernance.eyebrow")}</p><h1 className="mt-3 text-2xl font-semibold tracking-tight">{t("tenantGovernance.title")}</h1><p className="mt-2 max-w-3xl text-sm leading-relaxed text-muted-foreground">{t("tenantGovernance.description")}</p></header>
    <p className="rounded-lg border border-border bg-muted/30 p-4 text-sm leading-relaxed text-muted-foreground">{t("tenantGovernance.scopeNotice")}</p>
    <Tabs defaultValue="policies" className="space-y-4"><TabsList className="h-auto flex-wrap"><TabsTrigger value="policies">{t("tenantGovernance.policiesTab")}</TabsTrigger><TabsTrigger value="mcp">{t("tenantMcp.tab")}</TabsTrigger><TabsTrigger value="audit">{t("tenantGovernance.auditTab")}</TabsTrigger><TabsTrigger value="config-audit">{t("configAudit.tab")}</TabsTrigger></TabsList><TabsContent value="policies"><ToolPolicies identity={identity} /></TabsContent><TabsContent value="mcp"><TenantMcpConfigs identity={identity} /></TabsContent><TabsContent value="audit"><ToolAudit identity={identity} /></TabsContent><TabsContent value="config-audit"><ConfigChangeAudit identity={identity} /></TabsContent></Tabs>
  </div>;
}

export function TenantGovernanceSettingsPage() {
  const user = useAuthStore(state => state.user);
  if (!canAdministerTenantPolicies(user)) return <Navigate to="/403" replace />;
  // Authorization changes reset editors and query state before another account can reuse them.
  const identity = JSON.stringify([user!.user_id, user!.roles, user!.tier]);
  return <GovernanceContent key={identity} identity={identity} />;
}
