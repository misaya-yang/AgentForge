import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { getAssistantSessionTools, type AssistantSessionTools } from "@/api/assistantTools";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { generateUUID } from "@/lib/utils";

interface ToolsDialogProps { sessionId: string; open: boolean; onClose: () => void }

export function ToolsDialog({ sessionId, open, onClose }: ToolsDialogProps) {
  const token = useAuthStore(state => state.token);
  const authority = useAuthStore(state => sdkAuthorityScope(state.user));
  const authenticated = useAuthStore(state => state.isAuthenticated);
  const [transition, setTransition] = useState(0);
  const scope = useMemo(() => ({ id: generateUUID(), token, authority, transition, invalidated: false }), [token, authority, transition]);
  useLayoutEffect(() => useAuthStore.subscribe((state, previous) => {
    if (state.token !== previous.token || state.isAuthenticated !== previous.isAuthenticated
      || sdkAuthorityScope(state.user) !== sdkAuthorityScope(previous.user)) {
      scope.invalidated = true;
      setTransition(value => value + 1);
    }
  }), [scope]);
  const isCurrent = useCallback(() => {
    const state = useAuthStore.getState();
    return Boolean(!scope.invalidated && state.isAuthenticated && state.token && state.user
      && state.token === scope.token && sdkAuthorityScope(state.user) === scope.authority);
  }, [scope]);
  if (!open || !authenticated || !token) return null;
  return <ToolInspector key={`${scope.id}:${sessionId}`} sessionId={sessionId} onClose={onClose} isCurrent={isCurrent} />;
}

function ToolInspector({ sessionId, onClose, isCurrent }: { sessionId: string; onClose: () => void; isCurrent: () => boolean }) {
  const { t } = useTranslation();
  const [data, setData] = useState<AssistantSessionTools | null>(null);
  const [failed, setFailed] = useState(false);
  const [loading, setLoading] = useState(true);
  const [revision, setRevision] = useState(0);
  const [filter, setFilter] = useState("");
  const [set, selectSet] = useState<"catalog" | "estimate" | "pinned">("catalog");
  const lifetime = useRef({ mounted: false, request: 0 });
  useLayoutEffect(() => {
    const owner = lifetime.current;
    owner.mounted = true;
    return () => { owner.mounted = false; owner.request += 1; };
  }, []);
  useEffect(() => {
    const owner = lifetime.current;
    const request = ++owner.request;
    const controller = new AbortController();
    const current = () => owner.mounted && owner.request === request && isCurrent();
    if (!current()) return;
    setData(null); setFailed(false); setLoading(true);
    void getAssistantSessionTools(sessionId, controller.signal).then(value => {
      if (current()) { setData(value); setLoading(false); }
    }).catch(() => { if (current()) { setFailed(true); setLoading(false); } });
    return () => { owner.request += 1; controller.abort(); };
  }, [sessionId, revision, isCurrent]);
  const active = () => lifetime.current.mounted && isCurrent();
  const close = () => {
    if (!active()) return;
    lifetime.current.mounted = false; lifetime.current.request += 1;
    onClose();
  };
  const tools = !data ? [] : set === "catalog" ? data.platform_catalog
    : set === "estimate" ? data.next_turn_estimate.tools : data.last_run_pinned?.tools ?? [];
  const visible = tools.filter(tool => `${tool.name} ${tool.description ?? ""}`.toLowerCase().includes(filter.trim().toLowerCase()));
  const unknown = t("assistant.toolsMetadataUnknown", "Not reported");
  return (
    <Dialog open onOpenChange={value => { if (!value) close(); }}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t("assistant.toolsTitle", "Assistant tools")}</DialogTitle>
          <DialogDescription>{t("assistant.toolsScope", "The platform catalog, your tenant access, and this conversation's resolved tools are different sets. A new run pins its own set when it starts.")}</DialogDescription>
        </DialogHeader>
        <Button variant="outline" disabled={loading} onClick={() => { if (active()) { lifetime.current.request += 1; setData(null); setFailed(false); setLoading(true); setRevision(value => value + 1); } }}>
          {failed ? t("common.retry", "Retry") : t("common.refresh", "Refresh")}
        </Button>
        {loading && <p role="status">{t("common.loading", "Loading…")}</p>}
        {failed && <p role="alert" className="text-sm text-red-600">{t("assistant.toolsUnavailable", "Tool status is unavailable. Check the connection or worker configuration.")}</p>}
        {data && isCurrent() && <div className="space-y-3">
          <div role="group" aria-label={t("assistant.toolsSetLabel", "Tool set")} className="flex flex-wrap gap-2">
            {(["catalog", "estimate", "pinned"] as const).map(value => <Button key={value} variant={set === value ? "default" : "outline"} aria-pressed={set === value}
              onClick={() => { if (active()) { selectSet(value); setFilter(""); } }}>
              {value === "catalog" ? t("assistant.toolsCatalogSet", "Platform catalog") : value === "estimate" ? t("assistant.toolsEstimateSet", "Next-run estimate") : t("assistant.toolsPinnedSet", "Last pinned run")}
            </Button>)}
          </div>
          <p className="text-xs text-muted-foreground">{set === "catalog" ? t("assistant.toolsCatalogNote", "Catalog membership does not confirm that a tool is callable in this session.")
            : set === "estimate" ? data.next_turn_estimate.status === "unavailable" ? t("assistant.toolsEstimateUnavailable", "Current worker/configuration could not be checked; catalog entries are not confirmed callable.") : t("assistant.toolsEstimate", "Callable estimate if a turn started now; checked again at admission.")
              : data.last_run_pinned ? t("assistant.toolsPinned", "Last run {{run}} ({{status}}) pinned {{count}} tools.", { run: data.last_run_pinned.run_id, status: data.last_run_pinned.status, count: data.last_run_pinned.tools.length }) : t("assistant.toolsNoPinned", "No pinned run was reported for this session.")}</p>
          <Input aria-label={t("assistant.toolsSearch", "Filter tool names and descriptions")} value={filter} onChange={event => { if (active()) setFilter(event.target.value); }} />
          <p role="status" className="text-xs text-muted-foreground">{t("assistant.toolsResultCount", "Showing {{shown}} of {{count}} returned tools", { shown: visible.length, count: tools.length })}</p>
          <div className="max-h-[45dvh] space-y-2 overflow-y-auto">
            {visible.length === 0 && <p className="text-sm">{t("assistant.toolsNoMatches", "No tools to display in this set with this filter.")}</p>}
            {visible.map((tool, index) => <div key={`${tool.name}:${index}`} className="rounded border p-3 text-sm">
              <strong className="break-all">{tool.name}</strong>
              {tool.description && <p className="mt-1 text-xs text-muted-foreground">{tool.description}</p>}
              <dl className="mt-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
                <dt>{t("assistant.toolsTenantPermission", "Tenant permission")}</dt><dd>{tool.tenant_visible === true ? t("assistant.toolsTenantAllowed", "Reported as allowed") : tool.tenant_visible === false ? t("assistant.toolNotAuthorized", "No tenant permission") : unknown}</dd>
                <dt>{t("assistant.toolsEffect", "Effect")}</dt><dd className="break-all">{tool.effect || unknown}</dd>
                <dt>{t("assistant.toolsApproval", "Approval policy")}</dt><dd className="break-all">{tool.approval || unknown}</dd>
              </dl>
              {tool.device_required && <p className="mt-1 text-xs text-amber-700 dark:text-amber-300">{t("assistant.toolNeedsDevice", "Choose an online device_id before use.")}</p>}
              {tool.required_inputs.length > 0 && <p className="mt-1 break-words text-xs text-muted-foreground">{t("assistant.toolRequiredInputs", "Required inputs")}: {tool.required_inputs.join(", ")}</p>}
            </div>)}
          </div>
        </div>}
      </DialogContent>
    </Dialog>
  );
}
