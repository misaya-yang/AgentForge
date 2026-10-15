export type ArchitectureTone = "healthy" | "degraded" | "inactive" | "unknown";

export function canViewArchitectureStatus(
  roles: readonly string[],
  permissions: readonly string[],
): boolean {
  const subjects = new Set(
    [...roles, ...permissions].map((value) => value.trim().toLowerCase()),
  );
  return ["platform_admin", "superadmin", "super_admin"].some((role) => subjects.has(role));
}

const REASON_LABELS: Record<string, string> = {
  health_contract_degraded: "Health check unavailable",
  optional_dependency_degraded: "Optional dependency degraded",
  required_dependency_unavailable: "Required dependency unavailable",
};

export function architectureReasonLabel(reason: string): string {
  return REASON_LABELS[reason] ?? "Status unavailable";
}

export function architectureStatusTone(status: string): ArchitectureTone {
  if (["healthy", "ready"].includes(status)) return "healthy";
  if (["degraded", "unavailable", "not_ready"].includes(status)) return "degraded";
  if (["inactive", "integrated", "not-applicable", "one-shot"].includes(status)) {
    return "inactive";
  }
  return "unknown";
}

export function architectureStatusClass(status: string): string {
  const tone = architectureStatusTone(status);
  if (tone === "healthy") return "bg-emerald-500/10 text-emerald-700 dark:text-emerald-300";
  if (tone === "degraded") return "bg-amber-500/10 text-amber-700 dark:text-amber-300";
  if (tone === "inactive") return "bg-slate-500/10 text-slate-600 dark:text-slate-300";
  return "bg-muted text-muted-foreground";
}
