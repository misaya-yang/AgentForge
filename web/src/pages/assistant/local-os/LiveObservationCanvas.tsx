import {
  Eye,
  Hand,
  ImageOff,
  MonitorUp,
  Octagon,
  ShieldCheck,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader } from "@/components/ui/card";

import { driverLabels, formatTimestamp, sessionLabels } from "./presentation";
import type { LocalOSObservation } from "./types";

export interface LiveObservationCanvasProps {
  observation?: LocalOSObservation;
  canTakeOver?: boolean;
  canStop?: boolean;
  onRequestTakeover?: () => void;
  onStop?: () => void;
}

const activeStatuses = new Set([
  "observing",
  "awaiting_approval",
  "controlling",
  "paused",
  "stopping",
]);

export function LiveObservationCanvas({
  observation,
  canTakeOver = false,
  canStop = false,
  onRequestTakeover,
  onStop,
}: LiveObservationCanvasProps) {
  const active = observation ? activeStatuses.has(observation.sessionStatus) : false;
  const sessionLabel = observation ? sessionLabels[observation.sessionStatus] : "No session";

  return (
    <Card aria-labelledby="local-os-observation-heading" className="overflow-hidden">
      <CardHeader className="flex-row items-start justify-between gap-4 space-y-0 p-4">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <MonitorUp aria-hidden="true" className="size-4 text-muted-foreground" />
            <h2 id="local-os-observation-heading" className="text-sm font-semibold">
              Live observation
            </h2>
          </div>
          <p className="mt-1 text-xs text-muted-foreground">
            Latest verified view from the selected device. This page does not control the OS directly.
          </p>
        </div>
        <Badge variant="outline" aria-live="polite">
          {sessionLabel}
        </Badge>
      </CardHeader>

      <CardContent className="space-y-3 p-4 pt-0">
        <figure className="overflow-hidden rounded-md border border-border bg-zinc-950">
          <div className="relative aspect-video min-h-52 w-full">
            {observation?.screenshotUrl ? (
              <img
                src={observation.screenshotUrl}
                alt={
                  observation.screenshotAlt ??
                  `Latest observation from ${observation.deviceLabel}`
                }
                className="h-full w-full object-contain"
              />
            ) : (
              <div className="flex h-full min-h-52 flex-col items-center justify-center gap-2 px-6 text-center text-zinc-400">
                <ImageOff aria-hidden="true" className="size-7" />
                <p className="text-sm font-medium text-zinc-300">No observation available</p>
                <p className="max-w-md text-xs">
                  A real screenshot or accessibility observation appears only after an authorized Local Node session starts.
                </p>
              </div>
            )}
            {observation ? (
              <div className="absolute inset-x-0 bottom-0 flex flex-wrap items-center justify-between gap-2 bg-zinc-950/85 px-3 py-2 text-xs text-zinc-200 backdrop-blur-sm">
                <span className="truncate">
                  {observation.appName ?? "Unknown app"}
                  {observation.windowTitle ? ` · ${observation.windowTitle}` : ""}
                </span>
                <span>Observation #{observation.sequence}</span>
              </div>
            ) : null}
          </div>
          {observation ? (
            <figcaption className="sr-only">
              Captured {formatTimestamp(observation.capturedAt)} using {driverLabels[observation.driver]}.
            </figcaption>
          ) : null}
        </figure>

        {observation ? (
          <dl className="grid gap-3 rounded-md border border-border/70 bg-muted/25 p-3 text-xs sm:grid-cols-2 lg:grid-cols-4">
            <div>
              <dt className="text-muted-foreground">Device</dt>
              <dd className="mt-0.5 font-medium">{observation.deviceLabel}</dd>
            </div>
            <div>
              <dt className="text-muted-foreground">Execution driver</dt>
              <dd className="mt-0.5 font-medium">{driverLabels[observation.driver]}</dd>
            </div>
            <div>
              <dt className="text-muted-foreground">Origin</dt>
              <dd className="mt-0.5 truncate font-medium">
                {observation.origin ?? "Desktop application"}
              </dd>
            </div>
            <div>
              <dt className="text-muted-foreground">Captured</dt>
              <dd className="mt-0.5 font-medium">{formatTimestamp(observation.capturedAt)}</dd>
            </div>
          </dl>
        ) : null}

        {observation?.maskingApplied ? (
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            <ShieldCheck aria-hidden="true" className="size-4 text-emerald-600" />
            Sensitive-region masking was applied before this observation was shared.
          </div>
        ) : observation ? (
          <div className="flex items-center gap-2 text-xs text-amber-700 dark:text-amber-300">
            <Eye aria-hidden="true" className="size-4" />
            No masking receipt was reported for this observation.
          </div>
        ) : null}

        <div className="flex flex-wrap justify-end gap-2">
          {onRequestTakeover ? (
            <Button
              variant="outline"
              disabled={!active || !canTakeOver}
              onClick={onRequestTakeover}
            >
              <Hand aria-hidden="true" className="mr-1.5 size-4" />
              Take over locally
            </Button>
          ) : null}
          {onStop ? (
            <Button
              variant="destructive"
              disabled={!active || !canStop || observation?.sessionStatus === "stopping"}
              onClick={onStop}
            >
              <Octagon aria-hidden="true" className="mr-1.5 size-4" />
              Stop control
            </Button>
          ) : null}
        </div>
      </CardContent>
    </Card>
  );
}

