import { Cloud, Unplug } from "lucide-react";

import { Badge } from "@/components/ui/badge";

import type { OfflineDegradation } from "./types";

export interface OfflineDegradationNoticeProps {
  degradation: OfflineDegradation;
}

export function OfflineDegradationNotice({ degradation }: OfflineDegradationNoticeProps) {
  return (
    <section
      className="rounded-lg border border-amber-500/25 bg-amber-500/10 p-4 text-amber-950 dark:text-amber-100"
      role="status"
      aria-live="polite"
      aria-labelledby="local-os-offline-heading"
    >
      <div className="flex items-start gap-3">
        <Unplug aria-hidden="true" className="mt-0.5 size-5 shrink-0" />
        <div className="min-w-0 flex-1">
          <h2 id="local-os-offline-heading" className="text-sm font-semibold">
            Local capabilities are unavailable
          </h2>
          <p className="mt-1 text-sm opacity-90">{degradation.reason}</p>

          {degradation.unavailableCapabilities.length > 0 ? (
            <div className="mt-3">
              <p className="text-xs font-medium uppercase tracking-wide opacity-75">Unavailable</p>
              <div className="mt-1.5 flex flex-wrap gap-1.5">
                {degradation.unavailableCapabilities.map((capability) => (
                  <Badge key={capability} variant="outline" className="border-amber-700/30">
                    {capability}
                  </Badge>
                ))}
              </div>
            </div>
          ) : null}

          {degradation.availableCloudCapabilities.length > 0 ? (
            <div className="mt-3 flex items-start gap-2 rounded-md bg-background/60 p-2.5 text-foreground">
              <Cloud aria-hidden="true" className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
              <p className="text-xs">
                Still available: {degradation.availableCloudCapabilities.join(", ")}.
              </p>
            </div>
          ) : null}
        </div>
      </div>
    </section>
  );
}

