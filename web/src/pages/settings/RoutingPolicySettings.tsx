import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";
import { activateRoutingPolicy, publishRoutingPolicy, readRoutingDecision, readRoutingPolicy } from "@/api/routingPolicy";
import { useAuthStore } from "@/store/useAuthStore";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import { initialRoutingPolicy, parseRoutingPolicy, routingPolicyFingerprint } from "./routingPolicyDraft";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";

function Editor({ tenant, isCurrent }: { tenant: string; isCurrent: () => boolean }) {
  const { i18n } = useTranslation(); const text = (en: string, zh: string) => i18n.language.startsWith("zh") ? zh : en;
  const [draft, setDraft] = useState(() => JSON.stringify(initialRoutingPolicy(tenant), null, 2));
  const [id, setId] = useState(""), [version, setVersion] = useState(""), [expectedPin, setExpectedPin] = useState("");
  const [firstActivation, setFirstActivation] = useState(false), [review, setReview] = useState(false);
  const [decisionId, setDecisionId] = useState(""), [decision, setDecision] = useState<unknown>();
  const [busy, setBusy] = useState(false), [notice, setNotice] = useState(""); const pending = useRef(false);
  const run = async (action: () => Promise<void>) => {
    if (pending.current || !isCurrent()) return; pending.current = true; setBusy(true); setNotice("");
    try { await action(); } catch { if (isCurrent()) setNotice(text("Operation unconfirmed or unavailable. Read the exact immutable version/decision before making another explicit write; no request was retried.", "操作未确认或不可用。请先读取准确的不可变版本或决策，再决定是否重新写入；未自动重试。")); }
    finally { pending.current = false; if (isCurrent()) setBusy(false); }
  };
  const load = () => run(async () => { const policy = await readRoutingPolicy(id, version); if (!isCurrent()) return;
    const parsed = parseRoutingPolicy(JSON.stringify(policy), tenant); setDraft(JSON.stringify(parsed, null, 2)); setReview(false); setNotice(text("Immutable version loaded. This does not prove it is active.", "已读取不可变版本；这不代表它当前已激活。")); });
  const publish = () => run(async () => {
    const policy = parseRoutingPolicy(draft, tenant); if (!review || !isCurrent()) return;
    setReview(false); const pin = await publishRoutingPolicy(policy); if (!isCurrent()) return;
    const observed = await readRoutingPolicy(policy.policy_id, policy.version); if (!isCurrent()) return;
    if (routingPolicyFingerprint(parseRoutingPolicy(JSON.stringify(observed), tenant)) !== routingPolicyFingerprint(policy)) throw new Error("routing_readback_changed");
    setId(policy.policy_id); setVersion(policy.version); setNotice(text(`Published immutable version; server pin ${pin}. Activation is a separate action.`, `不可变版本已发布；服务端指纹 ${pin}。激活需要单独操作。`));
  });
  const activate = () => run(async () => { if (!review || (!firstActivation && !/^[a-f0-9]{64}$/.test(expectedPin))) return;
    setReview(false); const pin = await activateRoutingPolicy(id, version, firstActivation ? null : expectedPin); if (!isCurrent()) return;
    setNotice(text(`Server acknowledged activation ${pin}. A lost response must be investigated, never replayed automatically.`, `服务端已确认激活 ${pin}。若响应丢失，应人工核查，不会自动重放。`)); });
  return <main className="space-y-5" aria-busy={busy}>
    <Link to="/settings">{text("Back to settings", "返回设置")}</Link>
    <h1 className="text-2xl font-semibold">{text("Routing policies and decisions", "路由策略与决策")}</h1>
    <p>{text("Publish immutable tenant policies, activate with compare-and-swap, and inspect server decisions. The console never invents catalog availability, permissions, prices or evaluation outcomes.", "发布租户不可变策略，通过指纹比较后激活，并查看服务端决策。目录可用性、权限、价格与决策结果均由服务端提供。")}</p>
    <fieldset disabled={busy} className="space-y-3 rounded-xl border p-4"><legend>{text("Immutable policy", "不可变策略")}</legend>
      <label className="block">{text("Policy ID", "策略 ID")}<Input value={id} maxLength={128} onChange={e => { setId(e.target.value); setReview(false); }} /></label>
      <label className="block">{text("Version", "版本")}<Input value={version} maxLength={128} onChange={e => { setVersion(e.target.value); setReview(false); }} /></label>
      <Button disabled={!id || !version} onClick={() => void load()}>{text("Read this version", "读取此版本")}</Button>
      <label className="block">{text("Policy JSON (current tenant only)", "策略 JSON（仅当前租户）")}<Textarea rows={18} className="font-mono" value={draft} onChange={e => { setDraft(e.target.value); setReview(false); }} /></label>
      <label className="flex gap-2"><input type="checkbox" checked={review} onChange={e => setReview(e.target.checked)} />{text("I reviewed this exact draft and the separate activation impact.", "我已审阅此准确草稿及单独激活的影响。")}</label>
      <Button disabled={!review} onClick={() => void publish()}>{text("Publish reviewed version", "发布已审阅版本")}</Button>
      <label className="block">{text("Expected current active pin", "预期的当前激活指纹")}<Input value={expectedPin} onChange={e => { setExpectedPin(e.target.value); setReview(false); }} disabled={firstActivation} /></label>
      <label className="flex gap-2"><input type="checkbox" checked={firstActivation} onChange={e => { setFirstActivation(e.target.checked); setReview(false); }} />{text("Create the first active binding only; conflict if one already exists.", "仅建立首次激活绑定；已有绑定时必须冲突。")}</label>
      <Button disabled={!id || !version || !review || (!firstActivation && !/^[a-f0-9]{64}$/.test(expectedPin))} onClick={() => void activate()}>{text("Activate reviewed version", "激活已审阅版本")}</Button>
    </fieldset>
    <fieldset disabled={busy} className="space-y-3 rounded-xl border p-4"><legend>{text("Explain a server decision", "解释服务端决策")}</legend>
      <label className="block">{text("Decision ID from a request receipt", "请求凭据中的决策 ID")}<Input value={decisionId} maxLength={255} onChange={e => { setDecisionId(e.target.value); setDecision(undefined); }} /></label>
      <Button disabled={!decisionId.trim()} onClick={() => void run(async () => { const data = await readRoutingDecision(decisionId); if (isCurrent()) setDecision(data); })}>{text("Inspect exact decision", "查看准确决策")}</Button>
      {decision !== undefined && <pre className="max-h-96 overflow-auto whitespace-pre-wrap break-all rounded bg-muted p-3" aria-label={text("Server decision evidence", "服务端决策证据")}>{JSON.stringify(decision, null, 2)}</pre>}
    </fieldset>
    {notice && <p role="status">{notice}</p>}
  </main>;
}
export function RoutingPolicySettings() {
  const scope = useAuthenticatedReadScope(), tenant = useAuthStore(state => state.user?.tenant_id);
  if (!scope.isCurrent() || !tenant) return <p role="alert">Current tenant session is required.</p>;
  return <Editor key={scope.id} tenant={tenant} isCurrent={scope.isCurrent} />;
}
