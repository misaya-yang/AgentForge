import { useEffect, useMemo, useReducer, useState } from "react";
import { Link } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { previewProviderModels, applyProviderModelReview, readProviderModelReview } from "@/api/providers";
import { useAuthStore } from "@/store/useAuthStore";
import { CatalogReviewSession } from "./catalogReviewState";

export function ProviderCatalogReview({ provider, onClose, onReadback }: {provider: string; onClose: () => void; onReadback: () => void}) {
  const { t } = useTranslation();
  const [, render] = useReducer(n => n + 1, 0);
  const token = useAuthStore(state => state.token);
  const user = useAuthStore(state => state.user);
  const session = useMemo(() => { void token; void user; return new CatalogReviewSession(provider, {preview: previewProviderModels, apply: applyProviderModelReview, read: readProviderModelReview}, render); }, [provider, token, user]);
  const [selected, setSelected] = useState<string[]>([]);
  const [replace, setReplace] = useState(false);
  useEffect(() => { session.mount(); return () => session.dispose(); }, [session]);
  const preview = session.preview;
  const includesExisting = preview?.changes.some(c => selected.includes(c.model_id) && c.existing);
  return <Dialog open onOpenChange={open => { if (!open && !session.busy) onClose(); }}>
    <DialogContent className="max-h-[85vh] max-w-3xl overflow-y-auto" showCloseButton={!session.busy}>
      <DialogHeader><DialogTitle>{t("services.catalog.title")} · {provider}</DialogTitle>
        <DialogDescription>{t("services.catalog.explanation")}</DialogDescription></DialogHeader>
      {!preview && <Button disabled={session.busy} onClick={() => { setSelected([]); setReplace(false); void session.load(); }}>{t("services.catalog.preview")}</Button>}
      {session.busy && <p role="status">{t("services.catalog.loading")}</p>}
      {session.error && <p role="alert" className="text-destructive">{t(`services.catalog.${session.error}`)}</p>}
      {preview && <>
        <p className="text-sm">{t("services.catalog.policy")}</p>
        {preview.discovery_warnings.map((warning, i) => <p key={i} className="text-sm text-amber-700">{warning}</p>)}
        <div className="space-y-3">{preview.changes.map(change => <section className="rounded border p-3" key={change.model_id}>
          <label className="flex items-center gap-2"><input type="checkbox" checked={selected.includes(change.model_id)} disabled={session.busy || session.consumed || change.kind === "unchanged"} onChange={e => setSelected(ids => e.target.checked ? [...ids, change.model_id] : ids.filter(id => id !== change.model_id))} /><span className="font-medium">{change.model_id}</span><span>{t(`services.catalog.${change.kind}`)}</span></label>
          <ul className="mt-2 space-y-1 text-xs">{change.changes.map(field => <li key={field.field} className="break-all">{field.field}: {JSON.stringify(field.before)} → {JSON.stringify(field.after)}</li>)}</ul>
          {change.existing && <details><summary>{t("services.catalog.preserved")}</summary><pre className="overflow-x-auto text-xs">{JSON.stringify(Object.fromEntries(change.preserved_fields.map(field => [field, change.existing?.[field]])), null, 2)}</pre></details>}
          {change.new_model_policy && <p className="mt-2 text-xs break-all">{t("services.catalog.newPolicy")}: {JSON.stringify(change.new_model_policy)}</p>}
        </section>)}</div>
        <details><summary>{t("services.catalog.retained")} ({preview.retained_models.length})</summary><ul>{preview.retained_models.map(m => <li key={String(m.model_id)}>{String(m.model_id)}</li>)}</ul></details>
        <details><summary>{t("services.catalog.skipped")} ({preview.skipped_models.length})</summary><ul>{preview.skipped_models.map((m, i) => <li key={i}>{m.model_id}: {m.reason}</li>)}</ul></details>
        <section><h3 className="font-medium">{t("services.catalog.references")}</h3><p className="text-xs">{t("services.catalog.coverage")}</p><ul>{preview.affected_references.map((r, i) => <li key={i}><Link className="underline" to={`/agents/${encodeURIComponent(r.agent_id)}`}>{r.name}</Link> · {r.model_id} · {r.kind} · {r.match}</li>)}</ul></section>
        <label className="flex gap-2 text-sm"><input type="checkbox" checked={replace} disabled={session.busy || session.consumed} onChange={e => setReplace(e.target.checked)} />{t("services.catalog.replace")}</label>
        <div className="flex gap-2"><Button disabled={session.busy || session.consumed || !selected.length || Boolean(includesExisting && !replace)} onClick={() => void session.apply(selected, replace)}>{t("services.catalog.apply")}</Button>
          <Button variant="outline" disabled={session.busy} onClick={() => void session.read().then(onReadback)}>{t("services.catalog.readback")}</Button></div>
        {session.consumed && <p className="text-sm">{t("services.catalog.noReplay")}</p>}
      </>}
      {session.result && <section role="status"><p>{t("services.catalog.status")}: {session.result.state}</p>{session.result.result?.message && <p>{session.result.result.message}</p>}<details><summary>{t("services.catalog.current")}</summary><pre className="overflow-x-auto text-xs">{JSON.stringify(session.result.current?.models ?? session.result.result?.models, null, 2)}</pre></details></section>}
      <Button variant="ghost" disabled={session.busy} onClick={onClose}>{t("services.catalog.close")}</Button>
    </DialogContent>
  </Dialog>;
}
