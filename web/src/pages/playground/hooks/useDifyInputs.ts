import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { useAppStore } from "@/store/useAppStore";
import { useAuthStore } from "@/store/useAuthStore";

/** Parse only an object, preserving JSON value types and never merging field names. */
export function parseDifyInputs(text: string): Record<string, unknown> | undefined {
  if (!text.trim()) return undefined;
  const value: unknown = JSON.parse(text, (_key, item: unknown) => {
    if (typeof item === "number" && !Number.isFinite(item)) throw new Error("Invalid Dify inputs");
    return item;
  });
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid Dify inputs");
  return value as Record<string, unknown>;
}

/** Drafts never enter browser storage, session metadata, history or diagnostics. */
export function useDifyInputs(serviceId: string | undefined, sessionId: string | undefined, enabled: boolean, sessionEnabled: boolean) {
  const token = useAuthStore(state => state.token);
  const authority = useAuthStore(state => sdkAuthorityScope(state.user));
  const scope = useMemo(() => ({ token, authority, serviceId, sessionId, enabled, sessionEnabled }),
    [token, authority, serviceId, sessionId, enabled, sessionEnabled]);
  const mounted = useRef(true);
  useLayoutEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const pending = useRef<object | null>(null);
  const current = useRef(scope);
  current.current = scope;
  const [draft, setDraft] = useState({ scope, text: "" });
  // Reset during render, not after paint: a new view cannot display or send an old draft.
  if (draft.scope !== scope) setDraft({ scope, text: "" });
  const text = draft.scope === scope && enabled ? draft.text : "";
  let value: Record<string, unknown> | undefined;
  let invalid = false;
  try { value = parseDifyInputs(text); } catch { invalid = true; }
  const isCurrent = () => {
    const auth = useAuthStore.getState();
    const selection = useAppStore.getState();
    return mounted.current && current.current === scope && auth.token === token && sdkAuthorityScope(auth.user) === authority &&
      selection.selectedServiceId === serviceId && selection.activeSessionId === sessionId;
  };
  const beginSend = () => {
    if (!enabled || invalid || !isCurrent() || pending.current === scope) return undefined;
    pending.current = scope;
    return () => { if (pending.current === scope) pending.current = null; };
  };
  return { text, value, invalid, isCurrent, beginSend, setText: (next: string) => {
    if (isCurrent() && enabled && pending.current !== scope) setDraft({ scope, text: next });
  } };
}
