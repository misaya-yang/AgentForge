import type { RequestBody } from "@/api/contracts";
import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { api } from "@/lib/api";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { generateUUID } from "@/lib/utils";
import { useAuthStore } from "@/store/useAuthStore";

interface Config {
  strategy: string;
  runtime?: { strategy?: string };
  available_strategies: { value: string; label: string; description?: string }[];
}
interface Draft { strategy: string; baseline: string }
type Notice = "saved" | "saveUnknown" | "readBackFailed" | "readBackChanged" | null;
const path = "/api/v1/config/load-balancer";
const currentStrategy = (config?: Config) => config?.runtime?.strategy ?? config?.strategy;

function LoadBalancerEditor({ identity, isCurrent }: { identity: string; isCurrent: () => boolean }) {
  const { t } = useTranslation();
  const client = useQueryClient();
  const active = useRef(true);
  const submitting = useRef(false);
  const [busy, setBusy] = useState(false);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [notice, setNotice] = useState<Notice>(null);
  const [needsReview, setNeedsReview] = useState(false);
  useLayoutEffect(() => { active.current = true; return () => { active.current = false; }; }, []);
  const ownsView = () => active.current && isCurrent();
  const query = useQuery({
    queryKey: ["load-balancer", identity],
    queryFn: async ({ signal }) => {
      if (!isCurrent()) throw new Error("Settings scope changed");
      const { data } = await api.get<Config>(path, { signal });
      if (!isCurrent() || signal.aborted) throw new Error("Settings scope changed");
      if (typeof currentStrategy(data) !== "string" || !Array.isArray(data.available_strategies)) {
        throw new Error("Invalid load-balancer read-back");
      }
      return data;
    },
    retry: false, gcTime: 0,
  });
  const current = currentStrategy(query.data);
  const selected = draft?.strategy ?? current;
  const conflict = Boolean(draft && current !== draft.baseline);
  const strategies = query.data?.available_strategies ?? [];
  const valid = strategies.some(strategy => strategy.value === selected);
  const ready = Boolean(query.data && !query.isError && !query.isFetching && !busy);
  const canSave = ready && Boolean(draft) && valid && !conflict && !needsReview && selected !== current;

  const refresh = async () => {
    if (submitting.current || !ownsView()) return;
    await query.refetch();
  };
  const save = async () => {
    if (submitting.current || !ownsView() || !canSave || !draft) return;
    submitting.current = true;
    setBusy(true);
    setNotice(null);
    const submitted = draft;
    try {
      // Re-read before writing so an observed newer value is never overwritten
      // silently. The API has no revision/CAS token; this is not an atomic lock.
      const before = await query.refetch({ throwOnError: true });
      if (!ownsView()) return;
      if (currentStrategy(before.data) !== submitted.baseline) return;
      if (!before.data?.available_strategies.some(strategy => strategy.value === submitted.strategy)) return;
      let accepted = false;
      try {
        await api.put(path, ({ strategy: submitted.strategy } satisfies RequestBody<"PUT /api/v1/config/load-balancer">));
        accepted = true;
      } catch {
        // Both server profiles can mutate before an audit/transport failure.
        // A failed response is not proof that the old setting is still active.
      }
      if (!ownsView()) return;
      setNeedsReview(true);
      if (!accepted) setNotice("saveUnknown");
      try {
        const after = await query.refetch({ throwOnError: true });
        if (!ownsView()) return;
        if (accepted && currentStrategy(after.data) === submitted.strategy) {
          setDraft(null);
          setNeedsReview(false);
          setNotice("saved");
        } else if (accepted) {
          setNotice("readBackChanged");
        }
      } catch {
        if (ownsView() && accepted) setNotice("readBackFailed");
      }
      if (ownsView()) void client.invalidateQueries({ queryKey: ["system-status"] });
    } catch {
      // A failed preflight issues no PUT. The query error keeps saving blocked.
    } finally {
      submitting.current = false;
      if (ownsView()) setBusy(false);
    }
  };
  const useCurrent = () => {
    if (!ready || submitting.current || !ownsView()) return;
    setDraft(null); setNotice(null); setNeedsReview(false);
  };
  const reviewDraft = () => {
    if (!ready || submitting.current || !ownsView() || !draft || current === undefined) return;
    setDraft({ ...draft, baseline: current }); setNotice(null); setNeedsReview(false);
  };

  return <Card>
    <CardHeader>
      <CardTitle>{t("settings.loadBalancer.title")}</CardTitle>
      <CardDescription>{t("settings.loadBalancer.description")}</CardDescription>
    </CardHeader>
    <CardContent className="space-y-4" aria-busy={busy || query.isFetching}>
      <p className="text-sm text-muted-foreground">{t("settings.loadBalancer.runtimeNotice")}</p>
      {query.isPending && <p role="status">{t("common.loading")}</p>}
      {query.isError && <p role="alert" className="text-sm text-destructive">{t("settings.loadBalancer.loadFailed")}</p>}
      <dl className="space-y-2 text-sm">
        <div><dt className="text-muted-foreground">{t("settings.loadBalancer.current")}</dt><dd className="font-mono" data-testid="lb-current">{current ?? t("settings.loadBalancer.unknown")}</dd></div>
        {draft && <div><dt className="text-muted-foreground">{t("settings.loadBalancer.draft")}</dt><dd className="font-mono" data-testid="lb-draft">{draft.strategy}</dd></div>}
      </dl>
      {notice && <p role={notice === "saved" ? "status" : "alert"} className="text-sm">{t(`settings.loadBalancer.${notice}`)}</p>}
      {conflict && <p role="alert" className="text-sm text-destructive">{t("settings.loadBalancer.conflict")}</p>}
      <div role="radiogroup" aria-label={t("settings.loadBalancer.title")} className="grid gap-3 sm:grid-cols-2">
        {strategies.map(strategy => <label key={strategy.value} className={`flex cursor-pointer gap-3 rounded-lg border p-4 ${selected === strategy.value ? "border-primary bg-primary/5" : "border-border"}`}>
          <input type="radio" name="load-balancer-strategy" value={strategy.value} checked={selected === strategy.value} disabled={!ready}
            onChange={() => {
              if (submitting.current || !ownsView() || !ready || current === undefined) return;
              setDraft(previous => ({ strategy: strategy.value, baseline: previous?.baseline ?? current }));
              // Reconciliation warnings survive editing until explicitly reviewed.
              if (!needsReview) setNotice(null);
            }} />
          <span><span className="block font-medium">{strategy.label}</span><span className="text-sm text-muted-foreground">{strategy.description}</span></span>
        </label>)}
      </div>
      <div className="flex flex-wrap justify-end gap-2">
        <Button variant="outline" disabled={busy || query.isFetching} onClick={() => void refresh()}>{t("settings.loadBalancer.refresh")}</Button>
        {draft && <Button variant="outline" disabled={!ready} onClick={useCurrent}>{t("settings.loadBalancer.useCurrent")}</Button>}
        {draft && (conflict || needsReview) && selected !== current && <Button variant="outline" disabled={!ready || !valid} onClick={reviewDraft}>{t("settings.loadBalancer.reviewDraft")}</Button>}
        <Button disabled={!canSave} onClick={() => void save()}>{t(busy ? "settings.loadBalancer.saving" : "settings.loadBalancer.save")}</Button>
      </div>
    </CardContent>
  </Card>;
}

export function LoadBalancerSettings() {
  const token = useAuthStore(state => state.token);
  const authority = useAuthStore(state => sdkAuthorityScope(state.user));
  // An opaque key isolates reads/drafts without putting credentials in cache keys.
  const scope = useMemo(() => ({ identity: generateUUID(), token, authority }), [token, authority]);
  const isCurrent = () => {
    const state = useAuthStore.getState();
    return state.token === scope.token && sdkAuthorityScope(state.user) === scope.authority;
  };
  return <LoadBalancerEditor key={scope.identity} identity={scope.identity} isCurrent={isCurrent} />;
}
