import { useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Bell, Clock3, FilePenLine, Play, RefreshCw, WandSparkles, Webhook } from "lucide-react";
import { Dialog, DialogContent, DialogTitle, DialogDescription } from "@/components/ui/dialog";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { listAutomations, draftAutomation, createAutomation, confirmAutomation, changeAutomation, automationExecutions, automationNotifications, type AutomationRule, type AutomationDefinition, type AutomationAction, type AutomationTrigger } from "@/api/automations";
import "./automations.css";

const icons = { cron: Clock3, webhook: Webhook, space_page_changed: FilePenLine, connector_event: RefreshCw, run_agent: Play, run_skill: WandSparkles, write_space_page: FilePenLine, notify: Bell };
function initialTrigger(kind: AutomationTrigger["kind"]): AutomationTrigger {
  if (kind === "cron") return { kind, expression: "0 9 * * 1-5", timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC" };
  if (kind === "space_page_changed") return { kind, item_id: "" };
  if (kind === "connector_event") return { kind, entry_id: "" };
  return { kind };
}
function initialAction(kind: AutomationAction["kind"]): AutomationAction {
  if (kind === "run_agent") return { kind, prompt: "" };
  if (kind === "run_skill") return { kind, skill_id: "", prompt: "" };
  if (kind === "write_space_page") return { kind, item_id: "", markdown: "" };
  return { kind, message: "" };
}
function destination(action: AutomationAction) { return "item_id" in action ? action.item_id : "skill_id" in action ? action.skill_id : null; }
function actionContent(action: AutomationAction) { return "message" in action ? action.message : "markdown" in action ? action.markdown : action.prompt; }
function AutomationWorkspace() {
  const { t } = useTranslation();
  const user = useAuthStore(state => state.user);
  const token = useAuthStore(state => state.token);
  const authority = `${sdkAuthorityScope(user)}:${token}`;
  const queryClient = useQueryClient();
  const listing = useQuery({ queryKey: ["automations", authority], queryFn: ({ signal }) => listAutomations(signal) });
  const notifications = useQuery({ queryKey: ["automation-notifications", authority], queryFn: ({ signal }) => automationNotifications(signal), refetchInterval: 15000 });
  const [prompt, setPrompt] = useState("");
  const [definition, setDefinition] = useState<AutomationDefinition>(() => ({ title: "", model_id: "", trigger: initialTrigger("cron"), actions: [initialAction("notify")] }));
  const [pending, setPending] = useState(false);
  const inFlight = useRef(false);
  const [notice, setNotice] = useState<{ authority: string; text: string } | null>(null);
  const [review, setReview] = useState<{ authority: string; rule: AutomationRule } | null>(null);
  const [secret, setSecret] = useState<{ authority: string; value: string; id: string } | null>(null);
  const [history, setHistory] = useState<AutomationRule | null>(null);
  const executions = useQuery({ queryKey: ["automation-executions", authority, history?.automation_id], queryFn: ({ signal }) => automationExecutions(history!.automation_id, signal), enabled: Boolean(history) });
  const current = () => `${sdkAuthorityScope(useAuthStore.getState().user)}:${useAuthStore.getState().token}` === authority;
  async function perform(operation: () => Promise<void>) {
    if (inFlight.current || !current()) return;
    inFlight.current = true; setPending(true); setNotice(null);
    try { await operation(); if (current()) await queryClient.invalidateQueries({ queryKey: ["automations", authority] }); }
    catch { if (current()) setNotice({ authority, text: t("automation.failed") }); }
    finally { inFlight.current = false; if (current()) setPending(false); }
  }
  function updateAction(index: number, patch: Record<string, string>) { setDefinition(previous => ({ ...previous, actions: previous.actions.map((action, at) => at === index ? { ...action, ...patch } as AutomationAction : action) })); }
  return <section className="forge-page">
    <header className="forge-page-header"><div><h1>{t("automation.title")}</h1><p>{t("automation.summary")}</p></div></header>
    <div className="automation-create">
      <form onSubmit={event => { event.preventDefault(); void perform(async () => { const rule = await draftAutomation(prompt); if (current()) setReview({ authority, rule }); }); }}>
        <h2><WandSparkles size={20} aria-hidden="true" />{t("automation.describe")}</h2>
        <label htmlFor="automation-prompt">{t("automation.prompt")}</label>
        <textarea id="automation-prompt" className="forge-input" required maxLength={8000} value={prompt} onChange={event => setPrompt(event.target.value)} placeholder={t("automation.example")} />
        <p className="automation-hint">{t("automation.confirmHint")}</p>
        <button type="submit" className="forge-button forge-button-primary" disabled={pending}>{pending ? t("automation.working") : t("automation.draft")}</button>
      </form>
      <details><summary>{t("automation.manual")}</summary><form className="automation-manual" onSubmit={event => { event.preventDefault(); void perform(async () => { const rule = await createAutomation(definition); if (current()) setReview({ authority, rule }); }); }}>
        <div className="automation-fields"><label>{t("automation.name")}<input className="forge-input" required maxLength={120} value={definition.title} onChange={event => setDefinition(old => ({ ...old, title: event.target.value }))} /></label>
        <label>{t("automation.model")}<input className="forge-input" required maxLength={255} value={definition.model_id} onChange={event => setDefinition(old => ({ ...old, model_id: event.target.value }))} /></label></div>
        <label>{t("automation.trigger")}<select className="forge-input" value={definition.trigger.kind} onChange={event => setDefinition(old => ({ ...old, trigger: initialTrigger(event.target.value as AutomationTrigger["kind"]) }))}>{(["cron", "webhook", "space_page_changed", "connector_event"] as const).map(kind => <option key={kind} value={kind}>{t(`automation.kind.${kind}`)}</option>)}</select></label>
        {definition.trigger.kind === "connector_event" && <p className="automation-hint">{t("automation.connectorHint")}</p>}
        {definition.trigger.kind === "cron" && <div className="automation-fields">{(["expression", "timezone"] as const).map(field => <label key={field}>{t(`automation.${field}`)}<input className="forge-input" required value={definition.trigger.kind === "cron" ? definition.trigger[field] : ""} onChange={event => setDefinition(old => ({ ...old, trigger: { ...old.trigger, [field]: event.target.value } as AutomationTrigger }))} /></label>)}</div>}
        {definition.trigger.kind === "space_page_changed" && <label>{t("automation.item")}<input className="forge-input" required value={definition.trigger.item_id} onChange={event => setDefinition(old => ({ ...old, trigger: { kind: "space_page_changed", item_id: event.target.value } }))} /></label>}
        {definition.trigger.kind === "connector_event" && <label>{t("automation.entry")}<input className="forge-input" required value={definition.trigger.entry_id} onChange={event => setDefinition(old => ({ ...old, trigger: { kind: "connector_event", entry_id: event.target.value } }))} /></label>}
        {definition.actions.map((action, index) => <fieldset key={index}><legend>{t("automation.actionNumber", { number: index + 1 })}</legend><label>{t("automation.action")}<select className="forge-input" value={action.kind} onChange={event => setDefinition(old => ({ ...old, actions: old.actions.map((value, at) => at === index ? initialAction(event.target.value as AutomationAction["kind"]) : value) }))}>{(["run_agent", "run_skill", "write_space_page", "notify"] as const).map(kind => <option key={kind} value={kind}>{t(`automation.kind.${kind}`)}</option>)}</select></label>
          {action.kind === "run_skill" && <label>{t("automation.skill")}<input className="forge-input" required value={action.skill_id} onChange={event => updateAction(index, { skill_id: event.target.value })} /></label>}
          {action.kind === "write_space_page" && <label>{t("automation.item")}<input className="forge-input" required value={action.item_id} onChange={event => updateAction(index, { item_id: event.target.value })} /></label>}
          <label>{t("automation.content")}<textarea className="forge-input" required maxLength={action.kind === "notify" ? 4000 : 20000} value={actionContent(action)} onChange={event => updateAction(index, { [action.kind === "notify" ? "message" : action.kind === "write_space_page" ? "markdown" : "prompt"]: event.target.value })} /></label>
          {definition.actions.length > 1 && <button className="forge-button" type="button" onClick={() => setDefinition(old => ({ ...old, actions: old.actions.filter((_value, at) => at !== index) }))}>{t("automation.removeAction")}</button>}
        </fieldset>)}
        <div className="automation-actions"><button type="button" className="forge-button" disabled={definition.actions.length >= 8} onClick={() => setDefinition(old => ({ ...old, actions: [...old.actions, initialAction("notify")] }))}>{t("automation.addAction")}</button><button type="submit" className="forge-button forge-button-primary" disabled={pending}>{t("automation.draft")}</button></div>
      </form></details>
    </div>
    {notice?.authority === authority && !review && <p role="alert" className="forge-error">{notice.text}</p>}
    {secret?.authority === authority && <aside className="automation-secret"><h2>{t("automation.webhookSecret")}</h2><p>{t("automation.secretHint")}</p><code>{secret.value}</code><p><code>{`${location.origin}/api/v1/automations/${secret.id}/webhook`}</code></p><button type="button" className="forge-button" onClick={() => setSecret(null)}>{t("common.close")}</button></aside>}
    {listing.isPending ? <div className="forge-state" role="progressbar" aria-label={t("automation.loading")} /> : listing.isError ? <div className="forge-state forge-error" role="alert"><p>{t("automation.loadFailed")}</p><button type="button" className="forge-button" onClick={() => void listing.refetch()}>{t("common.retry")}</button></div> : !listing.data?.length ? <div className="forge-state"><Clock3 size={32} aria-hidden="true" /><p>{t("automation.empty")}</p></div> : <ul className="automation-grid" role="list">{listing.data.map(rule => {
      const Icon = icons[rule.definition.trigger.kind];
      return <li key={rule.automation_id}><article><div className="automation-card-header"><Icon size={20} aria-hidden="true" /><span className="automation-status">{t(`automation.status.${rule.status}`)}</span></div><h2>{rule.title}</h2><p>{t(`automation.kind.${rule.definition.trigger.kind}`)}</p><div className="automation-actions">
        {rule.status === "draft" && <button type="button" className="forge-button forge-button-primary" onClick={() => setReview({ authority, rule })}>{t("automation.review")}</button>}
        {(["active", "paused"] as const).includes(rule.status as "active" | "paused") && <><button type="button" className="forge-button" disabled={pending} onClick={() => void perform(async () => { await changeAutomation(rule, rule.status === "active" ? "pause" : "resume"); })}>{rule.status === "active" ? t("automation.pause") : t("automation.resume")}</button><button type="button" className="forge-button" disabled={pending} onClick={() => void perform(async () => { await changeAutomation(rule, "cancel"); })}>{t("automation.cancel")}</button></>}
        <button type="button" className="forge-button" onClick={() => setHistory(rule)}>{t("automation.history")}</button>
      </div></article></li>;
    })}</ul>}
    <section className="automation-notifications" aria-label={t("automation.notifications")}><h2><Bell size={20} aria-hidden="true" />{t("automation.notifications")}</h2>{notifications.isPending ? <p role="status">{t("automation.loading")}</p> : notifications.isError ? <p role="alert">{t("automation.loadFailed")}</p> : !notifications.data?.length ? <p className="automation-hint">{t("automation.noNotifications")}</p> : <ul>{notifications.data.map(notification => <li key={notification.notification_id}><p>{notification.message}</p><time dateTime={notification.created_at}>{new Date(notification.created_at).toLocaleString()}</time></li>)}</ul>}</section>
    <Dialog open={review?.authority === authority} onOpenChange={open => { if (!open && !pending) setReview(null); }}><DialogContent><DialogTitle>{t("automation.review")}</DialogTitle><DialogDescription>{t("automation.confirmHint")}</DialogDescription>{notice?.authority === authority && <p role="alert" className="forge-error">{notice.text}</p>}{review?.authority === authority && <><h3>{review.rule.title}</h3><p>{t("automation.model")}: {review.rule.definition.model_id}</p><p>{t(`automation.kind.${review.rule.definition.trigger.kind}`)}</p>{review.rule.definition.trigger.kind === "cron" && <p>{review.rule.definition.trigger.expression} · {review.rule.definition.trigger.timezone}</p>}{("item_id" in review.rule.definition.trigger || "entry_id" in review.rule.definition.trigger) && <p>{"item_id" in review.rule.definition.trigger ? review.rule.definition.trigger.item_id : "entry_id" in review.rule.definition.trigger ? review.rule.definition.trigger.entry_id : ""}</p>}<ol className="automation-review-actions">{review.rule.definition.actions.map((action, index) => <li key={index}><strong>{t(`automation.kind.${action.kind}`)}</strong>{destination(action) && <p><code>{destination(action)}</code></p>}<p>{actionContent(action)}</p></li>)}</ol><div className="automation-actions"><button className="forge-button" type="button" disabled={pending} onClick={() => setReview(null)}>{t("common.cancel")}</button><button className="forge-button forge-button-primary" type="button" disabled={pending} onClick={() => void perform(async () => { const result = await confirmAutomation(review.rule); if (current()) { setReview(null); if (result.webhook_secret) setSecret({ authority, value: result.webhook_secret, id: result.automation.automation_id }); } })}>{t("automation.activate")}</button></div></>}</DialogContent></Dialog>
    <Dialog open={Boolean(history)} onOpenChange={open => { if (!open) setHistory(null); }}><DialogContent><DialogTitle>{t("automation.history")}</DialogTitle><DialogDescription>{t("automation.historyHint")}</DialogDescription>{executions.isPending ? <p role="status">{t("automation.loading")}</p> : executions.isError ? <p className="forge-error" role="alert">{t("automation.loadFailed")}</p> : <ul className="automation-history">{executions.data?.map(execution => <li key={execution.delivery_id}><span>{t("automation.actionNumber", { number: execution.action_index + 1 })}</span><span>{t(`automation.status.${execution.status}`, { defaultValue: execution.status })}</span><time dateTime={execution.created_at}>{new Date(execution.created_at).toLocaleString()}</time></li>)}</ul>}</DialogContent></Dialog>
  </section>;
}

export function AutomationsPage() {
  const scope = useAuthStore(state => `${sdkAuthorityScope(state.user)}:${state.token}`);
  return <AutomationWorkspace key={scope} />;
}
