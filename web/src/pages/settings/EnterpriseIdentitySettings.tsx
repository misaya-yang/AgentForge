import type { RequestBody } from "@/api/contracts";
import { PasskeyManagement } from "./PasskeyManagement";
import { useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";
import { api } from "@/lib/api";
import { getConsoleSession } from "@/api/auth";
import { useAuthStore } from "@/store/useAuthStore";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import { PasskeyStepUp } from "./PasskeyStepUp";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

const scimScopes = ["scim.users.read", "scim.users.write", "scim.groups.read", "scim.groups.write"] as const;
function IdentityEditor({ identity, isCurrent }: { identity: string; isCurrent: () => boolean }) {
  const { t, i18n } = useTranslation(), user = useAuthStore(state => state.user);
  const text = (en: string, zh: string) => i18n.language.startsWith("zh") ? zh : en;
  const mayProvision = user?.roles.includes("admin") || user?.permissions.includes("identity:provision");
  const [provider, setProvider] = useState(""), [subject, setSubject] = useState(""), [targetUser, setTargetUser] = useState("");
  const [days, setDays] = useState(30), [scopes, setScopes] = useState<string[]>(["scim.users.read"]);
  const [revokeId, setRevokeId] = useState(""), [secret, setSecret] = useState<{ id: string; token: string; expires: number }>();
  const [review, setReview] = useState(false), [busy, setBusy] = useState(false), [notice, setNotice] = useState("");
  const pending = useRef(false);
  const session = useQuery({ queryKey: ["identity-session", identity], retry: false, gcTime: 0,
    queryFn: async () => { const result = await getConsoleSession(); if (!isCurrent()) throw new Error("Session changed"); return { user: result.user, expires_at: result.expires_at }; } });
  const change = (action: () => void) => { setReview(false); action(); };
  const run = async (action: () => Promise<void>) => {
    if (pending.current || !isCurrent()) return; pending.current = true; setBusy(true); setNotice("");
    try { await action(); } catch { if (isCurrent()) setNotice(text("Operation unconfirmed or unavailable. No request was retried. Credential issuance may have committed; retain its exact ID if available and inspect before another explicit grant.", "操作未确认或不可用，未自动重试。凭据签发可能已提交；请保留已知准确 ID 并核查，再决定是否另行授权。")); }
    finally { pending.current = false; if (isCurrent()) setBusy(false); }
  };
  return <main className="space-y-5" aria-busy={busy}>
    <Link to="/settings">{text("Back to settings", "返回设置")}</Link>
    <h1 className="text-2xl font-semibold">{text("Enterprise identity", "企业身份")}</h1>
    <p>{text("Configured OIDC brokers, SCIM grants and current-session passkey assurance. SSO provider configuration and session inventory remain server-owned; unavailable features never fall back to a password.", "管理已配置的 OIDC、SCIM 授权及当前会话通行密钥认证。SSO 配置和会话清单由服务端管理；功能不可用时不会降级为密码。")}</p>
    <section className="space-y-2 rounded-xl border p-4" aria-label={text("Current session", "当前会话")}>
      <h2 className="font-semibold">{text("Current session", "当前会话")}</h2>
      {session.isPending && <p role="status">{t("common.loading")}</p>}
      {session.isError && <p role="alert">{text("Session metadata unavailable.", "会话元数据不可用。")}</p>}
      {session.data && <dl><dt>{text("User / tenant", "用户 / 租户")}</dt><dd>{session.data.user.user_id} / {session.data.user.tenant_id}</dd>
        <dt>{text("Absolute expiry", "绝对到期时间")}</dt><dd>{t("common.formattedDateTime", { value: new Date(session.data.expires_at * 1000) })}</dd></dl>}
      <Button variant="outline" disabled={session.isFetching} onClick={() => void session.refetch()}>{t("management.refresh")}</Button>
    </section>
    <PasskeyStepUp />
    <fieldset disabled={busy} className="space-y-3 rounded-xl border p-4"><legend>{text("Configured SSO provider", "已配置的 SSO 提供方")}</legend>
      <label className="block">{text("Provider ID", "提供方 ID")}<Input value={provider} maxLength={128} onChange={e => change(() => setProvider(e.target.value))} /></label>
      <Button disabled={!provider.trim()} onClick={() => void run(async () => {
        const { data } = await api.post<{ authorization_url: string }>(`/api/auth/enterprise/oidc/${encodeURIComponent(provider)}/begin`);
        if (!isCurrent()) return; const destination = new URL(data.authorization_url);
        if (destination.protocol !== "https:" || destination.username || destination.password) throw new Error("Invalid SSO redirect");
        window.location.assign(destination.href);
      })}>{text("Continue to this SSO provider", "继续前往此 SSO 提供方")}</Button>
      {mayProvision && <><label className="block">{text("Provider subject", "提供方 subject")}<Input value={subject} maxLength={255} onChange={e => change(() => setSubject(e.target.value))} /></label>
        <label className="block">{text("Existing user ID in this tenant", "当前租户现有用户 ID")}<Input value={targetUser} maxLength={255} onChange={e => change(() => setTargetUser(e.target.value))} /></label>
        <Button disabled={!review || !provider || !subject || !targetUser} onClick={() => void run(async () => {
          setReview(false); await api.post("/api/auth/enterprise/identities", ({ provider_id: provider, subject, user_id: targetUser } satisfies RequestBody<"POST /api/auth/enterprise/identities">));
          if (isCurrent()) setNotice(text("Server acknowledged the identity link.", "服务端已确认身份绑定。"));
        })}>{text("Link reviewed identity", "绑定已审阅身份")}</Button></>}
    </fieldset>
    {mayProvision && <fieldset disabled={busy} className="space-y-3 rounded-xl border p-4"><legend>{text("SCIM credential grants", "SCIM 凭据授权")}</legend>
      <p>{text("New credentials are tenant-scoped and shown once in this view only. A browser cookie cannot authenticate a SCIM request. No complete credential is available from a later metadata read.", "新凭据绑定当前租户，仅在此页面展示一次。浏览器 cookie 不能认证 SCIM 请求；后续元数据读取无法恢复完整凭据。")}</p>
      {scimScopes.map(scope => <label key={scope} className="flex gap-2"><input type="checkbox" checked={scopes.includes(scope)} onChange={e => change(() => setScopes(current => e.target.checked ? [...current, scope] : current.filter(value => value !== scope)))} />{scope}</label>)}
      <label className="block">{text("Expiry in days (1–90)", "有效天数（1–90）")}<Input type="number" min={1} max={90} value={days} onChange={e => change(() => setDays(Number(e.target.value)))} /></label>
      <Button disabled={!review || !scopes.length || !Number.isSafeInteger(days) || days < 1 || days > 90 || Boolean(secret)} onClick={() => void run(async () => {
        setReview(false); const { data } = await api.post<{ id: string; token: string; tenant_id: string; expires_at: number }>("/api/auth/enterprise/scim-credentials", ({ scopes, ttl_seconds: days * 86400 } satisfies RequestBody<"POST /api/auth/enterprise/scim-credentials">));
        if (!isCurrent()) return;
        if (data.tenant_id !== user?.tenant_id || typeof data.token !== "string" || !/^[a-f0-9]{64}$/.test(data.token) || !data.id || !Number.isSafeInteger(data.expires_at)) throw new Error("Invalid SCIM receipt");
        setSecret({ id: data.id, token: data.token, expires: data.expires_at });
      })}>{text("Issue reviewed credential", "签发已审阅凭据")}</Button>
      {secret && <section aria-label={text("One-time credential", "一次性凭据")} className="space-y-2 rounded border p-3">
        <p>{secret.id} · {t("common.formattedDateTime", { value: new Date(secret.expires * 1000) })}</p>
        <label className="block">{text("Save this credential securely", "请安全保存凭据")}<Input readOnly value={secret.token} autoComplete="off" /></label>
        <Button onClick={() => setSecret(undefined)}>{text("Saved; hide credential", "已保存，隐藏凭据")}</Button>
      </section>}
      <label className="block">{text("Exact credential ID to revoke", "要撤销的准确凭据 ID")}<Input value={revokeId} maxLength={64} onChange={e => change(() => setRevokeId(e.target.value))} /></label>
      <Button variant="destructive" disabled={!review || !/^[a-f0-9-]{36}$/i.test(revokeId)} onClick={() => void run(async () => {
        setReview(false); await api.delete(`/api/auth/enterprise/scim-credentials/${encodeURIComponent(revokeId)}`); if (isCurrent()) setNotice(text("Server acknowledged revocation of the exact credential.", "服务端已确认撤销此准确凭据。"));
      })}>{text("Revoke reviewed credential", "撤销已审阅凭据")}</Button>
    </fieldset>}
    {mayProvision && <label className="flex gap-2"><input type="checkbox" checked={review} disabled={busy} onChange={e => setReview(e.target.checked)} />{text("I reviewed the exact identity or credential, scopes and expiry. This is a new explicit authorization decision.", "我已审阅准确身份或凭据、权限范围及期限。这是新的明确授权决定。")}</label>}
    {notice && <p role="status">{notice}</p>}
  </main>;
}
export function EnterpriseIdentitySettings() {
  const scope = useAuthenticatedReadScope();
  return scope.isCurrent() ? <><IdentityEditor key={scope.id} identity={scope.id} isCurrent={scope.isCurrent} /><PasskeyManagement /></> : null;
}
