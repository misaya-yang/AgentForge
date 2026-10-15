export interface ActivityBatch {
  eventCount: number;
  thinkingStart: boolean;
  thinkingDelta: string;
  thinkingEnd: string | null;
  subagentEvents: Array<{ eventType: string; data: unknown; now: number }>;
}

export type ActivityEvent =
  | { kind: "thinking_start" }
  | { kind: "thinking_delta"; text: string }
  | { kind: "thinking_end"; text: string }
  | { kind: "subagent"; eventType: string; data: unknown; now: number };

export const activityFlushMetrics = {
  events: 0,
  flushes: 0,
};

export function resetActivityFlushMetrics(): void {
  activityFlushMetrics.events = 0;
  activityFlushMetrics.flushes = 0;
}

export function emptyActivityBatch(): ActivityBatch {
  return {
    eventCount: 0,
    thinkingStart: false,
    thinkingDelta: "",
    thinkingEnd: null,
    subagentEvents: [],
  };
}

export function foldActivityEvent(batch: ActivityBatch, event: ActivityEvent): ActivityBatch {
  const next: ActivityBatch = {
    ...batch,
    eventCount: batch.eventCount + 1,
    subagentEvents: batch.subagentEvents.slice(),
  };
  if (event.kind === "thinking_start") {
    next.thinkingStart = true;
    return next;
  }
  if (event.kind === "thinking_delta") {
    next.thinkingDelta += event.text;
    return next;
  }
  if (event.kind === "thinking_end") {
    next.thinkingEnd = event.text;
    return next;
  }
  next.subagentEvents.push({
    eventType: event.eventType,
    data: event.data,
    now: event.now,
  });
  return next;
}

export function createActivityFlushQueue(options: {
  scheduleFlush: (flush: () => void) => void;
  apply: (batch: ActivityBatch) => void;
}): {
  enqueue: (event: ActivityEvent) => void;
  flushNow: () => void;
} {
  let batch = emptyActivityBatch();
  let scheduled = false;

  const flush = () => {
    scheduled = false;
    if (batch.eventCount === 0) {
      return;
    }
    const ready = batch;
    batch = emptyActivityBatch();
    activityFlushMetrics.flushes += 1;
    options.apply(ready);
  };

  return {
    enqueue(event: ActivityEvent) {
      activityFlushMetrics.events += 1;
      batch = foldActivityEvent(batch, event);
      if (!scheduled) {
        scheduled = true;
        options.scheduleFlush(flush);
      }
    },
    flushNow: flush,
  };
}
