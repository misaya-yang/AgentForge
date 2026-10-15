import { useEffect, useMemo, useState } from "react";
import { CheckCircle2, Loader2, MonitorCog, Unplug, X } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";

import { LocalOSControlSurface } from "./LocalOSControlSurface";
import type { LocalOSControlState } from "./useLocalOSControl";

export interface LocalOSPanelProps {
  open: boolean;
  onClose: () => void;
  state: LocalOSControlState;
  width?: number;
  className?: string;
}

function statusCopy(state: LocalOSControlState["loadState"]): string {
  if (state === "loading") return "Checking authorized folders";
  if (state === "online") return "Authorized folders available";
  return "Local files unavailable";
}

export function LocalOSPanel({
  open,
  onClose,
  state,
  width = 560,
  className,
}: LocalOSPanelProps) {
  const [pairingError, setPairingError] = useState<string>();
  const { actions: surfaceActions, pair, refresh } = state;

  useEffect(() => {
    if (open) void refresh();
  }, [open, refresh]);

  const actions = useMemo(
    () => ({
      ...surfaceActions,
      onPairDevice: async () => {
        setPairingError(undefined);
        try {
          await pair();
        } catch {
          setPairingError(
            "A pairing challenge could not be created. The Local Node control plane may be unavailable.",
          );
        }
      },
    }),
    [pair, surfaceActions],
  );

  return (
    <aside
      aria-hidden={!open}
      className={cn(
        "flex h-full shrink-0 flex-col overflow-hidden border-l border-[hsl(var(--assistant-border))] bg-background",
        className,
      )}
      style={{ width }}
    >
      <header className="flex shrink-0 items-center gap-3 border-b border-border px-4 py-3">
        <MonitorCog aria-hidden="true" className="size-4 text-muted-foreground" />
        <div className="min-w-0 flex-1">
          <p className="text-sm font-semibold">Local files</p>
          <p className="truncate text-xs text-muted-foreground">{statusCopy(state.loadState)}</p>
        </div>
        <Badge variant="outline" className="gap-1.5">
          {state.loadState === "loading" ? (
            <Loader2 aria-hidden="true" className="size-3 animate-spin" />
          ) : state.loadState === "online" ? (
            <CheckCircle2 aria-hidden="true" className="size-3 text-emerald-600" />
          ) : (
            <Unplug aria-hidden="true" className="size-3 text-amber-600" />
          )}
          {state.onlineDeviceCount} online
        </Badge>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="size-8"
          onClick={onClose}
          aria-label="Close Local files panel"
        >
          <X aria-hidden="true" className="size-4" />
        </Button>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto p-4">
        <section
          className="mb-4 rounded-lg border border-border bg-muted/20 p-3"
          aria-labelledby="local-os-session-opt-in-heading"
        >
          <div className="flex items-start justify-between gap-4">
            <div className="min-w-0">
              <h2 id="local-os-session-opt-in-heading" className="text-sm font-semibold">
                Enable local files for this chat
              </h2>
              <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                List, grep, read, hash, and watch tools are exposed only while the selected
                paired device and authorized folder grant remain ready. Reading a folder does
                not authorize sending its content anywhere else.
              </p>
              {!state.canEnableForSession && !state.sessionOptInRequested ? (
                <p className="mt-1 text-xs text-muted-foreground">
                  This switch unlocks only after the server reports an online device and a
                  usable, non-revoked grant.
                </p>
              ) : null}
            </div>
            <Switch
              checked={state.sessionOptInEffective}
              disabled={!state.sessionOptInRequested && !state.canEnableForSession}
              onCheckedChange={(enabled) => state.setSessionOptIn(enabled)}
              aria-label="Enable local file capabilities for this chat session"
            />
          </div>
          <div className="mt-3 flex items-start gap-2 text-xs text-muted-foreground" role="status">
            {state.sessionOptInEffective ? (
              <CheckCircle2 aria-hidden="true" className="mt-0.5 size-3.5 shrink-0 text-emerald-600" />
            ) : (
              <Unplug aria-hidden="true" className="mt-0.5 size-3.5 shrink-0 text-amber-600" />
            )}
            <span>{state.sessionOptInReason}</span>
          </div>
        </section>

        {state.pairingCode ? (
          <section
            className="mb-4 rounded-lg border border-sky-500/25 bg-sky-500/10 p-3"
            aria-live="polite"
          >
            <p className="text-sm font-medium">Pairing challenge created</p>
            <p className="mt-1 text-xs text-muted-foreground">
              Enter this correlation code locally in the trusted Local Node. The node must
              separately prove its device key over its authenticated outbound channel; this
              code is not a credential and cannot authorize an action by itself.
            </p>
            <div className="mt-2 flex flex-wrap items-center gap-2">
              <code className="rounded-md border border-sky-500/25 bg-background px-2.5 py-1.5 text-sm font-semibold tracking-wider">
                {state.pairingCode}
              </code>
              {state.pairingExpiresAt ? (
                <span className="text-xs text-muted-foreground">
                  Expires {new Date(state.pairingExpiresAt).toLocaleTimeString()}
                </span>
              ) : null}
            </div>
          </section>
        ) : null}

        {pairingError ? (
          <p
            className="mb-4 rounded-lg border border-destructive/25 bg-destructive/10 p-3 text-sm text-destructive"
            role="alert"
          >
            {pairingError}
          </p>
        ) : null}

        <LocalOSControlSurface model={state.model} actions={actions} />
      </div>
    </aside>
  );
}
