import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { listAgentTools } from "@/api/agents";
import { capabilityDisplayName } from "@/pages/agents/agentCatalogPresentation";
import { Button } from "@/components/ui/button";
import {
  MAX_SDK_CAPABILITIES,
  visibleSdkCapabilities,
  type SdkCapabilityOption,
} from "@/lib/sdkCapabilitySelection";

export function SdkCapabilityPicker({ disabled, canView, selected, onChange, onCatalogChange }: {
  disabled: boolean;
  canView: boolean;
  selected: string[];
  onChange: (ids: string[]) => void;
  onCatalogChange: (rows: SdkCapabilityOption[]) => void;
}) {
  const { t } = useTranslation();
  const [rows, setRows] = useState<SdkCapabilityOption[]>([]);
  const [status, setStatus] = useState<"idle" | "loading" | "ready" | "failed">("idle");
  const generation = useRef(0);
  const pending = useRef(false);
  useEffect(() => () => { generation.current += 1; }, []);

  async function load() {
    if (disabled || !canView || pending.current) return;
    pending.current = true;
    const request = ++generation.current;
    setStatus("loading");
    // No stale selection survives a catalog refresh or an uncertain read.
    onChange([]); onCatalogChange([]); setRows([]);
    try {
      const visible = visibleSdkCapabilities(await listAgentTools());
      if (request !== generation.current) return;
      setRows(visible); onCatalogChange(visible); setStatus("ready");
    } catch {
      if (request === generation.current) setStatus("failed");
    } finally {
      if (request === generation.current) pending.current = false;
    }
  }

  return (
    <fieldset className="space-y-3 rounded-lg border p-3" disabled={disabled}>
      <legend className="px-1 text-sm font-medium">{t("services.sdkAgent.capabilities.title")}</legend>
      <p className="text-xs text-muted-foreground">{t("services.sdkAgent.capabilities.hint", { count: MAX_SDK_CAPABILITIES })}</p>
      {!canView ? <p className="text-xs text-muted-foreground">{t("services.sdkAgent.capabilities.permission")}</p> : <>
        <Button type="button" variant="outline" size="sm" onClick={() => void load()} disabled={disabled || status === "loading"}>
          {t(status === "loading" ? "services.sdkAgent.capabilities.loading" : status === "idle" ? "services.sdkAgent.capabilities.load" : "services.sdkAgent.capabilities.refresh")}
        </Button>
        {status === "failed" && <p role="alert" className="text-xs text-destructive">{t("services.sdkAgent.capabilities.failed")}</p>}
        {status === "ready" && rows.length === 0 && <p role="status" className="text-xs text-muted-foreground">{t("services.sdkAgent.capabilities.empty")}</p>}
        {rows.length > 0 && <div className="max-h-44 space-y-2 overflow-y-auto">
          {rows.map((row) => <label key={row.id} className="flex items-start gap-2 rounded p-1 text-sm">
            <input type="checkbox" checked={selected.includes(row.id)} className="mt-1 accent-primary"
              disabled={disabled || (!selected.includes(row.id) && selected.length >= MAX_SDK_CAPABILITIES)}
              onChange={(event) => onChange(event.target.checked ? [...selected, row.id] : selected.filter((id) => id !== row.id))} />
            <span className="min-w-0 break-words">{capabilityDisplayName(row.name, row.title)}<span className="block text-xs text-muted-foreground">{row.summary || row.description}</span><span className="block text-xs text-muted-foreground">{row.id} · {row.risk}</span></span>
          </label>)}
        </div>}
        <p role="status" className="text-xs text-muted-foreground">{t("services.sdkAgent.capabilities.selected", { count: selected.length })}</p>
        {selected.length > 0 && <Button type="button" variant="outline" size="sm" onClick={() => onChange([])}>{t("services.sdkAgent.capabilities.clear")}</Button>}
      </>}
    </fieldset>
  );
}
