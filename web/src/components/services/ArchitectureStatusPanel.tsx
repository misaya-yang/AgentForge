import { Boxes, Clock3, LockKeyhole, RefreshCw } from "lucide-react";

import type { ArchitectureStatus } from "@/api/architecture";
import { Button } from "@/components/ui/button";
import {
  architectureReasonLabel,
  architectureStatusClass,
} from "./architectureStatusPresentation";

interface ArchitectureStatusPanelProps {
  data?: ArchitectureStatus;
  loading: boolean;
  error: boolean;
  allowed: boolean;
  onRefresh: () => void;
}

export function ArchitectureStatusPanel({
  data,
  loading,
  error,
  allowed,
  onRefresh,
}: ArchitectureStatusPanelProps) {
  if (!allowed) {
    return (
      <div className="flex items-center gap-2 rounded-lg border border-border/60 bg-muted/20 px-3 py-2 text-xs text-muted-foreground">
        <LockKeyhole className="h-3.5 w-3.5" />
        Platform architecture status is available to platform administrators.
      </div>
    );
  }
  if (loading) {
    return <div className="h-40 animate-pulse rounded-xl border bg-muted/20" />;
  }
  if (error || !data) {
    return (
      <div className="flex flex-col gap-3 rounded-xl border border-amber-500/30 bg-amber-500/5 p-4 sm:flex-row sm:items-center sm:justify-between">
        <p className="text-sm text-amber-800 dark:text-amber-200">
          Architecture status is temporarily unavailable. Existing service controls remain usable.
        </p>
        <Button variant="outline" size="sm" onClick={onRefresh}>
          <RefreshCw className="mr-2 h-3.5 w-3.5" /> Retry
        </Button>
      </div>
    );
  }

  return (
    <section aria-labelledby="architecture-status-title" className="space-y-3 rounded-xl border border-border/70 bg-card/50 p-3 sm:p-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h2 id="architecture-status-title" className="flex items-center gap-2 text-sm font-semibold">
            <Boxes className="h-4 w-4 text-primary" /> Platform architecture
          </h2>
          <p className="mt-1 text-xs text-muted-foreground">
            Mode {data.mode} · topology {data.topology_revision}
          </p>
          {!data.mode_configuration_valid ? (
            <p role="alert" className="mt-1 text-xs text-amber-700 dark:text-amber-300">
              Invalid mode configuration; safely using full mode.
            </p>
          ) : null}
        </div>
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <Clock3 className="h-3.5 w-3.5" />
          <span className="max-w-52 truncate">Checked {data.last_check}</span>
          <Button variant="ghost" size="icon" className="h-8 w-8" onClick={onRefresh} aria-label="Refresh architecture status">
            <RefreshCw className="h-3.5 w-3.5" />
          </Button>
        </div>
      </div>

      <div className="grid gap-3 xl:grid-cols-2">
        {data.groups.map((group) => (
          <div key={group.group_id} className="min-w-0 rounded-lg border border-border/60 bg-background/70 p-3">
            <h3 className="mb-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
              {group.display_name}
            </h3>
            <div className="space-y-2">
              {group.services.map((service) => (
                <article key={service.service_id} className="min-w-0 rounded-md border border-border/50 p-3">
                  <div className="flex flex-wrap items-start justify-between gap-2">
                    <div className="min-w-0">
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="text-sm font-medium">{service.display_name}</span>
                        <span className={`rounded-full px-2 py-0.5 text-[11px] font-medium ${architectureStatusClass(service.status)}`}>
                          {service.status}
                        </span>
                        {service.lifecycle === "one-shot" ? (
                          <span className="rounded-full bg-violet-500/10 px-2 py-0.5 text-[11px] text-violet-700 dark:text-violet-300">one-time job</span>
                        ) : null}
                      </div>
                      <p className="mt-1 text-xs text-muted-foreground">{service.responsibility}</p>
                    </div>
                    <div className="text-right text-[11px] text-muted-foreground">
                      <div>v{service.version}</div>
                      <div>Configured: {service.replicas} replica{service.replicas === 1 ? "" : "s"} · {service.scale_support}</div>
                    </div>
                  </div>
                  <div className="mt-2 flex flex-wrap gap-1.5">
                    {service.dependencies.map((dependency) => (
                      <span key={`${dependency.required}-${dependency.service_id}`} className="rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
                        {dependency.required ? "required" : "optional"}: {dependency.service_id} · {dependency.status}
                      </span>
                    ))}
                  </div>
                  {service.degraded_reasons.length > 0 ? (
                    <p className="mt-2 text-[11px] text-amber-700 dark:text-amber-300">
                      {service.degraded_reasons.map(architectureReasonLabel).join(" · ")}
                    </p>
                  ) : null}
                  <p className="mt-2 truncate text-[10px] text-muted-foreground">
                    Owner: {service.state_owner} · Last check: {service.last_check ?? "not applicable"}
                  </p>
                </article>
              ))}
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}
