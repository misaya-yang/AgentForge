import type { RequestBody } from "@/api/contracts";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "@/lib/api";
import { createPasskey, type RegistrationChallenge } from "@/lib/passkey";
import { registerConsolePasskey } from "@/lib/passkeyCeremony";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import { useAuthStore } from "@/store/useAuthStore";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

/** Remains mounted across its own successful session rotation so the one-time
 * recovery receipt survives, while any unrelated account generation hides it. */
export function PasskeyManagement() {
  const scope = useAuthenticatedReadScope(), token = useAuthStore(state => state.token), user = useAuthStore(state => state.user);
  const { i18n } = useTranslation(), text = (en: string, zh: string) => i18n.language.startsWith("zh") ? zh : en;
  const [busy, setBusy] = useState(false), [review, setReview] = useState(false), [id, setId] = useState(""), [notice, setNotice] = useState("");
  const [formToken, setFormToken] = useState(token);
  const draftCurrent = formToken === token;
  useEffect(() => { setFormToken(token); setId(""); setReview(false); setNotice(""); }, [token]);
  const [receipt, setReceipt] = useState<{ generation: string; codes: string[] }>(); const pending = useRef(false), mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  useEffect(() => { if (receipt && receipt.generation !== token) setReceipt(undefined); }, [token, receipt]);
  const visibleReceipt = receipt?.generation === token ? receipt : undefined;
  const enroll = async () => {
    if (pending.current || !draftCurrent || !review || visibleReceipt || !scope.isCurrent() || !user?.tenant_id) return;
    pending.current = true; setBusy(true); setReview(false); setNotice("");
    try {
      const { grant, codes } = await registerConsolePasskey({
        start: async () => (await api.post<RegistrationChallenge>("/api/auth/enterprise/passkeys/register/start", ({} satisfies RequestBody<"POST /api/auth/enterprise/passkeys/register/start">))).data,
        create: createPasskey,
        finish: async (flow_id, credential) => (await api.post<unknown>("/api/auth/enterprise/passkeys/register/finish", ({ flow_id, credential } satisfies RequestBody<"POST /api/auth/enterprise/passkeys/register/finish">))).data,
      }, scope.isCurrent, user.tenant_id, user.user_id);
      if (!scope.isCurrent()) return;
      const state = useAuthStore.getState(); state.setAuth(grant.csrf_token, grant.user, grant.force_password_change, state.rememberMe);
      const generation = useAuthStore.getState().token!;
      setReceipt({ generation, codes });
      setNotice(text("Registration confirmed and the session rotated. Save all ten new recovery codes now; older codes and sessions were retired by the server.", "注册已确认，会话已轮换。请立即保存十个新的恢复码；旧恢复码与会话已由服务端撤销。"));
    } catch { if (scope.isCurrent()) setNotice(text("Registration unavailable, cancelled or unconfirmed. A recent MFA session is required; no password fallback or POST retry was used. Inspect account security before starting a new ceremony.", "注册不可用、已取消或结果未确认。必须使用近期 MFA 会话；未降级为密码，也未重试 POST。再次注册前请核查账号安全状态。")); }
    finally { pending.current = false; if (mounted.current) setBusy(false); }
  };
  const revoke = async () => {
    if (pending.current || !draftCurrent || !review || !scope.isCurrent() || !/^[A-Za-z0-9_-]{1,2048}$/.test(id)) return;
    pending.current = true; setBusy(true); setReview(false); setNotice("");
    try { await api.delete(`/api/auth/enterprise/passkeys/${encodeURIComponent(id)}`); if (scope.isCurrent()) useAuthStore.getState().clearAuth(); }
    catch { if (scope.isCurrent()) setNotice(text("Revocation unconfirmed or rejected. The server protects the last credential; no delete was retried.", "撤销未确认或被拒绝。服务端保护最后一个凭据；未重试删除。")); }
    finally { pending.current = false; if (mounted.current) setBusy(false); }
  };
  if (!user || !token) return null;
  return <section className="space-y-3 rounded-xl border p-4" aria-busy={busy}>
    <h2 className="font-semibold">{text("Passkey enrollment and revocation", "通行密钥注册与撤销")}</h2>
    <p>{text("Enrollment requires recent OIDC MFA or verification with an existing passkey. Password-only initial enrollment stays blocked by the server policy. Registering rotates the session and recovery codes; revoking requires signing in again. This API does not expose a credential inventory.", "注册需要近期 OIDC MFA 或现有通行密钥验证。仅密码的首次注册仍受服务端策略限制。注册会轮换会话与恢复码；撤销后需重新登录。此 API 尚未提供凭据清单。")}</p>
    <fieldset disabled={!draftCurrent || busy || Boolean(visibleReceipt)} className="space-y-3">
      <label className="flex gap-2"><input type="checkbox" checked={draftCurrent && review} onChange={e => setReview(e.target.checked)} />{text("I reviewed the security impact and will save the new recovery material.", "我已审阅安全影响，并将保存新的恢复凭据。")}</label>
      <Button disabled={!review} onClick={() => void enroll()}>{text("Register a new passkey", "注册新的通行密钥")}</Button>
      <label className="block">{text("Exact credential ID to revoke", "要撤销的准确凭据 ID")}<Input value={draftCurrent ? id : ""} maxLength={2048} onChange={e => { setId(e.target.value); setReview(false); }} /></label>
      <Button variant="destructive" disabled={!review || !/^[A-Za-z0-9_-]{1,2048}$/.test(id)} onClick={() => void revoke()}>{text("Revoke and sign out", "撤销并退出")}</Button>
    </fieldset>
    {visibleReceipt && <section aria-label={text("One-time recovery codes", "一次性恢复码")} className="space-y-2 rounded border p-3">
      <p>{text("Registration confirmed and the session rotated. Save these ten new recovery codes now; old recovery material was retired by the server.", "注册已确认，会话已轮换。请立即保存十个新的恢复码；旧恢复凭据已由服务端撤销。")}</p>
      <ol className="break-all font-mono text-sm">{visibleReceipt.codes.map(code => <li key={code}>{code}</li>)}</ol>
      <Button onClick={() => setReceipt(undefined)}>{text("Saved securely; hide codes", "已安全保存，隐藏恢复码")}</Button>
    </section>}
    {notice && <p role="status">{notice}</p>}
  </section>;
}
