import type {
  ApprovalRisk,
  ComputerUseDriver,
  ComputerUseSessionStatus,
  LocalActionStatus,
  LocalNodeConnectionStatus,
  PermissionHealthStatus,
} from "./types";

export const connectionLabels: Record<LocalNodeConnectionStatus, string> = {
  online: "Online",
  offline: "Offline",
  stale: "Connection stale",
  pairing: "Pairing",
  revoked: "Revoked",
  incompatible: "Update required",
};

export const connectionClasses: Record<LocalNodeConnectionStatus, string> = {
  online: "border-emerald-500/25 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300",
  offline: "border-border bg-muted text-muted-foreground",
  stale: "border-amber-500/25 bg-amber-500/10 text-amber-700 dark:text-amber-300",
  pairing: "border-sky-500/25 bg-sky-500/10 text-sky-700 dark:text-sky-300",
  revoked: "border-destructive/25 bg-destructive/10 text-destructive",
  incompatible: "border-amber-500/25 bg-amber-500/10 text-amber-700 dark:text-amber-300",
};

export const permissionLabels: Record<PermissionHealthStatus, string> = {
  ready: "Ready",
  denied: "Denied",
  needs_action: "Action required",
  unsupported: "Unsupported",
  unknown: "Unknown",
};

export const permissionClasses: Record<PermissionHealthStatus, string> = {
  ready: "border-emerald-500/25 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300",
  denied: "border-destructive/25 bg-destructive/10 text-destructive",
  needs_action: "border-amber-500/25 bg-amber-500/10 text-amber-700 dark:text-amber-300",
  unsupported: "border-border bg-muted text-muted-foreground",
  unknown: "border-border bg-muted text-muted-foreground",
};

export const actionLabels: Record<LocalActionStatus, string> = {
  proposed: "Proposed",
  policy_check: "Policy check",
  awaiting_approval: "Awaiting approval",
  dispatched: "Dispatched",
  running: "Running",
  observed: "Observed",
  succeeded: "Succeeded",
  failed: "Failed",
  cancelled: "Cancelled",
  interrupted: "Interrupted",
  unknown: "Unknown",
};

export const actionClasses: Record<LocalActionStatus, string> = {
  proposed: "border-border bg-muted text-muted-foreground",
  policy_check: "border-sky-500/25 bg-sky-500/10 text-sky-700 dark:text-sky-300",
  awaiting_approval: "border-amber-500/25 bg-amber-500/10 text-amber-700 dark:text-amber-300",
  dispatched: "border-sky-500/25 bg-sky-500/10 text-sky-700 dark:text-sky-300",
  running: "border-primary/25 bg-primary/10 text-primary",
  observed: "border-violet-500/25 bg-violet-500/10 text-violet-700 dark:text-violet-300",
  succeeded: "border-emerald-500/25 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300",
  failed: "border-destructive/25 bg-destructive/10 text-destructive",
  cancelled: "border-border bg-muted text-muted-foreground",
  interrupted: "border-orange-500/25 bg-orange-500/10 text-orange-700 dark:text-orange-300",
  unknown: "border-destructive/40 bg-destructive/10 text-destructive",
};

export const sessionLabels: Record<ComputerUseSessionStatus, string> = {
  idle: "Idle",
  observing: "Observing",
  awaiting_approval: "Awaiting approval",
  controlling: "Controlling",
  paused: "Paused",
  stopping: "Stopping",
  stopped: "Stopped",
  unavailable: "Unavailable",
};

export const driverLabels: Record<ComputerUseDriver, string> = {
  connector: "Structured connector",
  dom: "DOM",
  accessibility: "Accessibility",
  computer_vision: "Computer vision",
  unknown: "Unknown driver",
};

export const riskLabels: Record<ApprovalRisk, string> = {
  medium: "Medium risk",
  high: "High risk",
  critical: "Critical risk",
};

export function formatTimestamp(value?: string): string {
  if (!value) return "Not reported";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
}

export function shortDigest(value?: string): string {
  if (!value) return "—";
  if (value.length <= 18) return value;
  return `${value.slice(0, 10)}…${value.slice(-6)}`;
}
