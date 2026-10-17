import { useState, type FormEvent } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";
import { getGuardrailDecisions, getGuardrailPolicy, type PolicySnapshot } from "@/api/guardrails";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { GuardrailsPolicyEditor } from "./GuardrailsPolicyEditor";
import type { GuardrailsText } from "./guardrailsForm";

export function GuardrailsSettingsPage() {
  const user = useAuthStore(s => s.user);
  const session = useAuthStore(s => s.token); // Memory-only session generation, never a persisted bearer.
  const identity = JSON.stringify([sdkAuthorityScope(user), session]);
  const manage = Boolean(user?.roles.includes("admin") || user?.permissions.includes("guardrails:manage"));
  return <GuardrailsContent key={identity} identity={identity} manage={manage} />;
}
function GuardrailsContent({ identity, manage }: { identity: string; manage: boolean }) {
  const { i18n } = useTranslation();
  const text: GuardrailsText = (zh, en) => (i18n.resolvedLanguage ?? i18n.language).startsWith("zh") ? zh : en;
  const client = useQueryClient();
  const [revision, setRevision] = useState(0);
  const [saved, setSaved] = useState(false);
  const [requestDraft, setRequestDraft] = useState("");
  const [requestId, setRequestId] = useState("");
  const key = ["guardrails-policy", identity, revision];
  const policy = useQuery({ queryKey: key, queryFn: ({ signal }) => getGuardrailPolicy(signal), enabled: manage, retry: false, gcTime: 0, staleTime: 0, refetchOnWindowFocus: false, refetchOnReconnect: false });
  const decisions = useQuery({ queryKey: ["guardrails-decisions", identity, requestId], queryFn: ({ signal }) => getGuardrailDecisions(requestId, signal), enabled: Boolean(requestId), retry: false, gcTime: 0, refetchOnWindowFocus: false, refetchOnReconnect: false });
  const errorText = (error: unknown) => {
    const status = (error as { response?: { status?: number } })?.response?.status;
    if (status === 403 || status === 401) return text("当前身份无权执行此操作，请确认登录和租户权限。", "Your current identity cannot perform this action. Check your session and tenant permissions.");
    if (status === 409) return text("策略已被其他人修改。请重新读取并核对后再编辑，当前草案尚未保存。", "Someone changed this policy. Reload and review before editing again; this draft was not saved.");
    if (status === 404) return text("未找到记录，或当前身份无权查看。", "No record was found, or your identity cannot view it.");
    if (status === 503) return text("服务无法确认操作。策略可能未通过校验，或保存状态未知；请重新读取确认，勿重复提交。", "The service could not confirm this operation. Policy validation may have failed or the save outcome is unknown. Reload to verify before submitting again.");
    return text("操作未获确认。请重新读取状态；未确认保存成功。", "The operation was not confirmed. Reload the current state; a successful save has not been established.");
  };
  const reload = () => { setSaved(false); setRevision(v => v + 1); };
  const onSaved = (snapshot: PolicySnapshot) => { client.setQueryData(key, snapshot); setSaved(true); };
  const lookup = (event: FormEvent) => { event.preventDefault(); const id = requestDraft.trim(); if (id === requestId) void decisions.refetch(); else setRequestId(id); };
  return <main className="mx-auto max-w-5xl space-y-6 p-4 sm:p-6">
    <Link to="/settings" className="text-sm text-primary hover:underline">{text("返回设置", "Back to settings")}</Link>
    <header><h1 className="text-2xl font-bold">{text("护栏策略与决策日志", "Guardrail policy and decisions")}</h1><p className="mt-2 text-sm text-muted-foreground">{text("管理当前租户的提示注入、敏感数据及内容安全检测。", "Manage prompt injection, sensitive-data and content-safety checks for the current tenant.")}</p></header>
    {manage ? <section className="space-y-4 rounded-xl border border-border bg-card p-4 sm:p-5" aria-busy={policy.isFetching}>
      <h2 className="text-lg font-medium">{text("租户策略", "Tenant policy")}</h2>
      {policy.isPending && <p role="status">{text("正在读取策略…", "Loading policy…")}</p>}
      {policy.isError && <><p role="alert" className="text-destructive">{errorText(policy.error)}</p><Button variant="outline" onClick={reload}>{text("重新读取", "Reload")}</Button></>}
      {saved && <p role="status">{text("策略已保存并重新读取确认。", "Policy saved and confirmed by readback.")}</p>}
      {policy.data && !policy.isError && <><p className="text-sm text-muted-foreground">{policy.data.policy ? text(`当前策略：${policy.data.policy.version} · ${policy.data.policy.enabled ? "启用" : "停用"}`, `Current policy: ${policy.data.policy.version} · ${policy.data.policy.enabled ? "Enabled" : "Disabled"}`) : text("尚未配置策略。新草案默认停用，完整配置后可保存。", "No policy configured. New drafts start disabled and require complete configuration before saving.")}</p><GuardrailsPolicyEditor key={`${revision}:${policy.data.policy_pin ?? "none"}`} snapshot={policy.data} text={text} errorText={errorText} onReload={reload} onSaved={onSaved} /></>}
    </section> : <p className="rounded-xl border p-4 text-sm">{text("策略管理需要租户管理员或护栏管理权限。你仍可查询本人请求的决策日志。", "Policy management requires tenant admin or guardrail management permission. You can still look up decisions for your own requests.")}</p>}
    <section className="space-y-4 rounded-xl border border-border bg-card p-4 sm:p-5" aria-busy={decisions.isFetching}>
      <h2 className="text-lg font-medium">{text("决策日志", "Decision log")}</h2>
      <p className="text-sm text-muted-foreground">{text("使用请求编号查询本租户可见的审计记录。日志仅含规则、位置及时间，不显示提示词或命中的秘密值。", "Look up tenant-authorized audit records by request ID. Records show rules, structural locations and timing, without prompts or matched secret values.")}</p>
      <form onSubmit={lookup} className="flex flex-wrap items-end gap-3"><div className="min-w-0 flex-1"><Label htmlFor="guardrail-request">{text("请求编号", "Request ID")}</Label><Input id="guardrail-request" value={requestDraft} maxLength={255} required onChange={e => { setRequestDraft(e.target.value); setRequestId(""); }} /></div><Button disabled={decisions.isFetching || !requestDraft.trim()}>{text("查询决策", "Find decisions")}</Button></form>
      {decisions.isFetching && <p role="status">{text("正在查询…", "Loading decisions…")}</p>}
      {decisions.isError && <p role="alert" className="text-destructive">{errorText(decisions.error)}</p>}
      {decisions.data && !decisions.isError && !decisions.isFetching && <div className="space-y-4">{decisions.data.decisions.map(row => <article key={row.audit_id} className="space-y-3 rounded-lg border p-4">
        <h3 className="font-medium">{row.decision.direction} · {row.decision.decision}</h3><p className="break-words text-sm">{row.created_at} · {text("策略", "Policy")} {row.decision.policy_version} · {row.decision.reason}</p>
        <dl className="grid gap-2 text-sm sm:grid-cols-3"><div><dt>{text("检测耗时", "Detection latency")}</dt><dd>{row.decision.elapsed_us / 1000} ms</dd></div><div><dt>{text("审计耗时", "Audit latency")}</dt><dd>{row.decision.audit_us === null ? "—" : `${row.decision.audit_us / 1000} ms`}</dd></div><div><dt>{text("输出等待", "Output holdback")}</dt><dd>{row.decision.holdback_us / 1000} ms</dd></div></dl>
        <p className="text-sm">{text("审计状态", "Audit state")}: {row.decision.audit_state}</p>
        <ul className="space-y-2 text-sm">{row.decision.stages.map(stage => <li key={stage.stage}>{stage.stage} · {stage.mode} · {text("分数", "Score")} {stage.score} · {stage.elapsed_us / 1000} ms<ul className="ml-4 break-words">{stage.findings.map((finding, i) => <li key={i}>{finding.rule} · {finding.location}</li>)}</ul></li>)}</ul>
      </article>)}</div>}
    </section>
  </main>;
}
