import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  deleteAssistantMemoryItem,
  getAssistantMemory,
  setAssistantMemoryEnabled,
  updateAssistantMemoryItem,
  type AssistantMemoryState,
} from "@/api/assistantMemory";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";

import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";

interface MemoryDialogProps {
  open: boolean;
  onClose: () => void;
}

export function MemoryDialog(props: MemoryDialogProps) {
  const scope = useAuthenticatedReadScope();
  if (!props.open || !scope.isCurrent()) return null;
  return <MemoryEditor key={scope.id} {...props} isCurrent={scope.isCurrent} />;
}

function MemoryEditor({ onClose, isCurrent }: MemoryDialogProps & { isCurrent: () => boolean }) {
  const { t } = useTranslation();
  const [state, setState] = useState<AssistantMemoryState | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  const [uncertain, setUncertain] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);
  const [editValue, setEditValue] = useState("");
  const [deleting, setDeleting] = useState<string | null>(null);
  const owner = useRef({ mounted: true, pending: false, blocked: false });
  useLayoutEffect(() => {
    const lifetime = owner.current;
    lifetime.mounted = true;
    return () => { lifetime.mounted = false; };
  }, []);
  const active = useCallback(() => owner.current.mounted && isCurrent(), [isCurrent]);
  const read = useCallback(async () => {
    if (!active() || owner.current.pending) return;
    owner.current.pending = true;
    setLoading(true); setError(""); setState(null);
    setEditing(null); setEditValue(""); setDeleting(null);
    try {
      const value = await getAssistantMemory();
      if (!active()) return;
      setState(value); setUncertain(false);
      owner.current.blocked = false;
    } catch {
      if (active()) setError(t("assistant.memoryLoadFailed", "Could not load memory settings."));
    } finally {
      owner.current.pending = false;
      if (active()) setLoading(false);
    }
  }, [active, t]);
  useEffect(() => { void read(); }, [read]);
  const mutate = async (action: () => Promise<unknown>) => {
    if (!active() || owner.current.pending || owner.current.blocked || !state) return;
    owner.current.pending = true;
    setBusy(true); setError(""); setUncertain(false);
    try {
      await action();
      if (!active()) return;
      const value = await getAssistantMemory();
      if (!active()) return;
      setState(value); setEditing(null); setEditValue(""); setDeleting(null);
    } catch {
      if (!active()) return;
      // A lost write/readback acknowledgement does not prove the change failed.
      owner.current.blocked = true;
      setState(null); setEditing(null); setEditValue(""); setDeleting(null);
      setUncertain(true);
    } finally {
      owner.current.pending = false;
      if (active()) setBusy(false);
    }
  };
  const toggle = (enabled: boolean) => mutate(() => setAssistantMemoryEnabled(enabled));
  const saveItem = (key: string) => mutate(() => updateAssistantMemoryItem(key, editValue));
  const removeItem = (key: string) => mutate(() => deleteAssistantMemoryItem(key));
  const close = () => {
    if (!active()) return;
    owner.current.mounted = false;
    onClose();
  };

  return (
    <Dialog open onOpenChange={(value) => { if (!value) close(); }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("assistant.memoryTitle", "Assistant memory")}</DialogTitle>
          <DialogDescription>
            {t("assistant.memoryEffect", "Changes affect new runs. Turning memory off also disables session memory for those runs; it does not erase existing records or revoke an active run.")}
          </DialogDescription>
        </DialogHeader>
        {error && <p role="alert" className="text-sm text-red-600">{error}</p>}
        {uncertain && <p role="alert" className="text-sm text-amber-700">{t("assistant.memoryChangeUnknown")}</p>}
        <button type="button" disabled={busy || loading} onClick={() => void read()} className="underline">{state ? t("common.refresh", "Refresh") : t("common.retry", "Retry")}</button>
        {loading ? (
          <p role="status" className="text-sm">{t("common.loading", "Loading…")}</p>
        ) : state && active() ? (
          <div className="space-y-4">
            <label className="flex items-center gap-3 text-sm">
              <input type="checkbox" checked={state.enabled} disabled={busy} onChange={(event) => void toggle(event.target.checked)} />
              {t("assistant.memoryEnabled", "Use memory on new runs")}
            </label>
            <p className="text-xs text-muted-foreground">{t("assistant.memoryScope", "These are your long-term records. Conversation history and session memory are separate. Deleting a record removes it from future retrieval, but cannot erase answers already produced.")}</p>
            <div className="max-h-[45dvh] space-y-3 overflow-y-auto">
              {state.items.length === 0 && <p className="text-sm text-muted-foreground">{t("assistant.memoryEmpty", "No long-term records saved.")}</p>}
              {state.items.map((item) => (
                <div key={item.key} className="rounded-lg border p-3 text-sm">
                  <div className="flex items-start justify-between gap-3">
                    <div className="min-w-0">
                      <p className="break-all font-medium">{item.key}</p>
                      <p className="text-xs text-muted-foreground">{item.source} · {item.updated_at ? new Date(item.updated_at).toLocaleString() : ""}</p>
                    </div>
                    <div className="flex shrink-0 gap-2">
                      <button type="button" disabled={busy} onClick={() => {
                        if (!active() || owner.current.pending || owner.current.blocked) return;
                        setEditing(item.key);
                        setEditValue(typeof item.value === "string" ? item.value : JSON.stringify(item.value));
                        setDeleting(null);
                      }} className="underline">{t("common.edit", "Edit")}</button>
                      <button type="button" disabled={busy} onClick={() => { if (active() && !owner.current.pending && !owner.current.blocked) setDeleting(item.key); }} className="text-red-600 underline">{t("common.delete", "Delete")}</button>
                    </div>
                  </div>
                  {editing === item.key ? (
                    <div className="mt-2 space-y-2">
                      <textarea aria-label={`${t("common.edit", "Edit")}: ${item.key}`} value={editValue} disabled={busy} onChange={(event) => { if (active() && !owner.current.pending) setEditValue(event.target.value); }} className="w-full rounded border bg-transparent p-2" rows={3} />
                      <button type="button" disabled={busy || !editValue.trim()} onClick={() => void saveItem(item.key)} className="rounded bg-primary px-3 py-1 text-white disabled:opacity-50">{t("common.save", "Save")}</button>
                    </div>
                  ) : <p className="mt-2 whitespace-pre-wrap break-words">{typeof item.value === "string" ? item.value : JSON.stringify(item.value)}</p>}
                  {deleting === item.key && (
                    <div role="alert" className="mt-2 flex items-center gap-3 text-xs">
                      <span>{t("assistant.memoryDeleteConfirm", "Remove this record from future use?")}</span>
                      <button type="button" disabled={busy} onClick={() => void removeItem(item.key)} className="text-red-600 underline">{t("assistant.memoryDelete", "Remove")}</button>
                      <button type="button" disabled={busy} onClick={() => { if (active() && !owner.current.pending) setDeleting(null); }} className="underline">{t("common.cancel", "Cancel")}</button>
                    </div>
                  )}
                </div>
              ))}
              {state.has_more && <p className="text-xs text-muted-foreground">{t("assistant.memoryMore", "More than 100 records exist; only the latest 100 are shown.")}</p>}
            </div>
          </div>
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
