import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { copyToClipboard } from "@/lib/clipboard";
import { taskDiagnosticState, taskDiagnosticSummary, type DiagnosticReceipt, type ResultObservation } from "../taskDiagnostics";

interface Props {
  receipt: DiagnosticReceipt;
  result: ResultObservation;
  refreshing: boolean;
  refresh: () => void;
}

/** Receives only an exact-identity, authorized detail observation from its caller. */
export function TaskDiagnostics({ receipt, result, refreshing, refresh }: Props) {
  const { t } = useTranslation();
  const [preview, setPreview] = useState<string | null>(null);
  const [copyStatus, setCopyStatus] = useState<"idle" | "copying" | "copied" | "failed">("idle");
  const copying = useRef(false);
  const state = taskDiagnosticState(receipt.status);
  const copy = async () => {
    if (!preview || copying.current) return;
    copying.current = true;
    setCopyStatus("copying");
    try { await copyToClipboard(preview); setCopyStatus("copied"); }
    catch { setCopyStatus("failed"); }
    finally { copying.current = false; }
  };
  return <section className="space-y-3 rounded-xl border border-border bg-card p-4" aria-label={t("tasks.diagnostics.title")}>
    <h3 className="text-sm font-semibold">{t("tasks.diagnostics.title")}</h3>
    <p className="text-sm">{t(`tasks.diagnostics.states.${state}`)}</p>
    {state === "completed" && result === "unavailable" && <p role="status" className="text-sm">{t("tasks.diagnostics.resultUnavailable")}</p>}
    <p className="text-xs text-muted-foreground">{t("tasks.diagnostics.scope")}</p>
    <div className="flex flex-wrap gap-2">
      <Button size="sm" variant="outline" disabled={refreshing} onClick={refresh}>{t("tasks.diagnostics.refresh")}</Button>
      <Button size="sm" variant="outline" disabled={refreshing || copyStatus === "copying"} onClick={() => {
        setPreview(taskDiagnosticSummary(receipt, result, new Date().toISOString()));
        setCopyStatus("idle");
      }}>{t("tasks.diagnostics.preview")}</Button>
    </div>
    {preview && <div className="space-y-3">
      <p className="text-xs text-muted-foreground">{t("tasks.diagnostics.previewNotice")}</p>
      <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-all rounded border bg-muted/30 p-3 text-xs">{preview}</pre>
      <Button size="sm" variant="outline" disabled={copyStatus === "copying"} onClick={() => void copy()}>{t("tasks.diagnostics.copy")}</Button>
      {copyStatus !== "idle" && <p role={copyStatus === "failed" ? "alert" : "status"} className="text-xs">{t(`tasks.diagnostics.${copyStatus}`)}</p>}
    </div>}
  </section>;
}
