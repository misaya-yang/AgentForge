import type { RequestBody } from "@/api/contracts";
import { useLayoutEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { api } from "@/lib/api";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import { PasskeyStepUp } from "./PasskeyStepUp";

interface Values {
  jwt_enabled: boolean;
  jwt_issuer: string;
  api_key_enabled: boolean;
  api_key_header: string;
}
interface Runtime extends Values {
  jwt_algorithms: string[];
  jwt_secret_configured: boolean;
  api_keys_count: number;
}
interface Config { runtime: Runtime; startup: Values & { jwt_algorithms: string[]; api_keys_count: number } }
interface Draft { values: Values; baseline: string }
type Notice = "saved" | "saveUnknown" | "readBackFailed" | "readBackChanged" | null;
const path = "/api/v1/config/auth";
const values = (value: Values): Values => ({ jwt_enabled: value.jwt_enabled, jwt_issuer: value.jwt_issuer,
  api_key_enabled: value.api_key_enabled, api_key_header: value.api_key_header });
const fingerprint = (value: unknown) => JSON.stringify(value);

// The API is write-only for credentials. Project only documented metadata into
// the query cache, including when an unexpected server returns extra fields.
function readConfig(data: unknown): Config {
  const object = (value: unknown): Record<string, unknown> => {
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid auth read-back");
    return value as Record<string, unknown>;
  };
  const boolean = (value: unknown): boolean => {
    if (typeof value !== "boolean") throw new Error("Invalid auth read-back");
    return value;
  };
  const string = (value: unknown): string => {
    if (typeof value !== "string") throw new Error("Invalid auth read-back");
    return value;
  };
  const count = (value: unknown): number => {
    if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) throw new Error("Invalid auth read-back");
    return value;
  };
  const algorithms = (value: unknown): string[] => {
    if (!Array.isArray(value) || !value.every(item => typeof item === "string")) throw new Error("Invalid auth read-back");
    return [...value];
  };
  const config = object(data), runtime = object(config.runtime), jwt = object(config.jwt), key = object(config.api_key);
  return {
    runtime: {
      jwt_enabled: boolean(runtime.jwt_enabled), jwt_issuer: runtime.jwt_issuer === null ? "" : string(runtime.jwt_issuer),
      api_key_enabled: boolean(runtime.api_key_enabled), api_key_header: string(runtime.api_key_header),
      jwt_algorithms: algorithms(runtime.jwt_algorithms), jwt_secret_configured: boolean(runtime.jwt_secret_configured),
      api_keys_count: count(runtime.api_keys_count),
    },
    startup: {
      jwt_enabled: boolean(jwt.enabled), jwt_issuer: jwt.issuer === null ? "" : string(jwt.issuer),
      api_key_enabled: boolean(key.enabled), api_key_header: string(key.header_name),
      jwt_algorithms: algorithms(jwt.algorithms), api_keys_count: count(key.key_count),
    },
  };
}

function AuthEditor({ identity, isCurrent }: { identity: string; isCurrent: () => boolean }) {
  const { t } = useTranslation();
  const active = useRef(true);
  const pending = useRef(false);
  const revision = useRef(0);
  const renderedRevision = revision.current;
  const [busy, setBusy] = useState(false);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [secret, setSecret] = useState("");
  const [notice, setNotice] = useState<Notice>(null);
  const [needsReview, setNeedsReview] = useState(false);
  useLayoutEffect(() => { active.current = true; return () => { active.current = false; }; }, []);
  const ownsView = () => active.current && isCurrent();
  const ownsAction = () => ownsView() && renderedRevision === revision.current;
  const query = useQuery({
    queryKey: ["auth-config", identity], retry: false, gcTime: 0,
    queryFn: async ({ signal }) => {
      if (!isCurrent()) throw new Error("Settings scope changed");
      const { data } = await api.get<unknown>(path, { signal });
      if (!isCurrent() || signal.aborted) throw new Error("Settings scope changed");
      return readConfig(data);
    },
  });
  const current = query.data?.runtime;
  const form = draft?.values ?? (current ? values(current) : null);
  const conflict = Boolean(draft && current && draft.baseline !== fingerprint(current));
  const ready = Boolean(current && !query.isError && !query.isFetching && !busy && ownsView());
  const invalid = form && (!/^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/.test(form.api_key_header)
    || (form.jwt_enabled && !current?.jwt_secret_configured && !secret));
  const changed = Boolean(form && current && (fingerprint(form) !== fingerprint(values(current)) || secret));
  const canSave = ready && Boolean(draft) && changed && !invalid && !conflict && !needsReview;

  const edit = (change: Partial<Values>, nextSecret?: string) => {
    if (!ready || pending.current || !ownsAction() || !current || !form) return;
    revision.current++;
    setDraft({ values: { ...form, ...change }, baseline: draft?.baseline ?? fingerprint(current) });
    if (nextSecret !== undefined) setSecret(nextSecret);
    if (!needsReview) setNotice(null);
  };
  const refresh = async () => {
    if (pending.current || !ownsView()) return;
    await query.refetch();
  };
  const save = async () => {
    if (pending.current || !ownsAction() || !canSave || !draft) return;
    revision.current++;
    pending.current = true; setBusy(true); setNotice(null);
    const submitted = draft;
    try {
      const before = await query.refetch({ throwOnError: true });
      if (!ownsView() || !before.data || fingerprint(before.data.runtime) !== submitted.baseline) return;
      const body = { ...submitted.values, jwt_issuer: submitted.values.jwt_issuer || null,
        jwt_algorithms: [...before.data.runtime.jwt_algorithms], ...(secret ? { jwt_secret: secret } : {}) };
      const replacesSecret = Boolean(secret);
      // Keep credentials out of mutation caches and reconciliation state. A
      // retry of an uncertain replacement requires fresh, explicit input.
      setSecret(""); setNeedsReview(true);
      let accepted = false;
      try {
        const { data } = await api.put<{ status?: string }>(path, (body satisfies RequestBody<"PUT /api/v1/config/auth">));
        accepted = data?.status === "success";
      } catch {
        // Both profiles mutate before cache/audit. Even a failed response can
        // follow a completed write; only a read is automatic, never a PUT.
      }
      if (!ownsView()) return;
      if (!accepted) setNotice("saveUnknown");
      try {
        const after = await query.refetch({ throwOnError: true });
        if (!ownsView() || !after.data) return;
        const observed = after.data.runtime;
        if (accepted && fingerprint(values(observed)) === fingerprint(submitted.values)
          && fingerprint(observed.jwt_algorithms) === fingerprint(body.jwt_algorithms)
          && (!replacesSecret || observed.jwt_secret_configured)) {
          setDraft(null); setNeedsReview(false); setNotice("saved");
        } else if (accepted) setNotice("readBackChanged");
      } catch {
        if (ownsView() && accepted) setNotice("readBackFailed");
      }
    } catch {
      // Preflight failures keep every field, including an unsubmitted secret.
    } finally {
      pending.current = false;
      if (ownsView()) setBusy(false);
    }
  };
  const useCurrent = () => {
    if (!ready || pending.current || !ownsAction()) return;
    revision.current++;
    setDraft(null); setSecret(""); setNotice(null); setNeedsReview(false);
  };
  const reviewDraft = () => {
    if (!ready || pending.current || !ownsAction() || !draft || !current) return;
    revision.current++;
    setDraft({ ...draft, baseline: fingerprint(current) }); setNotice(null); setNeedsReview(false);
  };
  const summary = (value: Config["startup"], id: string) => <dl data-testid={id} className="space-y-1 text-sm">
    <div><dt>{t("settings.auth.jwt.enable")}</dt><dd>{t(value.jwt_enabled ? "common.active" : "common.disabled")}</dd></div>
    <div><dt>{t("settings.auth.jwt.issuer")}</dt><dd className="break-all">{value.jwt_issuer || t("settings.auth.recovery.notSet")}</dd></div>
    <div><dt>{t("settings.auth.recovery.algorithms")}</dt><dd>{value.jwt_algorithms.join(", ") || t("settings.auth.recovery.notSet")}</dd></div>
    <div><dt>{t("settings.auth.apiKey.enable")}</dt><dd>{t(value.api_key_enabled ? "common.active" : "common.disabled")}</dd></div>
    <div><dt>{t("settings.auth.apiKey.headerName")}</dt><dd className="break-all">{value.api_key_header}</dd></div>
    <div><dt>{t("settings.auth.recovery.keyCount")}</dt><dd>{value.api_keys_count}</dd></div>
  </dl>;

  return <div className="space-y-4" aria-busy={busy || query.isFetching}>
    <PasskeyStepUp key={identity} />
    <p className="text-sm text-muted-foreground">{t("settings.auth.recovery.scope")}</p>
    {query.isPending && <p role="status">{t("common.loading")}</p>}
    {query.isError && <p role="alert">{t("settings.auth.recovery.loadFailed")}</p>}
    {query.data && <div className="grid gap-4 md:grid-cols-2">
      <Card><CardHeader><CardTitle>{t("settings.auth.recovery.current")}</CardTitle></CardHeader><CardContent>
        {summary(query.data.runtime, "auth-current")}
        <p>{t(query.data.runtime.jwt_secret_configured ? "settings.auth.recovery.secretConfigured" : "settings.auth.recovery.secretMissing")}</p>
      </CardContent></Card>
      <Card><CardHeader><CardTitle>{t("settings.auth.recovery.startup")}</CardTitle></CardHeader><CardContent>{summary(query.data.startup, "auth-startup")}</CardContent></Card>
    </div>}
    {draft && <p role="status">{t("settings.auth.recovery.draft")}</p>}
    {notice && <p role={notice === "saved" ? "status" : "alert"}>{t(`settings.auth.recovery.${notice}`)}</p>}
    {conflict && <p role="alert">{t("settings.auth.recovery.conflict")}</p>}
    {invalid && <p role="alert">{t("settings.auth.recovery.invalid")}</p>}
    <Card><CardHeader><CardTitle>{t("settings.auth.jwt.title")}</CardTitle><CardDescription>{t("settings.auth.jwt.description")}</CardDescription></CardHeader>
      <CardContent className="space-y-4">
        <div className="flex items-center justify-between"><Label htmlFor="settings-jwt-enabled">{t("settings.auth.jwt.enable")}</Label>
          <Switch id="settings-jwt-enabled" checked={form?.jwt_enabled ?? false} disabled={!ready} onCheckedChange={checked => edit({ jwt_enabled: checked })} />
        </div>
        {form?.jwt_enabled && <>
          <div className="space-y-2"><Label htmlFor="settings-jwt-secret">{t("settings.auth.jwt.secret")}</Label>
            <Input id="settings-jwt-secret" type="password" autoComplete="new-password" value={secret} disabled={!ready} onChange={event => edit({}, event.target.value)} />
            <p className="text-xs text-muted-foreground">{t("settings.auth.recovery.secretNotice")}</p>
          </div>
          <div className="space-y-2"><Label htmlFor="settings-jwt-issuer">{t("settings.auth.jwt.issuer")}</Label>
            <Input id="settings-jwt-issuer" placeholder={t("settings.auth.jwt.issuerPlaceholder")} value={form.jwt_issuer} disabled={!ready} onChange={event => edit({ jwt_issuer: event.target.value })} />
          </div>
        </>}
      </CardContent>
    </Card>
    <Card><CardHeader><CardTitle>{t("settings.auth.apiKey.title")}</CardTitle><CardDescription>{t("settings.auth.apiKey.description")}</CardDescription></CardHeader>
      <CardContent className="space-y-4">
        <div className="flex items-center justify-between"><Label htmlFor="settings-api-key-enabled">{t("settings.auth.apiKey.enable")}</Label>
          <Switch id="settings-api-key-enabled" checked={form?.api_key_enabled ?? false} disabled={!ready} onCheckedChange={checked => edit({ api_key_enabled: checked })} />
        </div>
        {form?.api_key_enabled && <div className="space-y-2"><Label htmlFor="settings-api-key-header">{t("settings.auth.apiKey.headerName")}</Label>
          <Input id="settings-api-key-header" placeholder={t("settings.auth.apiKey.headerPlaceholder")} value={form.api_key_header} disabled={!ready} onChange={event => edit({ api_key_header: event.target.value })} />
        </div>}
      </CardContent>
    </Card>
    <div className="flex flex-wrap justify-end gap-2">
      <Button variant="outline" disabled={busy || query.isFetching} onClick={() => void refresh()}>{t("settings.auth.recovery.refresh")}</Button>
      {draft && <Button variant="outline" disabled={!ready} onClick={useCurrent}>{t("settings.auth.recovery.useCurrent")}</Button>}
      {draft && (conflict || needsReview) && <Button variant="outline" disabled={!ready} onClick={reviewDraft}>{t("settings.auth.recovery.reviewDraft")}</Button>}
      <Button variant="primary" disabled={!canSave} onClick={() => void save()}>{t(busy ? "settings.auth.saving" : "settings.auth.save")}</Button>
    </div>
  </div>;
}

export function AuthSettings() {
  const scope = useAuthenticatedReadScope();
  if (!scope.isCurrent()) return null;
  return <AuthEditor key={scope.id} identity={scope.id} isCurrent={scope.isCurrent} />;
}
