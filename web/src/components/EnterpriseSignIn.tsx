import type { RequestBody } from "@/api/contracts";
import { consoleLoginFence } from "@/lib/consoleLoginFence";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "@/lib/api";
import { getPasskeyAssertion, type AssertionChallenge } from "@/lib/passkey";
import { signInWithPasskey, type EnterpriseGrant } from "@/lib/passkeyCeremony";
import { consoleAuthGeneration, useAuthStore } from "@/store/useAuthStore";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

/** Login ceremonies create a session; sensitive in-session writes use step-up. */
export function EnterpriseSignIn({ onAuthenticated }: { onAuthenticated: (grant: EnterpriseGrant) => void }) {
  const { i18n } = useTranslation(), text = (en: string, zh: string) => i18n.language.startsWith("zh") ? zh : en;
  const loading = useAuthStore(state => state.isLoading), mounted = useRef(true), pending = useRef(false);
  const [tenant, setTenant] = useState(""), [name, setName] = useState(""), [provider, setProvider] = useState(""), [failed, setFailed] = useState(false);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const run = async (action: (current: () => boolean) => Promise<void>) => {
    const initial = useAuthStore.getState();
    if (pending.current || initial.isLoading) return;
    pending.current = true; initial.setLoading(true); setFailed(false);
    const fence = consoleLoginFence(useAuthStore.getState, consoleAuthGeneration, useAuthStore.subscribe, () => mounted.current);
    const current = fence.current;
    try { await action(current); }
    catch (error) { if (fence.ownsFailure((error as { response?: { status?: number } })?.response?.status)) setFailed(true); }
    finally { pending.current = false; if (current()) useAuthStore.getState().setLoading(false); fence.dispose(); }
  };
  return <details className="mt-5 space-y-3 rounded-lg border p-3 text-sm">
    <summary className="cursor-pointer font-medium">{text("Passkey or configured SSO", "通行密钥或已配置的 SSO")}</summary>
    <p>{text("Use your enrolled passkey or administrator-provided SSO provider ID. Availability and tenant policy come from the server.", "使用已注册的通行密钥或管理员提供的 SSO 提供方 ID。可用性与租户策略由服务端决定。")}</p>
    <fieldset disabled={loading} className="space-y-3">
      <label className="block">{text("Tenant ID", "租户 ID")}<Input value={tenant} maxLength={255} autoComplete="organization" onChange={e => setTenant(e.target.value)} /></label>
      <label className="block">{text("Account name or email", "账号名或邮箱")}<Input value={name} maxLength={255} autoComplete="username" onChange={e => setName(e.target.value)} /></label>
      <Button disabled={!tenant.trim() || !name.trim()} onClick={() => void run(async current => {
        const grant = await signInWithPasskey({
          start: async () => (await api.post<AssertionChallenge>("/api/auth/enterprise/passkeys/authenticate/start", ({ tenant_id: tenant.trim(), user_name: name.trim() } satisfies RequestBody<"POST /api/auth/enterprise/passkeys/authenticate/start">))).data,
          assertion: getPasskeyAssertion,
          finish: async (flow_id, credential) => (await api.post<unknown>("/api/auth/enterprise/passkeys/authenticate/finish", ({ flow_id, credential } satisfies RequestBody<"POST /api/auth/enterprise/passkeys/authenticate/finish">))).data,
        }, current, tenant.trim());
        if (current()) onAuthenticated(grant);
      })}>{text("Sign in with passkey", "使用通行密钥登录")}</Button>
      <label className="block">{text("Configured SSO provider ID", "已配置的 SSO 提供方 ID")}<Input value={provider} maxLength={128} onChange={e => setProvider(e.target.value)} /></label>
      <Button variant="outline" disabled={!provider.trim()} onClick={() => void run(async current => {
        const { data } = await api.post<{ authorization_url: string }>(`/api/auth/enterprise/oidc/${encodeURIComponent(provider.trim())}/begin`);
        if (!current()) return; const destination = new URL(data.authorization_url);
        if (destination.protocol !== "https:" || destination.username || destination.password) throw new Error("Invalid SSO destination");
        window.location.assign(destination.href);
      })}>{text("Continue with SSO", "继续使用 SSO")}</Button>
    </fieldset>
    {failed && <p role="alert">{text("Sign-in unavailable, cancelled or rejected. No password fallback or automatic replay was attempted.", "登录不可用、已取消或被拒绝。未降级为密码，也未自动重放。")}</p>}
  </details>;
}
