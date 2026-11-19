import {
  AlertCircle,
  AppWindow,
  CheckCircle2,
  CircleEllipsis,
  FileText,
  Hand,
  Loader2,
  MonitorCog,
  ShieldCheck,
  TerminalSquare,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";
import { cn } from "@/lib/utils";

import { actionClasses, actionLabels, driverLabels, formatTimestamp } from "./presentation";
import type { LocalActionEvent, LocalActionKind } from "./types";

export interface LocalActionTimelineProps {
  actions: LocalActionEvent[];
  maxHeightClassName?: string;
  onVerifyUnknownAction?: (actionId: string) => void;
}

function ActionKindIcon({ kind }: { kind: LocalActionKind }) {
  const className = "size-4";
  switch (kind) {
    case "file":
      return <FileText aria-hidden="true" className={className} />;
    case "process":
      return <TerminalSquare aria-hidden="true" className={className} />;
    case "browser":
      return <MonitorCog aria-hidden="true" className={className} />;
    case "desktop":
      return <AppWindow aria-hidden="true" className={className} />;
    case "approval":
      return <ShieldCheck aria-hidden="true" className={className} />;
    case "takeover":
      return <Hand aria-hidden="true" className={className} />;
    default:
      return <CircleEllipsis aria-hidden="true" className={className} />;
  }
}

function ActionStateIcon({ action }: { action: LocalActionEvent }) {
  if (action.status === "succeeded") {
    return <CheckCircle2 aria-hidden="true" className="size-4 text-emerald-600" />;
  }
  if (action.status === "failed" || action.status === "unknown") {
    return <AlertCircle aria-hidden="true" className="size-4 text-destructive" />;
  }
  if (action.status === "running" || action.status === "dispatched") {
    return <Loader2 aria-hidden="true" className="size-4 animate-spin text-primary" />;
  }
  return <span aria-hidden="true" className="size-2 rounded-full bg-muted-foreground/50" />;
}

export function LocalActionTimeline({
  actions,
  maxHeightClassName = "max-h-[34rem]",
  onVerifyUnknownAction,
}: LocalActionTimelineProps) {
  const orderedActions = [...actions].sort((left, right) => left.sequence - right.sequence);

  return (
    <Card aria-labelledby="local-action-timeline-heading">
      <CardHeader className="flex-row items-start justify-between gap-3 space-y-0 p-4">
        <div>
          <h2 id="local-action-timeline-heading" className="text-sm font-semibold">
            Local action timeline
          </h2>
          <p className="mt-1 text-xs text-muted-foreground">
            Proposed actions, policy decisions, device dispatch, observations, and terminal receipts.
          </p>
        </div>
        <Badge variant="outline">{actions.length} events</Badge>
      </CardHeader>
      <CardContent className="p-0">
        {orderedActions.length === 0 ? (
          <p className="px-4 pb-4 text-sm text-muted-foreground">No local actions recorded.</p>
        ) : (
          <ScrollArea className={cn("px-4 pb-4", maxHeightClassName)}>
            <ol className="relative ml-2 border-l border-border" aria-live="polite">
              {orderedActions.map((action) => (
                <li key={action.id} className="relative pb-5 pl-6 last:pb-0">
                  <span className="absolute -left-[9px] top-1 flex size-4 items-center justify-center rounded-full bg-background">
                    <ActionStateIcon action={action} />
                  </span>
                  <div className="flex flex-wrap items-start justify-between gap-2">
                    <div className="flex min-w-0 items-start gap-2">
                      <span className="mt-0.5 text-muted-foreground">
                        <ActionKindIcon kind={action.kind} />
                      </span>
                      <div className="min-w-0">
                        <p className="text-sm font-medium">{action.title}</p>
                        {action.target ? (
                          <p className="mt-0.5 truncate font-mono text-xs text-muted-foreground">
                            {action.target}
                          </p>
                        ) : null}
                      </div>
                    </div>
                    <Badge variant="outline" className={cn(actionClasses[action.status])}>
                      {actionLabels[action.status]}
                    </Badge>
                  </div>
                  {action.detail ? (
                    <p className="mt-2 text-xs leading-relaxed text-muted-foreground">
                      {action.detail}
                    </p>
                  ) : null}
                  <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-muted-foreground">
                    <span>#{action.sequence}</span>
                    <time dateTime={action.timestamp}>{formatTimestamp(action.timestamp)}</time>
                    {action.driver ? <span>{driverLabels[action.driver]}</span> : null}
                    {action.errorCode ? <code>{action.errorCode}</code> : null}
                    {action.artifactRefs?.length ? (
                      <span>{action.artifactRefs.length} artifact(s)</span>
                    ) : null}
                  </div>
                  {action.status === "unknown" && onVerifyUnknownAction ? (
                    <Button
                      size="sm"
                      variant="outline"
                      className="mt-3"
                      onClick={() => onVerifyUnknownAction(action.id)}
                    >
                      Verify device state
                    </Button>
                  ) : null}
                </li>
              ))}
            </ol>
          </ScrollArea>
        )}
      </CardContent>
    </Card>
  );
}

