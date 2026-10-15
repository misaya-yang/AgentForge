/** One explicit credential submission; durable recovery contains metadata only. */
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { createSourceConnection, getSourceConnection, type SourceConnectionIntent, type SourceConnectionReceipt } from "@/api/knowledge";
import { Button } from "@/components/ui/button";
import { useAuthStore } from "@/store/useAuthStore";
import { cloudHostname, connectionNotAccepted, connectionRejectionReason, connectionStorageKey, readConnectionIntent, validateConnectionReceipt } from "./sourceConnectionContract";

type Props = { datasetId: string; enabled: boolean; reason: string | null };
export function SourceConnectionSetup(props: Props) {
  const userId = useAuthStore(state => state.user?.user_id);
  return userId ? <SourceConnectionSetupSession key={JSON.stringify([userId, props.datasetId])} {...props} userId={userId} /> : null;
}
export function SourceConnectionSetupSession({ datasetId, enabled, reason, userId }: Props & { userId: string }) {
  const { t } = useTranslation();
  const key = connectionStorageKey(userId, datasetId);
  const [initial] = useState(() => { try { return { intent: readConnectionIntent(key), failed: false }; } catch { return { intent: null, failed: true }; } });
  const [intent, setIntent] = useState<SourceConnectionIntent | null>(initial.intent);
  const [receipt, setReceipt] = useState<SourceConnectionReceipt | null>(null);
  const [name, setName] = useState(""); const [domain, setDomain] = useState("");
  const [email, setEmail] = useState(""); const [token, setToken] = useState("");
  const [storageFailed, setStorageFailed] = useState(initial.failed);
  const [error, setError] = useState<string | null>(null); const [busy, setBusy] = useState(false);
  const fence = useRef(false), mounted = useRef(true), ownId = useRef(initial.intent?.request_id ?? null);
  const controller = useRef<AbortController | null>(null);
  useEffect(() => { const live = mounted, read = controller; live.current = true; return () => { live.current = false; read.current?.abort(); }; }, []);
  useEffect(() => { if (!enabled) { setEmail(""); setToken(""); } }, [enabled]);
  function owns(expected: string | null) { if ((readConnectionIntent(key)?.request_id ?? null) !== expected) throw Error("Recovery owned by another request"); }
  async function save() {
    if (!enabled || intent || fence.current || storageFailed || !name.trim() || !cloudHostname.test(domain) || !email || !token) return;
    fence.current = true; setBusy(true); setError(null);
    let current: SourceConnectionIntent;
    try {
      owns(ownId.current);
      current = { request_id: crypto.randomUUID(), name, domain };
      sessionStorage.setItem(key, JSON.stringify(current)); ownId.current = current.request_id; setIntent(current);
    } catch { setStorageFailed(true); setEmail(""); setToken(""); fence.current = false; setBusy(false); return; }
    // No secret is copied to storage or retained for retry. The HTTP request is
    // the sole credential-bearing object after the form has been cleared.
    const submitted = { ...current, email, api_token: token }; setEmail(""); setToken("");
    try {
      const saved = validateConnectionReceipt(await createSourceConnection(datasetId, submitted), current);
      if (mounted.current) setReceipt(saved);
    } catch (failure) {
      if (connectionNotAccepted(failure)) {
        try { owns(current.request_id); sessionStorage.removeItem(key); ownId.current = null;
          if (mounted.current) { setIntent(null); setError(connectionRejectionReason(failure)); }
        } catch { if (mounted.current) setStorageFailed(true); }
      }
    } finally { submitted.email = ""; submitted.api_token = ""; fence.current = false; if (mounted.current) setBusy(false); }
  }
  async function recheck() {
    if (!intent || fence.current) return;
    fence.current = true; setBusy(true); setError(null); setReceipt(null); controller.current = new AbortController();
    try {
      const saved = validateConnectionReceipt(await getSourceConnection(datasetId, intent.request_id, controller.current.signal), intent);
      if (mounted.current) setReceipt(saved);
    } catch { if (mounted.current) setError("readFailed"); }
    finally { fence.current = false; if (mounted.current) setBusy(false); }
  }
  function another() {
    if (!receipt || fence.current) return;
    try { owns(intent?.request_id ?? null); sessionStorage.removeItem(key); ownId.current = null; }
    catch { setStorageFailed(true); return; }
    setIntent(null); setReceipt(null); setName(""); setDomain(""); setEmail(""); setToken(""); setError(null);
  }
  if (!enabled && !intent && !storageFailed && reason !== "encryption_key_required") return null;
  const label = (field: string) => t(`knowledge.sources.connectionSetup.${field}`);
  return <div className="mt-4 space-y-3 border-t pt-3" data-testid="source-connection-setup">
    <p className="text-sm font-medium">{label("title")}</p><p className="text-xs text-muted-foreground">{label("scope")}</p>
    {!enabled && reason === "encryption_key_required" && <p role="status">{label("keyRequired")}</p>}
    {storageFailed && <p role="alert">{label("storage")}</p>}{error && <p role="alert">{label(error)}</p>}
    {!intent && enabled && <div className="space-y-2">
      <label className="block">{label("name")}<input name="connection-name" value={name} maxLength={255} disabled={busy || storageFailed} onChange={event => setName(event.target.value)} /></label>
      <label className="block">{label("domain")}<input name="cloud-hostname" value={domain} maxLength={77} disabled={busy || storageFailed} onChange={event => setDomain(event.target.value)} /></label>
      <label className="block">{label("email")}<input name="confluence-email" type="email" autoComplete="off" value={email} maxLength={4096} disabled={busy || storageFailed} onChange={event => setEmail(event.target.value)} /></label>
      <label className="block">{label("token")}<input name="confluence-token" type="password" autoComplete="new-password" value={token} maxLength={4096} disabled={busy || storageFailed} onChange={event => setToken(event.target.value)} /></label>
      <Button size="sm" disabled={busy || storageFailed || !name.trim() || !cloudHostname.test(domain) || !email || !token} onClick={() => void save()}>{label("save")}</Button>
    </div>}
    {intent && <div className="space-y-2">
      <p className="break-all text-xs">{t("knowledge.sources.connectionSetup.requestId", { id: intent.request_id })}</p>
      <p role={receipt ? "status" : "alert"}>{label(busy ? "saving" : receipt ? "saved" : "uncertain")}</p>
      <Button size="sm" variant="outline" disabled={busy} onClick={() => void recheck()}>{label("recheck")}</Button>
      {receipt && enabled && <Button size="sm" variant="outline" disabled={busy || storageFailed} onClick={another}>{label("another")}</Button>}
    </div>}
  </div>;
}
