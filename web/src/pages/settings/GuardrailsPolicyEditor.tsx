import { useRef, useState, type FormEvent } from "react";
import { useMutation } from "@tanstack/react-query";
import { putGuardrailPolicy, type GuardrailPolicy, type GuardrailStage, type PolicySnapshot } from "@/api/guardrails";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { detectors, emptyPolicy, validPolicy, type GuardrailsText } from "./guardrailsForm";

export function GuardrailsPolicyEditor({ snapshot, onSaved, onReload, text, errorText }: {
  snapshot: PolicySnapshot; onSaved: (value: PolicySnapshot) => void; onReload: () => void;
  text: GuardrailsText; errorText: (error: unknown) => string;
}) {
  const [draft, setDraft] = useState<GuardrailPolicy>(() => structuredClone(snapshot.policy ?? emptyPolicy()));
  const [review, setReview] = useState(false);
  const submitting = useRef(false);
  const mutation = useMutation({ mutationFn: putGuardrailPolicy, retry: false,
    onSuccess: onSaved, onSettled: () => { submitting.current = false; } });
  const locked = mutation.isPending || mutation.isError || mutation.isSuccess;
  const changeStage = (index: number, next: GuardrailStage) => setDraft(p => ({ ...p, stages: p.stages.map((s, i) => i === index ? next : s) }));
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!validPolicy(draft) || locked || submitting.current) return;
    if (!review) { setReview(true); return; }
    submitting.current = true;
    mutation.mutate({ policy: structuredClone(draft), expected_pin: snapshot.policy_pin });
  };
  return <form onSubmit={submit} className="space-y-5" aria-label={text("护栏策略编辑", "Guardrail policy editor")}>
    <p className="text-sm text-muted-foreground">{text("策略仅作用于当前租户。停用后请求沿原有流程执行；观察模式记录结果，执行模式拦截命中的请求。", "Policies apply to the current tenant. Disabling preserves the existing request flow. Observe records decisions; enforce blocks matching requests.")}</p>
    {review && <p role="status">{text("请确认以下策略，然后确认保存。", "Review the policy below, then confirm save.")}</p>}
    <fieldset disabled={locked || review} className="space-y-5">
      <label className="flex items-center gap-2"><input type="checkbox" checked={draft.enabled} onChange={e => setDraft(p => ({ ...p, enabled: e.target.checked }))} />{text("启用护栏", "Enable guardrails")}</label>
      <div className="grid gap-4 sm:grid-cols-2">{([
        ["version", text("策略版本", "Policy version"), undefined, undefined],
        ["budget_ms", text("检测时间预算（毫秒）", "Detection budget (ms)"), 5, 500],
        ["max_input_bytes", text("最大输入（字节）", "Maximum input (bytes)"), 1024, 2097152],
        ["max_output_bytes", text("最大输出（字节）", "Maximum output (bytes)"), 1024, 4194304],
      ] as const).map(([field, title, min, max]) => <div key={field}><Label htmlFor={`guardrail-${field}`}>{title}</Label><Input id={`guardrail-${field}`} required type={field === "version" ? "text" : "number"} min={min} max={max} step={1} value={draft[field]} onChange={e => setDraft(p => ({ ...p, [field]: field === "version" ? e.target.value : Number(e.target.value) }))} /></div>)}</div>
      {draft.stages.map((stage, index) => <StageEditor key={stage.stage} stage={stage} text={text} onChange={next => changeStage(index, next)} />)}
    </fieldset>
    {!validPolicy(draft) && <p role="status" className="text-sm text-muted-foreground">{text("每阶段至少配置一条有效规则或敏感数据检测器，并选择输入或输出方向。规则表达式由服务器最终校验。", "Configure at least one valid rule or sensitive-data detector per stage and select input or output. The server validates rule expressions.")}</p>}
    {mutation.isError && <p role="alert" className="text-sm text-destructive">{errorText(mutation.error)}</p>}
    <div className="flex flex-wrap gap-3">
      <Button type="submit" disabled={locked || !validPolicy(draft)}>{text(mutation.isPending ? "正在保存…" : review ? "确认保存" : "检查更改", mutation.isPending ? "Saving…" : review ? "Confirm save" : "Review changes")}</Button>
      {review && !locked && <Button type="button" variant="outline" onClick={() => setReview(false)}>{text("返回编辑", "Back to editing")}</Button>}
      <Button type="button" variant="outline" disabled={mutation.isPending} onClick={onReload}>{text("放弃草案并重新读取", "Discard draft and reload")}</Button>
    </div>
  </form>;
}
function StageEditor({ stage, onChange, text }: { stage: GuardrailStage; onChange: (stage: GuardrailStage) => void; text: GuardrailsText }) {
  const names = { injection: text("提示注入", "Prompt injection"), sensitive_data: text("敏感数据", "Sensitive data"), content_safety: text("内容安全", "Content safety") };
  return <fieldset className="space-y-3 rounded-xl border border-border p-4"><legend className="px-2 font-medium">{names[stage.stage]}</legend>
    <label className="flex items-center gap-2">{text("处理方式", "Mode")}<select className="rounded border bg-background p-2" value={stage.mode} onChange={e => onChange({ ...stage, mode: e.target.value as GuardrailStage["mode"] })}><option value="observe">{text("观察", "Observe")}</option><option value="enforce">{text("执行拦截", "Enforce")}</option></select></label>
    <div className="flex gap-4">{(["input", "output"] as const).map(direction => <label key={direction} className="flex items-center gap-2"><input type="checkbox" checked={stage.directions.includes(direction)} onChange={e => onChange({ ...stage, directions: e.target.checked ? [...stage.directions, direction] : stage.directions.filter(d => d !== direction) })} />{direction === "input" ? text("输入", "Input") : text("输出", "Output")}</label>)}</div>
    <label className="block text-sm">{text("触发分数", "Trigger score")}<Input type="number" min={1} max={65535} required value={stage.threshold} onChange={e => onChange({ ...stage, threshold: Number(e.target.value) })} /></label>
    {stage.stage === "sensitive_data" && <div className="grid gap-2 sm:grid-cols-2">{detectors.map(detector => <label key={detector} className="flex items-center gap-2 text-sm"><input type="checkbox" checked={stage.detectors.includes(detector)} onChange={e => onChange({ ...stage, detectors: e.target.checked ? [...stage.detectors, detector] : stage.detectors.filter(d => d !== detector) })} />{{ email: text("电子邮件", "Email"), us_ssn: text("美国社会安全号码", "US SSN"), payment_card: text("支付卡", "Payment card"), private_key: text("私钥", "Private key"), cloud_access_key: text("云访问密钥", "Cloud access key"), credential_assignment: text("凭据赋值", "Credential assignment") }[detector]}</label>)}</div>}
    {stage.rules.map((rule, index) => {
      const update = (next: typeof rule) => onChange({ ...stage, rules: stage.rules.map((r, i) => i === index ? next : r) });
      return <fieldset key={index} className="space-y-3 rounded border p-3"><legend>{text("规则", "Rule")} {index + 1}</legend>
        <label className="block text-sm">{text("规则名称", "Rule name")}<Input required value={rule.id} maxLength={96} onChange={e => update({ ...rule, id: e.target.value })} /></label>
        {(["all", "any"] as const).map(field => <label key={field} className="block text-sm">{field === "all" ? text("必须全部匹配的表达式（每行一个）", "All matching expressions (one per line)") : text("任意匹配的表达式（每行一个）", "Any matching expressions (one per line)")}<textarea className="mt-1 w-full rounded border bg-background p-2 font-mono text-sm" rows={2} value={rule[field].join("\n")} onChange={e => update({ ...rule, [field]: e.target.value ? e.target.value.split("\n") : [] })} /></label>)}
        <div className="grid gap-3 sm:grid-cols-2"><label>{text("任意匹配的最少数量", "Minimum any matches")}<Input type="number" min={rule.any.length ? 1 : 0} max={rule.any.length} required value={rule.min_any} onChange={e => update({ ...rule, min_any: Number(e.target.value) })} /></label><label>{text("规则分数", "Rule score")}<Input type="number" min={1} max={65535} required value={rule.score} onChange={e => update({ ...rule, score: Number(e.target.value) })} /></label></div>
        <Button type="button" variant="outline" onClick={() => onChange({ ...stage, rules: stage.rules.filter((_, i) => i !== index) })}>{text("移除规则", "Remove rule")}</Button>
      </fieldset>;
    })}
    <Button type="button" variant="outline" disabled={stage.rules.length >= 32} onClick={() => onChange({ ...stage, rules: [...stage.rules, { id: "", all: [], any: [], min_any: 0, score: 1 }] })}>{text("添加规则", "Add rule")}</Button>
  </fieldset>;
}
