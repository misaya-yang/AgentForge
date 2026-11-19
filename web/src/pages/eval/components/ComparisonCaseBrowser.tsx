import { Button, Empty, Input, Select, Tag } from "antd";
import { ExternalLink } from "lucide-react";
import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { EvalExperimentRunComparisonResponse } from "@/api/eval";
import { COMPARISON_CASE_PAGE_SIZE, comparisonCaseRows, comparisonCaseStatus, comparisonCaseWindow } from "../comparisonCaseBrowser";

function statusColor(status: string) {
  if (/regress|fail|block/i.test(status)) return "error";
  if (/improv|pass/i.test(status)) return "success";
  if (/warn|inconclusive|insufficient|confound/i.test(status)) return "warning";
  return "default";
}

/** Parent keys each response observation, including replacements for the same pair. */
export function ComparisonCaseBrowser({ comparison, onOpenTrace }: {
  comparison: EvalExperimentRunComparisonResponse;
  onOpenTrace: (traceId: string) => void;
}) {
  const { t } = useTranslation();
  const [search, setSearch] = useState("");
  const [status, setStatus] = useState<string>();
  const [offset, setOffset] = useState(0);
  const mounted = useRef(true);
  useLayoutEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const rows = useMemo(() => comparisonCaseRows(comparison.case_diffs), [comparison]);
  const statuses = useMemo(() => Array.from(new Set(rows.map(row => comparisonCaseStatus(row.item)))).sort(), [rows]);
  const page = useMemo(() => comparisonCaseWindow(rows, search, status, offset), [rows, search, status, offset]);
  return <section>
      {comparison.case_diffs.length ? (
        <div className="eval-case-diffs">
          <h4>{t("eval.comparison.caseRegressions")}</h4>
          <div className="flex flex-wrap gap-3">
            <Input allowClear value={search} onChange={event => { setSearch(event.target.value); setOffset(0); }}
              className="max-w-sm" placeholder={t("eval.comparison.caseSearch")} aria-label={t("eval.comparison.caseSearch")} />
            <Select allowClear value={status} onChange={value => { setStatus(value); setOffset(0); }}
              className="min-w-48" placeholder={t("eval.comparison.allCaseStatuses")} aria-label={t("eval.comparison.caseStatus")}
              options={statuses.map(value => ({ value, label: value }))} />
          </div>
          <p role="status">{t("eval.comparison.caseWindow", { start: page.start, end: page.end, filtered: page.filtered, returned: rows.length })}</p>
          {page.filtered === 0 && <Empty description={t("eval.comparison.noMatchingCases")} /> }
          {page.rows.map(({ item, position }) => {
            const index = position;
            const status = comparisonCaseStatus(item);
            const baselineTraceId = String(item.baseline_trace_id || "");
            const candidateTraceId = String(item.candidate_trace_id || "");
            const toolDiffs = Array.isArray(item.tool_diffs) ? item.tool_diffs : [];
            const ragDiffs = Array.isArray(item.rag_diffs) ? item.rag_diffs : [];
            const baselineOutput = item.baseline_output;
            const candidateOutput = item.candidate_output;
            return (
              <article key={`${position}:${String(item.case_id || index)}:${candidateTraceId}`}>
                <div><Tag color={statusColor(status)}>{status}</Tag><strong>{String(item.case_id || `case-${index + 1}`)}</strong></div>
                <small>Execution: {String(item.baseline_execution_status || "unknown")} → {String(item.candidate_execution_status || "unknown")} · Quality: {String(item.baseline_quality_status || "unknown")} → {String(item.candidate_quality_status || "unknown")}</small>
                <p>{String(item.failure_reason || item.summary || item.output_diff || t("eval.comparison.noCaseDetail"))}</p>
                {toolDiffs.length ? <small>{t("eval.comparison.toolChanges", { count: toolDiffs.length })}</small> : null}
                {ragDiffs.length ? <small>{t("eval.comparison.ragChanges", { count: ragDiffs.length })}</small> : null}
                <div>
                  {baselineTraceId ? <Button type="link" icon={<ExternalLink size={13} />} onClick={() => { if (mounted.current) onOpenTrace(baselineTraceId); }}>{t("eval.comparison.baselineTrace")}</Button> : null}
                  {candidateTraceId ? <Button type="link" icon={<ExternalLink size={13} />} onClick={() => { if (mounted.current) onOpenTrace(candidateTraceId); }}>{t("eval.comparison.candidateTrace")}</Button> : null}
                </div>
                {baselineOutput !== undefined || candidateOutput !== undefined ? (
                  <div className="eval-case-output-diff">
                    <div><small>{t("eval.comparison.baselineOutput")}</small><p>{String(baselineOutput ?? "—")}</p></div>
                    <div><small>{t("eval.comparison.candidateOutput")}</small><p>{String(candidateOutput ?? "—")}</p></div>
                  </div>
                ) : null}
                {toolDiffs.length || ragDiffs.length ? (
                  <details className="eval-case-evidence">
                    <summary>{t("eval.comparison.trajectoryEvidence")}</summary>
                    <pre>{JSON.stringify({ tool_diffs: toolDiffs, rag_diffs: ragDiffs }, null, 2)}</pre>
                  </details>
                ) : null}
              </article>
            );
          })}
          <nav className="flex flex-wrap items-center justify-between gap-3" aria-label={t("eval.comparison.casePagination")}>
            <Button disabled={page.offset === 0} onClick={() => setOffset(Math.max(0, page.offset - COMPARISON_CASE_PAGE_SIZE))}>{t("eval.comparison.previousCases")}</Button>
            <Button disabled={!page.hasNext} onClick={() => setOffset(page.offset + COMPARISON_CASE_PAGE_SIZE)}>{t("eval.comparison.nextCases")}</Button>
          </nav>
          <p className="text-xs text-muted-foreground">{t("eval.comparison.caseScope")}</p>
        </div>
      ) : null}
  </section>;
}
