import { Suspense, useEffect, useState, type ReactNode } from "react";
/** Load a tab on first intent, then retain its subtree and local draft state. */
export function DeferredTab({ active, children, fallback }: { active: boolean; children: ReactNode; fallback?: ReactNode }) {
  const [visited, setVisited] = useState(active);
  useEffect(() => { if (active) setVisited(true); }, [active]);
  return active || visited ? <Suspense fallback={fallback}>{children}</Suspense> : null;
}
