import { useLayoutEffect, useRef, useState } from "react";
import { isHttpError } from '@/lib/fetchClient';
import { Alert, App as AntApp, Button, Empty, Input, Modal, Tag } from "antd";
import { Copy, KeyRound, RefreshCw, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import { createAgentApiToken, listAgentApiTokens, revokeAgentApiToken, rotateAgentApiToken } from "@/api/agents";
import type { AgentApiToken, AgentPublication } from "@/types/agents";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";

type Props = { publication: AgentPublication; canManage: boolean };
export function ApiTokenPanel({ publication, canManage }: Props) {
  const scope = useAuthenticatedReadScope();
  const allowed = useRef(canManage);
  allowed.current = canManage;
  return canManage && scope.isCurrent()
    ? <TokenPanelContent key={`${scope.id}:${publication.publication_id}`} publication={publication} isCurrent={() => allowed.current && scope.isCurrent()} />
    : null;
}

function TokenPanelContent({ publication, isCurrent }: { publication: AgentPublication; isCurrent: () => boolean }) {
  const { t } = useTranslation();
  const { message: messageApi } = AntApp.useApp();
  const active = useRef(true);
  const working = useRef(false);
  const reading = useRef(false);
  const blocked = useRef(false);
  const [name, setName] = useState("");
  const [issued, setIssued] = useState<string | null>(null);
  const [tokens, setTokens] = useState<AgentApiToken[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [readFailed, setReadFailed] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [copyFailed, setCopyFailed] = useState(false);
  const ownsView = () => active.current && isCurrent();
  const refresh = async () => {
    if (!ownsView() || reading.current) return false;
    reading.current = true;
    setLoading(true);
    try {
      const rows = await listAgentApiTokens(publication.publication_id);
      if (!ownsView()) return false;
      setTokens(rows); setLoaded(true); setReadFailed(false);
      return true;
    } catch (error) {
      if (ownsView()) {
        setReadFailed(true);
        if (isHttpError(error) && [401, 403, 404].includes(error.response?.status ?? 0)) setIssued(null);
      }
      return false;
    } finally {
      reading.current = false;
      if (ownsView()) setLoading(false);
    }
  };
  useLayoutEffect(() => {
    active.current = true;
    void refresh();
    return () => { active.current = false; };
    // The wrapper remounts for each publication/account/credential/authority scope.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const ready = loaded && !readFailed && !loading && !busy;
  const mutate = async (kind: "create" | "rotate" | "revoke", token?: AgentApiToken) => {
    if (!ownsView() || working.current || reading.current || blocked.current || !ready || issued) return;
    if (kind === "create" ? !name.trim() : !token || token.revoked_at) return;
    working.current = true; setBusy(true); setNotice(null); setCopyFailed(false);
    let accepted = false;
    try {
      try {
        if (kind === "create") {
          const result = await createAgentApiToken(publication.publication_id, {
            name: name.trim(), scopes: ["chat:write", "sessions:write", "attachments:write", "feedback:write"],
          });
          if (!ownsView()) return;
          setIssued(result.token); setName("");
        } else if (kind === "rotate") {
          const result = await rotateAgentApiToken(publication.publication_id, token!.token_id);
          if (!ownsView()) return;
          setIssued(result.token);
        } else {
          await revokeAgentApiToken(publication.publication_id, token!.token_id);
          if (!ownsView()) return;
        }
        accepted = true;
      } catch (error) {
        if (!ownsView()) return;
        blocked.current = true;
        const denied = isHttpError(error) && [401, 403, 404].includes(error.response?.status ?? 0);
        if (denied) setIssued(null);
        // Raw credential API errors can contain response/request secrets.
        setNotice(denied ? "tokenPermissionDenied" : "tokenOperationFailed");
      }
      if (!ownsView()) return;
      const observed = await refresh();
      if (!ownsView()) return;
      if (accepted && !observed) {
        blocked.current = true; setNotice("tokenWriteConfirmed");
      }
    } finally {
      working.current = false;
      if (ownsView()) setBusy(false);
    }
  };
  const disabled = !ready || blocked.current || Boolean(issued);
  return <section className="agent-api-token-panel" data-testid="agent-api-token-panel">
    <header><div><KeyRound size={17} /><strong>{t("agents.studio.channels.tokensTitle")}</strong></div><Tag>{t("agents.studio.channels.hashed")}</Tag></header>
    {notice && <Alert type="warning" showIcon title={t(`agents.studio.channels.${notice}`)} />}
    {blocked.current && <div>
      <p>{t("agents.studio.channels.tokenNewDecisionHelp")}</p>
      <Button disabled={!ready || Boolean(issued)} onClick={() => {
        if (!ownsView() || working.current || reading.current || !ready || issued) return;
        blocked.current = false; setNotice(null); setName("");
      }}>{t("agents.studio.channels.tokenNewDecision")}</Button>
    </div>}
    <div className="agent-api-token-create">
      <Input value={name} disabled={disabled} maxLength={255} placeholder={t("agents.studio.channels.tokenName")} onChange={(event) => setName(event.target.value)} />
      <Button type="primary" disabled={disabled || !name.trim()} loading={busy} onClick={() => void mutate("create")}>{t("agents.studio.channels.createToken")}</Button>
    </div>
    <Button disabled={busy || loading} onClick={() => { if (!working.current) void refresh(); }}>{t("agents.studio.channels.refreshTokens")}</Button>
    {loading && <p role="status">{t("agents.studio.channels.tokensLoading")}</p>}
    {readFailed && <Alert type="error" showIcon title={t("agents.studio.channels.tokensLoadFailed")} />}
    {loaded && !loading && !readFailed && (tokens.length === 0 ? <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t("agents.studio.channels.noTokens")} /> : tokens.map((token) => <article key={token.token_id}>
      <div><strong>{token.name}</strong><small>{token.scopes.join(" · ")}</small></div>
      <Tag color={token.revoked_at ? "default" : "success"}>{token.revoked_at ? t("agents.studio.channels.revoked") : t("agents.studio.channels.active")}</Tag>
      {!token.revoked_at && <Button size="small" disabled={disabled} icon={<RefreshCw size={13} />} loading={busy} onClick={() => void mutate("rotate", token)}>{t("agents.studio.channels.rotate")}</Button>}
      {!token.revoked_at && <Button danger size="small" disabled={disabled} aria-label={t("agents.studio.channels.revoke")} icon={<Trash2 size={13} />} loading={busy} onClick={() => void mutate("revoke", token)} />}
    </article>))}
    <Modal open={Boolean(issued)} title={t("agents.studio.channels.copyTokenTitle")} footer={<Button type="primary" onClick={() => { setIssued(null); setCopyFailed(false); }}>{t("agents.studio.channels.copied")}</Button>} closable={false} maskClosable={false}>
      <Alert type="warning" showIcon title={t("agents.studio.channels.copyTokenWarning")} />
      {copyFailed && <Alert type="error" showIcon title={t("agents.studio.channels.tokenCopyFailed")} />}
      <Input.Password readOnly value={issued ?? ""} addonAfter={<Button type="text" icon={<Copy size={14} />} onClick={async () => {
        if (!ownsView() || !issued) return;
        try { await navigator.clipboard.writeText(issued); if (ownsView()) { setCopyFailed(false); messageApi.success(t("agents.studio.channels.copySuccess")); } }
        catch { if (ownsView()) setCopyFailed(true); }
      }} />} />
    </Modal>
  </section>;
}
