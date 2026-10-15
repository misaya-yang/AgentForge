import { useState, type FormEvent } from "react";
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { getConfigAuditLog, type ConfigAuditFilters } from "@/api/configAudit";
import { ManagementQueryState } from "@/components/ManagementQueryState";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";

const emptyFilters: ConfigAuditFilters = { tenant_id: "", user_id: "", resource_type: "", resource_id: "", action: "" };
const fields = ["tenant_id", "user_id", "resource_type", "resource_id", "action"] as const;
const columns = ["created_at", ...fields, "status"] as const;
const panel = "rounded-xl border border-border bg-card p-4 sm:p-5";

/** Mounted only inside the exact administrator-gated governance page. */
export function ConfigChangeAudit({ identity }: { identity: string }) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState<ConfigAuditFilters>(emptyFilters);
  const [filters, setFilters] = useState<ConfigAuditFilters>(emptyFilters);
  const [offset, setOffset] = useState(0);
  const query = useQuery({
    queryKey: ["configuration-audit", identity, filters, offset],
    queryFn: ({ signal }) => getConfigAuditLog(filters, offset, signal),
    retry: false, gcTime: 0,
  });
  const apply = (event: FormEvent) => { event.preventDefault(); setOffset(0); setFilters({ ...draft }); };
  const data = query.data;
  const atBound = Boolean(data && offset + data.limit > data.max_offset);
  return <section className="space-y-4" aria-busy={query.isFetching}>
    <p className="text-sm leading-relaxed text-muted-foreground">{t("configAudit.notice")}</p>
    <p className="text-sm leading-relaxed text-muted-foreground">{t("configAudit.coverage")}</p>
    <form onSubmit={apply} className={`${panel} grid gap-3 sm:grid-cols-3`}>
      {fields.map(field => <div key={field} className="space-y-2">
        <Label htmlFor={`config-audit-${field}`}>{t(`configAudit.fields.${field}`)}</Label>
        <Input id={`config-audit-${field}`} maxLength={field === "action" ? 50 : field === "resource_type" ? 100 : 255} value={draft[field]} onChange={event => setDraft(previous => ({ ...previous, [field]: event.target.value }))} />
      </div>)}
      <div className="flex flex-wrap gap-2 sm:col-span-3">
        <Button type="submit" disabled={query.isFetching}>{t("tenantGovernance.applyFilters")}</Button>
        <Button type="button" variant="outline" onClick={() => { setDraft(emptyFilters); setFilters(emptyFilters); setOffset(0); }}>{t("management.clearFilters")}</Button>
        <Button type="button" variant="outline" disabled={query.isFetching} onClick={() => void query.refetch()}>{t("management.refresh")}</Button>
      </div>
    </form>
    {query.isError && <ManagementQueryState stale={Boolean(data)} retrying={query.isFetching} onRetry={() => void query.refetch()} />}
    {query.isPending ? <p role="status" className={panel}>{t("common.loading")}</p> : query.isError && !data ? null : data && <>
      <div className="flex flex-wrap items-center justify-between gap-3 text-sm">
        <span role="status">{t("configAudit.page", { page: Math.floor(offset / 25) + 1, count: data.items.length })}</span>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" disabled={!offset || query.isFetching} onClick={() => setOffset(value => Math.max(0, value - 25))}>{t("tenantGovernance.previous")}</Button>
          <Button variant="outline" size="sm" disabled={query.isFetching || query.isError || !data.has_more || atBound} onClick={() => setOffset(value => value + 25)}>{t("tenantGovernance.next")}</Button>
        </div>
      </div>
      {data.has_more && atBound && <p role="status" className="text-sm text-muted-foreground">{t("configAudit.bound")}</p>}
      {data.items.length ? <div className="overflow-hidden rounded-xl border border-border bg-card">
        <Table><TableHeader><TableRow>{columns.map(field => <TableHead key={field}>{t(`configAudit.fields.${field}`)}</TableHead>)}</TableRow></TableHeader>
          <TableBody>{data.items.map(entry => <TableRow key={entry.id}>{columns.map(field => <TableCell key={field} className="max-w-64 break-all text-xs">{entry[field] ?? "—"}</TableCell>)}</TableRow>)}</TableBody>
        </Table>
      </div> : <p className={panel}>{t("configAudit.empty")}</p>}
    </>}
  </section>;
}
