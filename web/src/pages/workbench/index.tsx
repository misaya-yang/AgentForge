import { useState } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { ArrowUpRight, ArrowRight, Activity, CheckCircle2, AlertCircle, RefreshCw, ShieldCheck, Server, Workflow, Coins, Gauge, Zap } from "lucide-react";
import { useTranslation } from "react-i18next";
import dayjs from "dayjs";
import { getUsageSummary } from "@/api/usage";
import { useSetupState } from "@/api/setup";
import { useAuthStore } from "@/store/useAuthStore";
import { GROUP_ORDER, useConsoleNavigation } from "@/config/consoleNavigation";

export function WorkbenchPage() {
  const { t, i18n } = useTranslation();
  const { user, hasPermission } = useAuthStore();
  const { items } = useConsoleNavigation();
  const [days, setDays] = useState(7);
  const end = dayjs().format("YYYY-MM-DD");
  const start = dayjs().subtract(days - 1, "day").format("YYYY-MM-DD");
  const canViewUsage = hasPermission("console:usage:view");
  const canViewServices = hasPermission("console:services:view");
  const setup = useSetupState();
  const usage = useQuery({
    queryKey: ["workbench-usage", user?.user_id, start, end],
    queryFn: () => getUsageSummary({ start_date: start, end_date: end }),
    enabled: canViewUsage,
    staleTime: 60_000,
    retry: false,
  });
  const collectionUnavailable = ["collection_error", "unavailable"].includes(usage.data?.data_status || "");
  const summary = collectionUnavailable ? undefined : usage.data;
  const number = new Intl.NumberFormat(i18n.language, { notation: "compact", maximumFractionDigits: 1 });
  const currency = new Intl.NumberFormat(i18n.language, { style: "currency", currency: "USD", maximumFractionDigits: 2 });
  const formatNumber = (value: number | undefined) => value == null || !Number.isFinite(value) ? "—" : number.format(value);
  const shortcuts = items.filter(item => ["/assistant", "/agents", "/knowledge", "/playground", "/eval", "/services"].includes(item.key));
  const assistant = items.find(item => item.key === "/assistant");
  const metrics = [
    { key: "requests", icon: Activity, value: formatNumber(summary?.total_requests) },
    { key: "tokens", icon: Zap, value: formatNumber(summary?.total_tokens) },
    { key: "success", icon: Gauge, value: summary && summary.total_requests > 0 && summary.success_rate != null ? `${summary.success_rate.toFixed(1)}%` : "—" },
    { key: "cost", icon: Coins, value: summary ? currency.format(summary.total_cost_usd) : "—" },
  ];

  return (
    <div className="workbench">
      <section className="workbench-hero" aria-labelledby="workbench-title">
        <div className="workbench-hero-copy">
          <span className="console-eyebrow"><Workflow size={14} aria-hidden="true" />{t("workbench.eyebrow")}</span>
          <h1 id="workbench-title">{t("workbench.greeting", { name: user?.display_name || user?.user_id || t("common.user") })}</h1>
          <p>{t("workbench.intro")}</p>
          <div className="workbench-hero-actions">
            {assistant && <Link className="console-primary-action" to="/assistant">{t("workbench.openAssistant")}<ArrowRight size={16} aria-hidden="true" /></Link>}
            {items.some(item => item.key === "/dashboard") && <Link className="console-secondary-action" to="/dashboard">{t("workbench.openDashboard")}<ArrowUpRight size={15} aria-hidden="true" /></Link>}
          </div>
        </div>
        <div className="workbench-identity">
          <ShieldCheck size={22} aria-hidden="true" />
          <span className="console-eyebrow">{t("workbench.workspace")}</span>
          <strong>{user?.department || t("workbench.personalWorkspace")}</strong>
          <span>{t("workbench.accessNote")}</span>
        </div>
      </section>

      {canViewUsage && <section className="workbench-section" aria-labelledby="workbench-usage-title" aria-busy={usage.isFetching}>
        <div className="workbench-section-heading">
          <div><h2 id="workbench-usage-title">{t("workbench.usage")}</h2><p>{t("workbench.usageScope")}</p></div>
          <div className="workbench-controls">
            <label className="sr-only" htmlFor="workbench-period">{t("workbench.period")}</label>
            <select id="workbench-period" value={days} onChange={event => setDays(Number(event.target.value))}>
              <option value={7}>{t("workbench.sevenDays")}</option><option value={30}>{t("workbench.thirtyDays")}</option>
            </select>
            <button type="button" className="console-icon-button" onClick={() => void usage.refetch()} disabled={usage.isFetching} aria-label={t("workbench.refresh")}>
              <RefreshCw size={16} className={usage.isFetching ? "animate-spin" : undefined} aria-hidden="true" />
            </button>
          </div>
        </div>
        {usage.isError && <div className="console-notice console-notice-warning" role="status"><AlertCircle size={17} aria-hidden="true" />{t(summary ? "workbench.refreshFailed" : "workbench.usageError")}</div>}
        {collectionUnavailable && <div className="console-notice console-notice-warning" role="status"><AlertCircle size={17} aria-hidden="true" />{t("workbench.collectionUnavailable")}</div>}
        <div className="workbench-metrics">
          {metrics.map(metric => <div className="workbench-metric" key={metric.key}>
            <div><span>{t(`workbench.metrics.${metric.key}`)}</span><metric.icon size={16} aria-hidden="true" /></div>
            <strong className={usage.isPending ? "console-loading-value" : undefined}>{usage.isPending ? "···" : metric.value}</strong>
            <span className="workbench-metric-note">{t(`workbench.metricNotes.${metric.key}`)}</span>
          </div>)}
        </div>
        {summary && <p className="workbench-data-note" role="status">
          {t("workbench.dataStatus", { status: t(`workbench.dataStates.${summary.data_status}`, summary.data_status) })}
          {summary.total_requests === 0 && <> · {t("workbench.noUsage")}</>}
          {summary.last_ingested_at && <> · {t("workbench.lastIngested", { time: new Date(summary.last_ingested_at).toLocaleString(i18n.language) })}</>}
        </p>}
      </section>}

      <div className="workbench-columns">
        <section className="workbench-section" aria-labelledby="workbench-tools-title">
          <div className="workbench-section-heading"><div><h2 id="workbench-tools-title">{t("workbench.tools")}</h2><p>{t("workbench.toolsHint")}</p></div></div>
          <div className="workbench-tools">
            {shortcuts.map(item => <Link className="workbench-tool" key={item.key} to={item.key}>
              <span className="workbench-tool-icon"><item.icon size={21} strokeWidth={1.6} aria-hidden="true" /></span>
              <ArrowUpRight className="workbench-tool-arrow" size={16} aria-hidden="true" />
              <h3>{t(item.labelKey)}</h3><p>{t(`workbench.toolDescriptions.${item.key.slice(1)}`)}</p>
            </Link>)}
            {!shortcuts.length && <p className="console-empty">{t("workbench.noTools")}</p>}
          </div>
        </section>
        <aside className="workbench-aside">
          {canViewServices && <section className="workbench-panel" aria-labelledby="workbench-readiness-title" aria-busy={setup.isFetching}>
            <div className="workbench-panel-heading"><Server size={17} aria-hidden="true" /><h2 id="workbench-readiness-title">{t("workbench.readiness")}</h2></div>
            {setup.isPending ? <p role="status" className="text-muted-foreground">{t("workbench.checking")}</p> : setup.isError ? <>
              <p role="status">{t("workbench.setupError")}</p><button type="button" className="console-text-action" onClick={() => void setup.refetch()} disabled={setup.isFetching}>{t("workbench.retry")}</button>
            </> : setup.data ? <>
              <div className={`console-readiness ${setup.data.configured ? "is-ready" : "needs-setup"}`}>
                {setup.data.configured ? <CheckCircle2 size={17} aria-hidden="true" /> : <AlertCircle size={17} aria-hidden="true" />}
                {t(setup.data.configured ? "workbench.configured" : "workbench.setupRequired")}
              </div>
              <p>{t(setup.data.configured ? "workbench.configuredNote" : "workbench.setupNote")}</p>
              <dl className="workbench-details"><div><dt>{t("workbench.mode")}</dt><dd>{setup.data.mode || "—"}</dd></div><div><dt>{t("workbench.model")}</dt><dd>{setup.data.default_model || t("workbench.notSet")}</dd></div></dl>
              <Link to="/services" className="console-text-action">{t("workbench.manageServices")}<ArrowRight size={14} aria-hidden="true" /></Link>
            </> : null}
          </section>}
          <section className="workbench-panel" aria-labelledby="workbench-explore-title">
            <div className="workbench-panel-heading"><Workflow size={17} aria-hidden="true" /><h2 id="workbench-explore-title">{t("workbench.explore")}</h2></div>
            {GROUP_ORDER.map(group => {
              const destinations = items.filter(item => item.group === group);
              return destinations.length ? <div className="workbench-directory" key={group}><h3>{t(`nav.group.${group}`)}</h3>{destinations.map(item => <Link to={item.key} key={item.key}><span>{t(item.labelKey)}</span><ArrowUpRight size={13} aria-hidden="true" /></Link>)}</div> : null;
            })}
          </section>
        </aside>
      </div>
    </div>
  );
}
