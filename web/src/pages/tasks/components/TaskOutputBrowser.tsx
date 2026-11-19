import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { copyToClipboard } from "@/lib/clipboard";
import { generateUUID } from "@/lib/utils";
import { OUTPUT_PAGE_SIZE, taskOutputItems, taskOutputPreview } from "../taskOutputPreview";

type Props = { taskId: string; requestId: string; result: unknown; scope: { id: string; isCurrent: () => boolean } };
export function TaskOutputBrowser({ taskId, requestId, result, scope }: Props) {
  const { t } = useTranslation();
  const observation = useMemo(() => ({ id: generateUUID(), active: true, taskId, requestId, result, scopeId: scope.id }), [taskId, requestId, result, scope.id]);
  const live = useRef(observation); live.current = observation;
  useLayoutEffect(() => { observation.active = true; return () => { observation.active = false; }; }, [observation]);
  const isCurrent = () => observation.active && live.current === observation && scope.isCurrent();
  const items = taskOutputItems(result, taskId, requestId);
  if (!isCurrent() || !items) return <p role="alert">{t("tasks.outputs.unavailable")}</p>;
  return <OutputItems key={observation.id} items={items} isCurrent={isCurrent} />;
}

function OutputItems({ items, isCurrent }: { items: unknown[]; isCurrent: () => boolean }) {
  const { t } = useTranslation();
  const [page, setPage] = useState(0);
  const [selected, setSelected] = useState<number | null>(null);
  const [raw, setRaw] = useState(false);
  const [copyStatus, setCopyStatus] = useState<"idle" | "copying" | "copied" | "failed">("idle");
  const control = useRef({ revision: 0, copying: false }); const revision = control.current.revision;
  const current = () => isCurrent() && control.current.revision === revision;
  const choose = (index: number | null, nextPage = page) => {
    if (!current()) return;
    control.current.revision++; control.current.copying = false;
    setSelected(index); setPage(nextPage); setRaw(false); setCopyStatus("idle");
  };
  const item = selected === null ? null : items[selected];
  const descriptor = item && typeof item === "object" ? item as Record<string, unknown> : null;
  const type = typeof descriptor?.type === "string" ? descriptor.type.slice(0, 80) : "unknown";
  const readable = type === "text" || type === "json";
  const preview = useMemo(() => selected === null ? null : taskOutputPreview(raw ? item : readable ? descriptor?.data : {
    type, mime_type: descriptor?.mime_type, url: descriptor?.url,
  }), [selected, raw, item, readable, descriptor, type]);
  const copy = async () => {
    if (!current() || !preview || control.current.copying) return;
    control.current.copying = true; setCopyStatus("copying");
    try { await copyToClipboard(preview.text); if (current()) setCopyStatus("copied"); }
    catch { if (current()) setCopyStatus("failed"); }
    finally { if (current()) control.current.copying = false; }
  };
  const start = page * OUTPUT_PAGE_SIZE;
  return <section className="space-y-4" aria-label={t("tasks.outputs.title")}>
    <p className="text-sm text-muted-foreground">{t("tasks.outputs.scope")}</p>
    <div className="flex flex-wrap items-center justify-between gap-2">
      <p className="text-sm">{t("tasks.outputs.range", { start: items.length ? start + 1 : 0, end: Math.min(start + OUTPUT_PAGE_SIZE, items.length), total: items.length })}</p>
      <div className="flex gap-2">
        <Button variant="outline" size="sm" disabled={page === 0} onClick={() => choose(null, page - 1)}>{t("tasks.outputs.previous")}</Button>
        <Button variant="outline" size="sm" disabled={start + OUTPUT_PAGE_SIZE >= items.length} onClick={() => choose(null, page + 1)}>{t("tasks.outputs.next")}</Button>
      </div>
    </div>
    {!items.length && <p role="status">{t("tasks.outputs.empty")}</p>}
    <div className="grid gap-2 sm:grid-cols-2">
      {items.slice(start, start + OUTPUT_PAGE_SIZE).map((value, index) => {
        const entry = value && typeof value === "object" ? value as Record<string, unknown> : {};
        return <Button key={start + index} variant={selected === start + index ? "secondary" : "outline"} className="h-auto justify-start whitespace-normal break-all text-left" aria-pressed={selected === start + index} onClick={() => choose(start + index)}>
          {t("tasks.outputs.item", { number: start + index + 1 })} · {typeof entry.type === "string" ? entry.type.slice(0, 80) : t("tasks.outputs.unknown")}
          {typeof entry.mime_type === "string" ? ` · ${entry.mime_type.slice(0, 120)}` : ""}
        </Button>;
      })}
    </div>
    {preview && <div className="space-y-3 rounded border p-3">
      <p className="text-sm font-medium">{t("tasks.outputs.item", { number: selected! + 1 })}</p>
      {!readable && !raw && <p className="text-xs text-muted-foreground">{t("tasks.outputs.descriptor")}</p>}
      <Button size="sm" variant="outline" aria-pressed={raw} onClick={() => {
        if (!current()) return; control.current.revision++; control.current.copying = false; setRaw(!raw); setCopyStatus("idle");
      }}>{t(raw ? "tasks.outputs.readable" : "tasks.outputs.raw")}</Button>
      {preview.truncated && <p role="status" className="text-xs">{t("tasks.outputs.truncated")}</p>}
      <pre className="max-h-96 overflow-auto whitespace-pre-wrap break-words rounded bg-muted/50 p-3 text-xs">{preview.text}</pre>
      <Button size="sm" variant="outline" disabled={copyStatus === "copying" || !preview.text} onClick={() => void copy()}>{t("tasks.outputs.copy")}</Button>
      {copyStatus !== "idle" && <p role={copyStatus === "failed" ? "alert" : "status"}>{t(`tasks.outputs.${copyStatus}`)}</p>}
    </div>}
  </section>;
}
