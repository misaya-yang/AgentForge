import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { Alert, Button, Checkbox, Drawer, Empty, Select, Tag, Typography } from "antd";
import { GitCompare, RefreshCw } from "lucide-react";
import { useTranslation } from "react-i18next";

import { getAgent, listAgentVersions } from "@/api/agents";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { generateUUID } from "@/lib/utils";
import { useAuthStore } from "@/store/useAuthStore";
import type { AgentVersion } from "@/types/agents";
import { compareAgentVersions, validateVersionRead, type ComparisonValue } from "./agentVersionComparisonState";

const prefix = "agents.studio.versionComparison";

function VersionComparisonContent({ agentId, isCurrent }: { agentId: string; isCurrent: () => boolean }) {
  const { t } = useTranslation();
  const active = useRef(true);
  const generation = useRef(0);
  const [versions, setVersions] = useState<AgentVersion[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);
  const [beforeId, setBeforeId] = useState<string>();
  const [afterId, setAfterId] = useState<string>();
  const [showUnchanged, setShowUnchanged] = useState(false);
  const ownsView = () => active.current && isCurrent();
  const refresh = async () => {
    if (!ownsView()) return;
    const attempt = ++generation.current;
    setLoading(true); setError(false); setVersions([]);
    try {
      // A fresh authorized read deliberately does not reuse Studio's older
      // unscoped version cache. Both responses must belong to this observation.
      const [agent, rows] = await Promise.all([getAgent(agentId), listAgentVersions(agentId)]);
      if (!ownsView() || attempt !== generation.current) return;
      setVersions(validateVersionRead(agentId, agent, rows));
    } catch {
      if (ownsView() && attempt === generation.current) { setVersions([]); setError(true); }
    } finally {
      if (ownsView() && attempt === generation.current) setLoading(false);
    }
  };
  useLayoutEffect(() => {
    active.current = true;
    void refresh();
    return () => { active.current = false; generation.current += 1; };
    // A new Agent, credential or authority remounts the complete comparison.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const current = isCurrent();
  const available = current && !loading && !error;
  const before = available ? versions.find(version => version.agent_version_id === beforeId) : undefined;
  const after = available ? versions.find(version => version.agent_version_id === afterId) : undefined;
  const sameSelection = Boolean(before && after && beforeId === afterId);
  const rows = before && after && !sameSelection ? compareAgentVersions(before, after) : [];
  const changed = rows.filter(row => row.changed);
  const shown = showUnchanged ? rows : changed;
  const options = available ? versions.map(version => ({ value: version.agent_version_id,
    label: `v${version.version_number} · r${version.source_draft_revision} · ${version.agent_version_id}` })) : [];
  const display = (value: ComparisonValue) => value.state === "value"
    ? value.text === "" ? t(`${prefix}.emptyValue`) : value.text
    : t(`${prefix}.${value.state}`);
  const snapshot = (version: AgentVersion, side: string) => <section className="agent-version-snapshot" aria-label={t(`${prefix}.${side}`)}>
    <strong>{t(`${prefix}.${side}`)} · v{version.version_number}</strong>
    <dl>
      <div><dt>{t(`${prefix}.versionId`)}</dt><dd>{version.agent_version_id}</dd></div>
      <div><dt>{t(`${prefix}.sourceRevision`)}</dt><dd>r{version.source_draft_revision}</dd></div>
      <div><dt>{t(`${prefix}.storedHash`)}</dt><dd>{version.spec_hash}</dd></div>
      <div><dt>{t(`${prefix}.createdAt`)}</dt><dd>{version.created_at}</dd></div>
    </dl>
  </section>;
  return <div className="agent-version-comparison" data-testid="agent-version-comparison-content">
    <Alert type="info" showIcon title={t(`${prefix}.scope`)} description={t(`${prefix}.boundary`)} />
    <div><Button icon={<RefreshCw size={14} />} disabled={loading || !current} onClick={() => void refresh()}>{t(`${prefix}.refresh`)}</Button></div>
    {(loading || !current) && <p role="status">{t(`${prefix}.loading`)}</p>}
    {error && current && <Alert type="error" showIcon title={t(`${prefix}.readError`)} />}
    {available && versions.length < 2 && <Empty description={t(`${prefix}.needTwo`)} />}
    {available && versions.length >= 2 && <div className="agent-version-comparison-selectors">
      <label><span>{t(`${prefix}.before`)}</span><Select showSearch optionFilterProp="label" aria-label={t(`${prefix}.before`)} value={beforeId} options={options} placeholder={t(`${prefix}.chooseVersion`)} onChange={setBeforeId} /></label>
      <label><span>{t(`${prefix}.after`)}</span><Select showSearch optionFilterProp="label" aria-label={t(`${prefix}.after`)} value={afterId} options={options} placeholder={t(`${prefix}.chooseVersion`)} onChange={setAfterId} /></label>
      <Button disabled={!before || !after || sameSelection} onClick={() => { if (ownsView()) { setBeforeId(afterId); setAfterId(beforeId); } }}>{t(`${prefix}.swap`)}</Button>
    </div>}
    {available && ((beforeId && !before) || (afterId && !after)) && <Alert type="warning" showIcon title={t(`${prefix}.selectionUnavailable`)} />}
    {sameSelection && <Alert type="info" showIcon title={t(`${prefix}.sameSelection`)} />}
    {before && after && !sameSelection && <>
      <div className="agent-version-comparison-pair">{snapshot(before, "before")}{snapshot(after, "after")}</div>
      <p role="status">{t(changed.length ? `${prefix}.hasChanges` : `${prefix}.noChanges`, { count: changed.length })}</p>
      <Checkbox checked={showUnchanged} onChange={event => setShowUnchanged(event.target.checked)}>{t(`${prefix}.showUnchanged`)}</Checkbox>
      <p className="agent-version-comparison-note">{t(`${prefix}.omitted`)}</p>
      {shown.map(row => <article key={row.key} className="agent-version-comparison-row" data-testid={`agent-version-diff-${row.key}`}>
        <header><strong>{t(`${prefix}.sections.${row.section}`)}{row.position ? ` · #${row.position}` : ""} · {t(`${prefix}.fields.${row.field}`)}</strong><Tag color={row.changed ? "blue" : "default"}>{t(`agents.studio.release.${row.changed ? "changed" : "unchanged"}`)}</Tag></header>
        <div className="agent-version-comparison-values">
          <div><span>{t(`${prefix}.before`)}</span><pre tabIndex={0} aria-label={`${t(`${prefix}.before`)} · ${t(`${prefix}.fields.${row.field}`)}`}>{display(row.before)}</pre></div>
          <div><span>{t(`${prefix}.after`)}</span><pre tabIndex={0} aria-label={`${t(`${prefix}.after`)} · ${t(`${prefix}.fields.${row.field}`)}`}>{display(row.after)}</pre></div>
        </div>
      </article>)}
    </>}
  </div>;
}

function ComparisonEntry({ agentId, isCurrent }: { agentId: string; isCurrent: () => boolean }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  return <section className="agent-version-comparison-entry">
    <div><Typography.Title level={4}>{t(`${prefix}.title`)}</Typography.Title><Typography.Paragraph type="secondary">{t(`${prefix}.description`)}</Typography.Paragraph></div>
    <Button icon={<GitCompare size={15} />} onClick={() => { if (isCurrent()) setOpen(true); }}>{t(`${prefix}.open`)}</Button>
    <Drawer title={t(`${prefix}.title`)} placement="right" size="large" open={open} onClose={() => setOpen(false)}>
      {open && <VersionComparisonContent agentId={agentId} isCurrent={isCurrent} />}
    </Drawer>
  </section>;
}

export function AgentVersionComparison({ agentId }: { agentId: string }) {
  const token = useAuthStore(state => state.token);
  const authority = useAuthStore(state => sdkAuthorityScope(state.user));
  const authenticated = useAuthStore(state => state.isAuthenticated);
  const [transition, setTransition] = useState(0);
  const scope = useMemo(() => ({ id: generateUUID(), token, authority, agentId, invalidated: false }), [token, authority, agentId, transition]);
  useLayoutEffect(() => useAuthStore.subscribe((state, previous) => {
    if (state.token !== previous.token || state.isAuthenticated !== previous.isAuthenticated
      || sdkAuthorityScope(state.user) !== sdkAuthorityScope(previous.user)) {
      // Store transitions are observed synchronously even if React batches
      // A → B → A into one render. An old observation never becomes current again.
      scope.invalidated = true;
      setTransition(value => value + 1);
    }
  }), [scope]);
  const isCurrent = () => {
    const state = useAuthStore.getState();
    return Boolean(!scope.invalidated && state.isAuthenticated && state.token && state.user && state.token === scope.token && sdkAuthorityScope(state.user) === scope.authority);
  };
  if (!authenticated || !token) return null;
  return <ComparisonEntry key={scope.id} agentId={agentId} isCurrent={isCurrent} />;
}
