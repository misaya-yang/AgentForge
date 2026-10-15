/** Only schedules dashboard refresh requests. It never cancels in-flight reads. */
export function createDashboardRefreshScheduler(options: {
  intervalMs: number;
  now: () => number;
  visible: () => boolean;
  schedule: (callback: () => void, delayMs: number) => () => void;
  onRequest: () => void;
  onVisibility: (visible: boolean) => void;
}) {
  let disposed = false;
  let generation = 0;
  let cancelTimer: (() => void) | undefined;
  let visible = options.visible();
  let deadline = options.now() + options.intervalMs;
  const disarm = () => { generation += 1; cancelTimer?.(); cancelTimer = undefined; };
  const request = () => { options.onRequest(); deadline = options.now() + options.intervalMs; };
  const arm = () => {
    disarm();
    if (disposed || !visible || options.intervalMs <= 0) return;
    const owner = generation;
    cancelTimer = options.schedule(() => {
      if (disposed || owner !== generation) return;
      visible = options.visible(); options.onVisibility(visible);
      if (visible && options.now() >= deadline) request();
      arm();
    }, Math.max(1, deadline - options.now()));
  };
  options.onVisibility(visible);
  arm();
  return {
    manual() {
      if (disposed) return;
      visible = options.visible(); options.onVisibility(visible);
      request(); arm();
    },
    visibilityChanged() {
      if (disposed) return;
      const next = options.visible();
      if (next === visible) return;
      visible = next; options.onVisibility(visible);
      // One catch-up for an overdue hidden period, never one per missed tick.
      if (visible && options.intervalMs > 0 && options.now() >= deadline) request();
      arm();
    },
    dispose() { disposed = true; disarm(); },
  };
}
