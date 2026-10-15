/** Explicit reversible application pause, with metadata-only uncertain-write recovery. */
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { replaceSourceCredentials, type SourceCredentialIntent, changeSourceConnection, getManagedSourceConnections, getSourceConnectionImpact, type SourceConnectionChange, type SourceConnectionControl, type SourceConnectionImpact } from "@/api/knowledge";
import { Button } from "@/components/ui/button";
import { useAuthStore } from "@/store/useAuthStore";
import { controlKey, readControlChange, validateControl, validateImpact } from "./sourceConnectionControlState";

type Props = { datasetId: string; enabled: boolean; reconnectEnabled?: boolean; onChanged: () => void };
export function SourceConnectionControls(props: Props) {
  const userId = useAuthStore(state => state.user?.user_id);
  return userId ? <SourceConnectionControlsSession key={JSON.stringify([userId, props.datasetId])} {...props} userId={userId} /> : null;
}
export function SourceConnectionControlsSession({ datasetId, userId, enabled, reconnectEnabled = false, onChanged }: Props & { userId: string }) {
  const { t } = useTranslation();
  const label = (key: string) => t(`knowledge.sources.connectionControls.${key}`);
  const key = controlKey(userId, datasetId);
  const [initial] = useState(() => { try { return { change: readControlChange(key), failed: false }; } catch { return { change: null, failed: true }; } });
  const [pending, setPending] = useState<SourceConnectionChange | SourceCredentialIntent | null>(initial.change);
  const [storageFailed, setStorageFailed] = useState(initial.failed);
  const [connections, setConnections] = useState<SourceConnectionControl[]>([]);
  const [listNext, setListNext] = useState<number | null>(null);
  const [impact, setImpact] = useState<SourceConnectionImpact | null>(null);
  const [confirmed, setConfirmed] = useState(false);
  const [busy, setBusy] = useState(false), [failed, setFailed] = useState(false);
  const [email, setEmail] = useState(""); const [token, setToken] = useState("");
  const [saved, setSaved] = useState(false);
  const fence = useRef(false), mounted = useRef(true), available = useRef(enabled);
  available.current = enabled;
  const controller = useRef<AbortController | null>(null);
  const generation = useRef(0);
  useEffect(() => { const live = mounted, read = controller; live.current = true; return () => { live.current = false; read.current?.abort(); }; }, []);
  useEffect(() => { generation.current += 1; controller.current?.abort(); setConfirmed(false); setImpact(null); setConnections([]); setListNext(null); }, [enabled, reconnectEnabled]);
  useEffect(() => { setEmail(""); setToken(""); }, [enabled, reconnectEnabled]);
  function owns(change: SourceConnectionChange | SourceCredentialIntent | null) {
    if (JSON.stringify(readControlChange(key)) !== JSON.stringify(change)) throw Error("Recovery changed elsewhere");
  }
  async function list(offset = 0) {
    if (!enabled || fence.current || pending) return;
    fence.current = true; setBusy(true); setFailed(false); setConfirmed(false); setImpact(null); setEmail(""); setToken("");
    const epoch = generation.current;
    controller.current = new AbortController();
    try {
      const result = await getManagedSourceConnections(datasetId, offset, controller.current.signal);
      if (!Array.isArray(result.connections) || !(result.next_offset === null || Number.isSafeInteger(result.next_offset))) throw Error("Invalid list");
      const rows = result.connections.map(row => validateControl(row));
      if (mounted.current && available.current && generation.current === epoch) { setConnections(rows); setListNext(result.next_offset); }
    } catch { if (mounted.current && generation.current === epoch) setFailed(true); }
    finally { fence.current = false; if (mounted.current) setBusy(false); }
  }
  async function read(id: string, offset = 0) {
    if (fence.current) return;
    fence.current = true; setBusy(true); setFailed(false); setConfirmed(false); setImpact(null); setEmail(""); setToken("");
    const epoch = generation.current;
    controller.current = new AbortController();
    try {
      const result = validateImpact(await getSourceConnectionImpact(datasetId, id, offset, controller.current.signal), id);
      if (mounted.current && generation.current === epoch) setImpact(result);
    } catch { if (mounted.current && generation.current === epoch) setFailed(true); }
    finally { fence.current = false; if (mounted.current) setBusy(false); }
  }
  async function change() {
    if (!enabled || !impact || !confirmed || pending || fence.current || storageFailed) return;
    const current = impact.connection;
    if (current.status !== "active" && current.status !== "disabled") return;
    const attempt: SourceConnectionChange = { connection_id: current.connection_id, expected_revision: current.revision, expected_status: current.status, status: current.status === "active" ? "disabled" : "active" };
    fence.current = true; setBusy(true); setFailed(false); setSaved(false); setConfirmed(false);
    try { owns(null); sessionStorage.setItem(key, JSON.stringify(attempt)); setPending(attempt); }
    catch { setStorageFailed(true); fence.current = false; setBusy(false); return; }
    setImpact(null); setEmail(""); setToken("");
    try {
      const result = validateControl(await changeSourceConnection(datasetId, attempt), attempt.connection_id);
      if (result.status !== attempt.status || result.revision === attempt.expected_revision) throw Error("Uncertain change");
      if (mounted.current) { setSaved(true); onChanged(); }
    } catch { /* Every uncertain/rejected response requires a fresh read, never blind retry. */ }
    finally { fence.current = false; if (mounted.current) setBusy(false); }
  }
  async function reconnect() {
    if (!enabled || !reconnectEnabled || !impact || !confirmed || pending || fence.current || storageFailed || !email || !token) return;
    const current = impact.connection;
    if (current.status !== "active" && current.status !== "disabled") return;
    const attempt: SourceCredentialIntent = { operation: "credentials", connection_id: current.connection_id, expected_revision: current.revision, expected_status: current.status };
    fence.current = true; setBusy(true); setFailed(false); setSaved(false); setConfirmed(false);
    try { owns(null); sessionStorage.setItem(key, JSON.stringify(attempt)); setPending(attempt); }
    catch { setStorageFailed(true); setEmail(""); setToken(""); fence.current = false; setBusy(false); return; }
    const credentials = { email, api_token: token }; setEmail(""); setToken(""); setImpact(null);
    try {
      const result = validateControl(await replaceSourceCredentials(datasetId, attempt, credentials), attempt.connection_id);
      if (result.status !== attempt.expected_status || result.revision === attempt.expected_revision) throw Error("Uncertain replacement");
      if (mounted.current) { setSaved(true); onChanged(); }
    } catch { /* Metadata readback only; never retain or retry credentials. */ }
    finally { credentials.email = ""; credentials.api_token = ""; fence.current = false; if (mounted.current) setBusy(false); }
  }
  function acceptCurrent() {
    if (!pending || !impact || fence.current || impact.connection.connection_id !== pending.connection_id) return;
    try { owns(pending); sessionStorage.removeItem(key); }
    catch { setStorageFailed(true); return; }
    setPending(null); setSaved(false); setConfirmed(false); onChanged();
  }
  if (!enabled && !pending && !storageFailed) return null;
  return <div className="mt-4 space-y-3 border-t pt-3" data-testid="source-connection-controls">
    <p className="text-sm font-medium">{label("title")}</p>
    <p className="text-xs text-muted-foreground">{label("scope")}</p>
    {storageFailed && <p role="alert">{label("storage")}</p>}{failed && <p role="alert">{label("readFailed")}</p>}
    {!pending && enabled && <>
      <Button size="sm" variant="outline" disabled={busy} onClick={() => void list()}>{label("load")}</Button>
      {connections.map(row => <div key={row.connection_id} className="space-y-1">
        <p>{row.name} · {row.domain} · {row.status}</p>
        <Button size="sm" variant="outline" disabled={busy} onClick={() => void read(row.connection_id)}>{label("review")} {row.name}</Button>
      </div>)}
      {listNext !== null && <Button size="sm" disabled={busy} onClick={() => void list(listNext)}>{label("moreConnections")}</Button>}
    </>}
    {pending && <>
      <p role={saved ? "status" : "alert"}>{label(pending && "operation" in pending ? (saved ? "credentialsSaved" : "credentialsUncertain") : (saved ? "saved" : "uncertain"))}</p>
      <Button size="sm" variant="outline" disabled={busy} onClick={() => void read(pending.connection_id)}>{label("recheck")}</Button>
    </>}
    {impact && <div className="space-y-2">
      <p>{impact.connection.name} · {impact.connection.domain} · {impact.connection.status}</p>
      <p>{t("knowledge.sources.connectionControls.impact", { count: impact.binding_count })}</p>
      {impact.bindings.map(binding => <p key={binding.binding_id}>{binding.space_name || binding.binding_id} · {binding.dataset_id} · {binding.status}</p>)}
      {impact.hidden_count > 0 && <p>{t("knowledge.sources.connectionControls.hidden", { count: impact.hidden_count })}</p>}
      {impact.next_offset !== null && <Button size="sm" variant="outline" disabled={busy} onClick={() => void read(impact.connection.connection_id, impact.next_offset!)}>{label("moreBindings")}</Button>}
      {!pending && impact.connection.status !== "active" && impact.connection.status !== "disabled" && <p role="status">{label("unsupported")}</p>}
      {pending ? <Button size="sm" disabled={busy || storageFailed} onClick={acceptCurrent}>{label("acceptCurrent")}</Button> : enabled && (impact.connection.status === "active" || impact.connection.status === "disabled") && <>
        <label className="flex gap-2"><input type="checkbox" checked={confirmed} disabled={busy} onChange={event => setConfirmed(event.target.checked)} />{label("confirm")}</label>
        {reconnectEnabled ? <div className="space-y-2">
          <p className="text-xs">{label("credentialsScope")}</p>
          <label className="block">{label("email")}<input name="replacement-email" type="email" autoComplete="off" value={email} maxLength={320} disabled={busy || storageFailed} onChange={event => setEmail(event.target.value)} /></label>
          <label className="block">{label("token")}<input name="replacement-token" type="password" autoComplete="new-password" value={token} maxLength={4096} disabled={busy || storageFailed} onChange={event => setToken(event.target.value)} /></label>
          <Button size="sm" disabled={busy || !confirmed || storageFailed || !email || !token} onClick={() => void reconnect()}>{label("replaceCredentials")}</Button>
        </div> : <p className="text-xs">{label("credentialsUnavailable")}</p>}
        <Button size="sm" disabled={busy || !confirmed || storageFailed} onClick={() => void change()}>{label(impact.connection.status === "active" ? "pause" : "resume")}</Button>
      </>}
    </div>}
  </div>;
}
