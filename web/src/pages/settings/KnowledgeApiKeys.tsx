import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { isAxiosError } from "axios";
import { Link } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { generateUUID } from "@/lib/utils";
import { issueKnowledgeKey, listKnowledgeKeys, revokeKnowledgeKey,
  type KnowledgeKeyInput, type KnowledgeKeyMetadata } from "@/api/knowledgeApiKeys";

type Review = { kind: "create"; input: KnowledgeKeyInput } | { kind: "revoke"; key: KnowledgeKeyMetadata };
function KeyEditor({ isCurrent }: { isCurrent: () => boolean }) {
  const { t } = useTranslation();
  const active = useRef(true);
  const writing = useRef(false);
  const reading = useRef(0);
  const [keys, setKeys] = useState<KnowledgeKeyMetadata[]>([]);
  const [page, setPage] = useState(0);
  const [loaded, setLoaded] = useState(false);
  const [readError, setReadError] = useState(false);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [name, setName] = useState("");
  const [writeScope, setWriteScope] = useState(false);
  const [days, setDays] = useState("7");
  const [review, setReview] = useState<Review | null>(null);
  const [secret, setSecret] = useState<{ id: string; value: string } | null>(null);
  const [uncertain, setUncertain] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const ownsView = () => active.current && isCurrent();
  const refresh = async (signal?: AbortSignal) => {
    const generation = ++reading.current;
    if (!ownsView()) return null;
    setLoading(true);
    try {
      const rows = await listKnowledgeKeys(signal);
      if (!ownsView() || signal?.aborted || reading.current !== generation) return null;
      setKeys(rows); setLoaded(true); setReadError(false);
      return rows;
    } catch (error) {
      if (ownsView() && !signal?.aborted && reading.current === generation) {
        setReadError(true);
        if (isAxiosError(error) && [401, 403].includes(error.response?.status ?? 0)) { setSecret(null); setReview(null); }
      }
      return null;
    } finally {
      if (ownsView() && !signal?.aborted && reading.current === generation) setLoading(false);
    }
  };
  useLayoutEffect(() => {
    active.current = true;
    const controller = new AbortController();
    void refresh(controller.signal);
    return () => { active.current = false; controller.abort(); };
    // The parent remounts this editor for every account/credential/authority change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const pages = Math.max(1, Math.ceil(keys.length / 20));
  const currentPage = Math.min(page, pages - 1);
  const ready = loaded && !readError && !loading && !busy;
  const confirm = async () => {
    if (writing.current || !ownsView() || !ready || !review) return;
    const submitted = review;
    if (submitted.kind === "create" && (uncertain || secret || Date.parse(submitted.input.expires_at) <= Date.now())) return;
    writing.current = true; setBusy(true); setNotice(null); setReview(null);
    try {
      if (submitted.kind === "create") {
        setSecret(null);
        try {
          const receipt = await issueKnowledgeKey(submitted.input);
          if (!ownsView()) return;
          setSecret({ id: receipt.metadata.key_id, value: receipt.secret });
          setName(""); setWriteScope(false); setDays("7"); setNotice("knowledgeKeys.issued");
        } catch {
          if (!ownsView()) return;
          setUncertain(true); setNotice("knowledgeKeys.issueUnknown");
        }
        await refresh();
      } else {
        let accepted = false;
        try { await revokeKnowledgeKey(submitted.key.key_id); accepted = true; } catch { /* Read only after any uncertain mutation. */ }
        if (!ownsView()) return;
        if (secret?.id === submitted.key.key_id) setSecret(null);
        const rows = await refresh();
        if (!ownsView()) return;
        const current = rows?.find(key => key.key_id === submitted.key.key_id);
        setNotice(current?.is_active === false ? "knowledgeKeys.revokedObserved"
          : accepted ? "knowledgeKeys.revokeReadback" : "knowledgeKeys.revokeUnknown");
      }
    } finally {
      writing.current = false;
      if (ownsView()) setBusy(false);
    }
  };
  const time = (value: string | null) => value ? new Date(value).toLocaleString() : t("knowledgeKeys.none");
  return <div className="space-y-5">
    <Link to="/settings" className="text-sm text-primary hover:underline">{t("knowledgeKeys.back")}</Link>
    <header className="space-y-2"><h1 className="text-2xl font-semibold">{t("knowledgeKeys.title")}</h1><p className="max-w-3xl text-sm text-muted-foreground">{t("knowledgeKeys.description")}</p></header>
    {notice && <p role="status" className="rounded-lg border p-3 text-sm">{t(notice)}</p>}
    {readError && <p role="alert" className="text-sm text-destructive">{t("knowledgeKeys.readError")}</p>}
    {secret && <Card><CardHeader><CardTitle>{t("knowledgeKeys.oneTime")}</CardTitle></CardHeader><CardContent className="space-y-3">
      <p className="text-sm">{t("knowledgeKeys.secretNotice")}</p><p className="break-all text-xs">{secret.id}</p>
      <code className="block break-all rounded bg-muted p-3 select-all">{secret.value}</code>
      <Button variant="outline" onClick={() => setSecret(null)}>{t("knowledgeKeys.dismissSecret")}</Button>
    </CardContent></Card>}
    <Card><CardHeader><CardTitle>{t("knowledgeKeys.createTitle")}</CardTitle></CardHeader><CardContent>
      <form className="space-y-3" onSubmit={event => {
        event.preventDefault();
        if (!ready || writing.current || !ownsView() || uncertain || secret || !name.trim() || !["1", "7", "30"].includes(days)) return;
        setReview({ kind: "create", input: { name: name.trim(), scopes: writeScope ? ["knowledge:read", "knowledge:write"] : ["knowledge:read"],
          expires_at: new Date(Date.now() + Number(days) * 86400000).toISOString() } });
      }}>
        <fieldset disabled={busy || Boolean(review) || uncertain || Boolean(secret)} className="space-y-3">
          <label className="grid gap-1 text-sm">{t("knowledgeKeys.name")}<Input required maxLength={100} value={name} onChange={event => setName(event.target.value)} /></label>
          <p className="text-sm">{t("knowledgeKeys.readScope")}</p>
          <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={writeScope} onChange={event => setWriteScope(event.target.checked)} />{t("knowledgeKeys.writeScope")}</label>
          <label className="grid gap-1 text-sm">{t("knowledgeKeys.expiry")}<select className="h-9 rounded border bg-background px-3" value={days} onChange={event => setDays(event.target.value)}>
            {["1", "7", "30"].map(value => <option key={value} value={value}>{t("knowledgeKeys.days", { count: Number(value) })}</option>)}
          </select></label>
          <Button type="submit" disabled={!ready || !name.trim()}>{t("knowledgeKeys.reviewCreate")}</Button>
        </fieldset>
      </form>
      {uncertain && <div className="mt-4 space-y-3 rounded border p-3"><p className="text-sm">{t("knowledgeKeys.uncertaintyNotice")}</p>
        <Button variant="outline" disabled={!ready} onClick={() => { if (!writing.current && ownsView()) { setUncertain(false); setName(""); setWriteScope(false); setDays("7"); setNotice("knowledgeKeys.newDecision"); } }}>{t("knowledgeKeys.allowNewDecision")}</Button>
      </div>}
    </CardContent></Card>
    {review && <section aria-label={t("knowledgeKeys.reviewTitle")} className="space-y-3 rounded-xl border border-primary/40 p-4">
      <h2 className="font-semibold">{t("knowledgeKeys.reviewTitle")}</h2>
      {review.kind === "create" ? <><p>{review.input.name}</p><p className="break-all text-sm">{review.input.scopes.join(", ")}</p><p className="text-sm">{t("knowledgeKeys.expiresAt")}: {time(review.input.expires_at)}</p><p className="text-sm">{t("knowledgeKeys.createImpact")}</p></>
        : <><p className="break-all">{review.key.name} · {review.key.key_id}</p><p className="text-sm">{t("knowledgeKeys.owner")}: {review.key.user_id ?? t("knowledgeKeys.none")}</p><p className="text-sm">{t("knowledgeKeys.scopes")}: {review.key.scopes.join(", ") || t("knowledgeKeys.none")}</p><p className="text-sm">{t("knowledgeKeys.roles")}: {review.key.roles.join(", ") || t("knowledgeKeys.none")}</p><p className="text-sm">{t("knowledgeKeys.tier")}: {review.key.tier ?? t("knowledgeKeys.none")}</p><p className="text-sm">{t("knowledgeKeys.revokeImpact")}</p></>}
      <div className="flex gap-2"><Button disabled={!ready} onClick={() => void confirm()}>{t(review.kind === "create" ? "knowledgeKeys.confirmCreate" : "knowledgeKeys.confirmRevoke")}</Button><Button variant="outline" disabled={busy} onClick={() => setReview(null)}>{t("common.cancel")}</Button></div>
    </section>}
    <section className="space-y-3" aria-label={t("knowledgeKeys.listTitle")}>
      <div className="flex items-center justify-between gap-2"><h2 className="font-semibold">{t("knowledgeKeys.listTitle")}</h2><Button variant="outline" disabled={busy || loading} onClick={() => void refresh()}>{t("knowledgeKeys.refresh")}</Button></div>
      <p className="text-sm text-muted-foreground">{t("knowledgeKeys.listScope")}</p>
      {loading && <p role="status">{t("common.loading")}</p>}
      {ready && !keys.length && <p className="text-sm">{t("knowledgeKeys.empty")}</p>}
      {loaded && !readError && !loading && keys.slice(currentPage * 20, (currentPage + 1) * 20).map(key => <article key={key.key_id} className="space-y-2 rounded-xl border p-4">
        <h3 className="break-all font-medium">{key.name}</h3><p className="break-all font-mono text-xs">{key.key_id} · {key.key_prefix ?? t("knowledgeKeys.none")}</p>
        <p className="text-sm">{t(key.is_active ? "knowledgeKeys.enabled" : "knowledgeKeys.revoked")}</p>
        <dl className="grid gap-2 text-sm sm:grid-cols-2">{[
          [t("knowledgeKeys.owner"), key.user_id ?? t("knowledgeKeys.none")],
          [t("knowledgeKeys.scopes"), key.scopes.join(", ") || t("knowledgeKeys.none")],
          [t("knowledgeKeys.roles"), key.roles.join(", ") || t("knowledgeKeys.none")],
          [t("knowledgeKeys.tier"), key.tier ?? t("knowledgeKeys.none")],
          [t("knowledgeKeys.createdAt"), time(key.created_at)], [t("knowledgeKeys.expiresAt"), time(key.expires_at)],
          [t("knowledgeKeys.lastUsed"), time(key.last_used_at)], [t("knowledgeKeys.useCount"), String(key.use_count)],
        ].map(([label, value]) => <div key={label}><dt className="text-muted-foreground">{label}</dt><dd className="break-all">{value}</dd></div>)}</dl>
        {key.is_active && <Button variant="outline" disabled={!ready || Boolean(review)} onClick={() => { if (!writing.current && ownsView()) setReview({ kind: "revoke", key }); }}>{t("knowledgeKeys.reviewRevoke")}</Button>}
      </article>)}
      {loaded && !readError && !loading && pages > 1 && <div className="flex items-center gap-3">
        <Button variant="outline" disabled={busy || currentPage === 0} onClick={() => setPage(currentPage - 1)}>{t("knowledgeKeys.previous")}</Button>
        <span className="text-sm">{t("knowledgeKeys.page", { page: currentPage + 1, pages, count: keys.length })}</span>
        <Button variant="outline" disabled={busy || currentPage + 1 >= pages} onClick={() => setPage(currentPage + 1)}>{t("knowledgeKeys.next")}</Button>
      </div>}
    </section>
  </div>;
}
export function KnowledgeApiKeysPage() {
  const token = useAuthStore(state => state.token);
  const authority = useAuthStore(state => sdkAuthorityScope(state.user));
  const scope = useMemo(() => ({ id: generateUUID(), token, authority }), [token, authority]);
  const isCurrent = () => {
    const state = useAuthStore.getState();
    return state.token === scope.token && sdkAuthorityScope(state.user) === scope.authority;
  };
  return <KeyEditor key={scope.id} isCurrent={isCurrent} />;
}
