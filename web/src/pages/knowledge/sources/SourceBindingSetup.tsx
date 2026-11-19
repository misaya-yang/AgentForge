/** Saved connection -> exact authorized space -> manual binding -> existing sync control. */
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { createSourceBinding, getSourceBinding, getSourceConnections, getSourceSpace,
  type SourceBindingReceipt, type SourceBindingRequest, type SourceConnection, type SourceSpace } from "@/api/knowledge";
import { Button } from "@/components/ui/button";
import { useAuthStore } from "@/store/useAuthStore";
import { readSourceBindingRequest, sourceBindingNotAccepted, sourceBindingStorageKey, validateSourceBinding, validateSourceSpace } from "./sourceBindingContract";

type Props = { datasetId: string; enabled: boolean; onSaved: () => void };
export function SourceBindingSetup(props: Props) {
  const userId = useAuthStore(state => state.user?.user_id);
  return userId ? <SourceBindingSetupSession key={JSON.stringify([userId, props.datasetId])} {...props} userId={userId} /> : null;
}

export function SourceBindingSetupSession({ datasetId, enabled, onSaved, userId }: Props & { userId: string }) {
  const { t } = useTranslation();
  const key = sourceBindingStorageKey(userId, datasetId);
  const [initial] = useState(() => { try { return { request: readSourceBindingRequest(key), failed: false }; }
    catch { return { request: null, failed: true }; } });
  const [open, setOpen] = useState(!!initial.request);
  const [request, setRequest] = useState<SourceBindingRequest | null>(initial.request);
  const [receipt, setReceipt] = useState<SourceBindingReceipt | null>(null);
  const [storageFailed, setStorageFailed] = useState(initial.failed);
  const [connections, setConnections] = useState<SourceConnection[] | null>(null);
  const [offset, setOffset] = useState(0);
  const [nextOffset, setNextOffset] = useState<number | null>(null);
  const [connectionId, setConnectionId] = useState("");
  const [spaceKey, setSpaceKey] = useState("");
  const [space, setSpace] = useState<SourceSpace | null>(null);
  const [reading, setReading] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(true);
  const generation = useRef(0);
  const controller = useRef<AbortController | null>(null);
  const busy = useRef(false);
  const storedIdentity = useRef(initial.request?.request_id ?? null);
  useEffect(() => {
    const lifecycle = mounted, reads = generation, activeRead = controller;
    lifecycle.current = true;
    return () => { lifecycle.current = false; reads.current++; activeRead.current?.abort(); };
  }, []);
  useEffect(() => { if (!enabled) { generation.current++; controller.current?.abort(); setReading(false); setSpace(null); } }, [enabled]);

  function invalidateSelection() {
    generation.current++; controller.current?.abort(); setReading(false); setSpace(null); setError(null);
  }
  async function read<T>(load: (signal: AbortSignal) => Promise<T>, apply: (value: T) => void) {
    if (busy.current || reading) return;
    const own = ++generation.current;
    controller.current?.abort(); controller.current = new AbortController();
    setReading(true); setError(null);
    try { const result = await load(controller.current.signal); if (mounted.current && generation.current === own) apply(result); }
    catch { if (mounted.current && generation.current === own) setError("readFailed"); }
    finally { if (mounted.current && generation.current === own) setReading(false); }
  }
  function loadConnections(page: number) {
    if (!enabled || request) return;
    setOpen(true); setConnections(null); setSpace(null); setConnectionId("");
    void read(signal => getSourceConnections(datasetId, page, signal), data => {
      if (!data || !Array.isArray(data.connections) || data.connections.some(c => !c || typeof c.connection_id !== "string" || !c.connection_id || typeof c.name !== "string") ||
          !(data.next_offset === null || (Number.isInteger(data.next_offset) && data.next_offset === page + 50))) throw Error("Invalid connection page");
      setConnections(data.connections); setOffset(page); setNextOffset(data.next_offset);
    });
  }
  function verifySpace() {
    if (!enabled || request || !connectionId || !/^[A-Za-z0-9_~-]{1,255}$/.test(spaceKey)) return;
    const selected = connectionId, typed = spaceKey;
    setSpace(null);
    void read(signal => getSourceSpace(datasetId, selected, typed, signal), data => setSpace(validateSourceSpace(data, selected, typed)));
  }
  function ownsStorage(expected: string | null) {
    if ((readSourceBindingRequest(key)?.request_id ?? null) !== expected) throw Error("Another source setup owns recovery");
  }
  async function save() {
    if (!enabled || !space || request || busy.current || reading || storageFailed) return;
    busy.current = true; setSubmitting(true); setError(null);
    let identity: SourceBindingRequest;
    try {
      ownsStorage(storedIdentity.current);
      identity = { request_id: crypto.randomUUID(), connection_id: space.connection_id, space_key: space.space_key, space_id: space.space_id, root_page_id: space.root_page_id };
      sessionStorage.setItem(key, JSON.stringify(identity)); storedIdentity.current = identity.request_id; setRequest(identity);
    } catch { setStorageFailed(true); setSubmitting(false); busy.current = false; return; }
    try {
      const result = validateSourceBinding(await createSourceBinding(datasetId, identity), datasetId, identity);
      if (mounted.current) { setReceipt(result); onSaved(); }
    } catch (failure) {
      if (sourceBindingNotAccepted(failure)) {
        try {
          ownsStorage(identity.request_id); sessionStorage.removeItem(key); storedIdentity.current = null;
          if (mounted.current) { setRequest(null); setSpace(null); setError("rejected"); }
        } catch { if (mounted.current) setStorageFailed(true); }
      }
      // Unknown errors retain the exact request. A failed GET never clears it.
    } finally { busy.current = false; if (mounted.current) setSubmitting(false); }
  }
  function recheck() {
    if (!request) return;
    const identity = request;
    setReceipt(null);
    void read(signal => getSourceBinding(datasetId, identity.request_id, signal), data => {
      setReceipt(validateSourceBinding(data, datasetId, identity)); onSaved();
    });
  }
  function another() {
    if (!receipt || busy.current || reading) return;
    try { ownsStorage(request?.request_id ?? null); sessionStorage.removeItem(key); storedIdentity.current = null; }
    catch { setStorageFailed(true); return; }
    setRequest(null); setReceipt(null); setSpace(null); setConnectionId(""); setSpaceKey(""); setConnections(null); setOpen(false);
  }
  if (!enabled && !request && !storageFailed) return null;
  return <div className="mt-4 space-y-3 border-t pt-3" data-testid="source-binding-setup">
    <p className="text-sm font-medium">{t("knowledge.sources.bindingSetup.title")}</p>
    <p className="text-xs text-muted-foreground">{t("knowledge.sources.bindingSetup.prerequisite")}</p>
    {storageFailed && <p role="alert">{t("knowledge.sources.bindingSetup.storageFailed")}</p>}
    {error && <p role="alert">{t(`knowledge.sources.bindingSetup.${error}`)}</p>}
    {!request && enabled && <Button size="sm" variant="outline" disabled={reading || submitting || storageFailed} onClick={() => loadConnections(0)}>
      {t("knowledge.sources.bindingSetup.loadConnections")}</Button>}
    {open && !request && enabled && <div className="space-y-2">
      {connections && connections.length === 0 && <p>{t("knowledge.sources.bindingSetup.noConnections")}</p>}
      {connections && connections.length > 0 && <>
        <label className="block text-sm">{t("knowledge.sources.bindingSetup.connection")}
          <select className="mt-1 w-full rounded border bg-background p-2" value={connectionId} disabled={submitting}
            onChange={event => { invalidateSelection(); setConnectionId(event.target.value); }}>
            <option value="">{t("knowledge.sources.bindingSetup.choose")}</option>
            {connections.map(item => <option key={item.connection_id} value={item.connection_id}>{item.name} ({item.connection_id})</option>)}
          </select></label>
        {(offset > 0 || nextOffset !== null) && <div className="flex gap-2">
          <Button size="sm" variant="outline" disabled={reading || offset === 0} onClick={() => loadConnections(Math.max(0, offset - 50))}>{t("knowledge.sources.bindingSetup.previous")}</Button>
          <Button size="sm" variant="outline" disabled={reading || nextOffset === null} onClick={() => nextOffset !== null && loadConnections(nextOffset)}>{t("knowledge.sources.bindingSetup.next")}</Button>
        </div>}
        <label className="block text-sm">{t("knowledge.sources.bindingSetup.spaceKey")}
          <input className="mt-1 w-full rounded border bg-background p-2" value={spaceKey} maxLength={255} disabled={submitting}
            onChange={event => { invalidateSelection(); setSpaceKey(event.target.value); }} />
        </label>
        <Button size="sm" variant="outline" disabled={reading || !connectionId || !/^[A-Za-z0-9_~-]{1,255}$/.test(spaceKey)} onClick={verifySpace}>{t("knowledge.sources.bindingSetup.verify")}</Button>
      </>}
      {space && <div role="status" className="space-y-2 rounded border p-2">
        <p>{space.space_name} ({space.space_key})</p><p>{space.root_page_title} ({space.root_page_id})</p><p className="text-xs">{t("knowledge.sources.bindingSetup.scope")}</p>
        <Button size="sm" disabled={reading || submitting || storageFailed} onClick={() => void save()}>{t("knowledge.sources.bindingSetup.save")}</Button>
      </div>}
    </div>}
    {reading && <p role="status">{t("common.loading")}</p>}
    {request && <div className="space-y-2">
      <p className="break-all text-xs">{t("knowledge.sources.bindingSetup.requestId", { id: request.request_id })}</p>
      <p role={receipt ? "status" : "alert"}>{t(submitting ? "knowledge.sources.bindingSetup.saving" : receipt ? "knowledge.sources.bindingSetup.saved" : "knowledge.sources.bindingSetup.uncertain")}</p>
      <Button size="sm" variant="outline" disabled={reading || submitting} onClick={recheck}>{t("knowledge.sources.bindingSetup.recheck")}</Button>
      {receipt && enabled && <Button size="sm" variant="outline" disabled={reading || submitting || storageFailed} onClick={another}>{t("knowledge.sources.bindingSetup.another")}</Button>}
    </div>}
  </div>;
}
