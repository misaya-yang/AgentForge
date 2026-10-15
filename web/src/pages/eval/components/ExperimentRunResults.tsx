import { Alert, Button, Checkbox, Empty, Input, Progress, Segmented, Space, Spin, Tag } from "antd";
import { ExternalLink, RefreshCw, Search } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  EvalExperimentCaseResult,
  EvalExperimentRun,
  EvalExperimentRunResultsResponse,
} from "@/api/eval";
import { agentVersionTarget } from "../agentVersionTarget";
import { canSelectEvalCaseForRetry, isFailedEvalCase } from "../retryAvailability";

type ResultFilter = "all" | "failed" | "review" | "passed" | "unscored";

function numberValue(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function recordValue(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function metricValue(run: EvalExperimentRun, ...keys: string[]): number | null {
  for (const source of [run.metrics || {}, run.score_summary || {}]) {
    for (const key of keys) {
      const value = numberValue(source[key]);
      if (value !== null) return value;
    }
  }
  return null;
}

function formatValue(value: number | null, kind: "number" | "ms" | "percent" | "cost" = "number") {
  if (value === null) return "—";
  if (kind === "ms") return `${Math.round(value)} ms`;
  if (kind === "percent") return `${(value * 100).toFixed(1)}%`;
  if (kind === "cost") return `${value.toFixed(3)}¢`;
  return value.toLocaleString(undefined, { maximumFractionDigits: 3 });
}

function statusColor(status: EvalExperimentCaseResult["status"]) {
  if (status === "passed") return "success";
  if (status === "failed") return "error";
  if (status === "review") return "warning";
  return "default";
}

function runStatusColor(status: EvalExperimentRun["status"]) {
  if (status === "succeeded") return "success";
  if (status === "failed" || status === "cancelled") return "error";
  if (status === "running") return "processing";
  return "default";
}

export function ExperimentRunResults({
  run,
  results,
  loading,
  error,
  onRetry,
  onOpenTrace,
  onCancel,
  onRetryFailedCases,
  actionLoading = false,
}: {
  run: EvalExperimentRun | null;
  results: EvalExperimentRunResultsResponse | null;
  loading: boolean;
  error: Error | null;
  onRetry: () => void;
  onOpenTrace: (item: EvalExperimentCaseResult) => void;
  onCancel?: () => void;
  onRetryFailedCases?: (caseIds: string[]) => void;
  actionLoading?: boolean;
}) {
  const { t } = useTranslation();
  const [filter, setFilter] = useState<ResultFilter>("all");
  const [query, setQuery] = useState("");
  const [selectedCaseIds, setSelectedCaseIds] = useState<Set<string>>(new Set());
  useEffect(() => setSelectedCaseIds(new Set()), [run?.run_id]);
  const cases = useMemo(() => results?.cases || [], [results?.cases]);
  const retryableIds = new Set(cases
    .filter(canSelectEvalCaseForRetry)
    .map((item) => item.case_id));
  const selectedRetryIds = [...selectedCaseIds].filter((caseId) => retryableIds.has(caseId));
  const visibleCases = useMemo(() => {
    const normalizedQuery = query.trim().toLowerCase();
    return [...cases]
      .filter((item) => filter === "all" || item.status === filter
        || (filter === "failed" && isFailedEvalCase(item)))
      .filter((item) => {
        if (!normalizedQuery) return true;
        return [item.case_id, item.failure_reason, item.candidate_trace_id]
          .filter(Boolean)
          .some((value) => String(value).toLowerCase().includes(normalizedQuery));
      })
      .sort((left, right) => {
        const rank = { failed: 0, review: 1, unscored: 2, passed: 3 };
        return rank[left.status] - rank[right.status];
      });
  }, [cases, filter, query]);

  if (!run) {
    return (
      <div className="eval-run-empty">
        {loading ? (
          <Spin size="small" description={t("eval.workbench.loadingRun", "Loading run…")}>
            <div className="eval-run-loading-placeholder" />
          </Spin>
        ) : error ? (
          <Alert
            type="error"
            showIcon
            title={t("eval.workbench.runLoadFailed", "Could not load this run")}
            description={error.message}
            action={<Button onClick={onRetry}>{t("common.retry", "Retry")}</Button>}
          />
        ) : (
          <Empty description={t("eval.workbench.runEmpty", "Run a test set or select a previous run to inspect results.")} />
        )}
      </div>
    );
  }

  const summary = run.score_summary || {};
  const isPending = run.status === "queued" || run.status === "running";
  const averageScore = numberValue(summary.average_score ?? summary.overall_score);
  const scoredCount = numberValue(summary.scored_count);
  const reviewCount = numberValue(summary.review_count);
  const skippedCount = numberValue(summary.skipped_count);
  const qualityCaseCount = numberValue(summary.case_count);
  const expectedCaseCount = numberValue(summary.expected_case_count);
  const excludedCaseCount = numberValue(summary.excluded_case_count);
  const targetSnapshot = recordValue(run.target_snapshot);
  const agentTarget = agentVersionTarget(run.target_snapshot);
  const candidateFingerprint = recordValue(run.candidate_fingerprint);
  const modelRef = recordValue(candidateFingerprint.model_ref);
  const gate = recordValue(run.metrics?.gate);
  const gateStatus = String(gate.status || run.gate_status || "not evaluated");
  const qualityStatus = summary.pending_human === true
    ? "Pending human review"
    : excludedCaseCount && excludedCaseCount > 0
      ? "Incomplete"
      : (qualityCaseCount && qualityCaseCount > 0) || (scoredCount && scoredCount > 0)
        ? "Scored"
        : "Unscored";
  const progress = { ...(run.metrics || {}), ...recordValue(run.metrics?.progress), ...(run.progress || {}) };
  const completed = numberValue(progress.completed_trials ?? progress.completed_cases ?? progress.completed);
  const failed = numberValue(progress.failed_trials ?? progress.failed_cases ?? progress.failed);
  const total = numberValue(progress.total_trials ?? progress.total_cases ?? progress.total);
  const progressed = numberValue(progress.attempted_trials) ?? completed ?? failed;
  const progressPercent = progressed !== null && total && total > 0
    ? Math.min(100, Math.round((progressed / total) * 100))
    : null;
  const metricCards = [
    [t("eval.workbench.qualityScore", "Quality"), metricValue(run, "quality_score", "overall_score", "average_score"), "number"],
    [t("eval.workbench.behaviorPassRate", "Behavior pass"), metricValue(run, "behavior_pass_rate", "pass_rate"), "percent"],
    [t("eval.workbench.latencyP50", "P50 latency"), metricValue(run, "latency_p50_ms", "p50_latency_ms", "avg_latency_ms"), "ms"],
    [t("eval.workbench.latencyP95", "P95 latency"), metricValue(run, "latency_p95_ms", "p95_latency_ms"), "ms"],
    [t("eval.workbench.tokensPerTask", "Tokens / task"), metricValue(run, "total_tokens_per_task", "tokens_per_task"), "number"],
    [t("eval.workbench.costPerTask", "Cost / task"), metricValue(run, "cost_per_task_cents", "cost_cents_per_task"), "cost"],
    [t("eval.workbench.executionErrorRate", "Execution errors"), metricValue(run, "execution_error_rate", "error_rate"), "percent"],
    ["Judge failures", metricValue(run, "judge_failed_trials"), "number"],
    [t("eval.workbench.flakyRate", "Flaky"), metricValue(run, "flaky_rate"), "percent"],
  ] as const;

  return (
    <section className="eval-run-results" aria-live="polite">
      <div className="eval-run-results-header">
        <div>
          <Space size={8} wrap>
            <h3>{t("eval.workbench.runResults", "Run results")} · {cases.length}/{results?.total ?? cases.length}</h3>
            <Tag color={runStatusColor(run.status)}>{run.status}</Tag>
          </Space>
          <code>{run.run_id}</code>
          <div>Mode: {run.run_mode || String(targetSnapshot.run_mode || "rescore_trace")}
            {targetSnapshot.dataset_version ? ` · Dataset ${String(targetSnapshot.dataset_version)}` : ""}
            {modelRef.model_id ? ` · Model ${String(modelRef.model_id)}` : ""}
            {run.repetitions && run.repetitions > 1 ? ` · ${run.repetitions} attempts per case` : ""}
          </div>
          {run.dataset_manifest_hash ? <div>Dataset snapshot: <code>{run.dataset_manifest_hash.slice(0, 12)}</code></div> : null}
          <div>Execution: {run.status} · Quality: {qualityStatus} · {agentTarget ? "Candidate gate" : "Gate"}: {gateStatus}</div>
          {expectedCaseCount !== null ? (
            <div>Quality coverage: {qualityCaseCount ?? 0}/{expectedCaseCount} cases
              {excludedCaseCount ? ` · ${excludedCaseCount} excluded from scoring` : ""}
            </div>
          ) : null}
        </div>
        <Space wrap>
          {isPending && onCancel ? <Button danger onClick={onCancel} loading={actionLoading}>Cancel run</Button> : null}
          {!isPending && run.run_mode === "live_candidate" && onRetryFailedCases && retryableIds.size > 0 ? (
            <Button disabled={selectedRetryIds.length === 0} loading={actionLoading}
              onClick={() => onRetryFailedCases(selectedRetryIds)}>
              Retry selected failed cases ({selectedRetryIds.length})
            </Button>
          ) : null}
          <Button icon={<RefreshCw size={15} />} onClick={onRetry} loading={loading}>
            {t("common.refresh", "Refresh")}
          </Button>
        </Space>
      </div>
      {agentTarget ? <Alert
        type="info"
        showIcon
        title={t("eval.workbench.agentCandidateNotRelease", "Candidate run results are not release approval")}
        description={t("eval.workbench.agentCandidateCoverage", {
          scored: qualityCaseCount ?? scoredCount ?? "unknown",
          expected: expectedCaseCount ?? "unknown",
          excluded: excludedCaseCount ?? "unknown",
          defaultValue: "Scored {{scored}} of {{expected}} expected cases; excluded {{excluded}}. Formal publication validation remains separate.",
        })}
      /> : null}

      {isPending ? (
        <div className="eval-run-progress" role="status">
          {progressPercent === null ? <Spin size="small" /> : <Progress percent={progressPercent} status="active" />}
          <span>
            {total !== null
              ? t("eval.workbench.realProgress", "{{completed}} / {{total}} completed · {{failed}} failed", { completed: completed ?? 0, total, failed: failed ?? 0 })
              : run.status === "running" ? t("eval.workbench.scoringCases", "Scoring test cases…") : t("eval.workbench.waitingEvaluator", "Waiting for evaluator worker…")}
          </span>
        </div>
      ) : null}
      {run.error_message ? <Alert type="error" showIcon title={run.error_message} /> : null}
      {excludedCaseCount && excludedCaseCount > 0 ? (
        <Alert type="warning" showIcon title="Some cases have no valid quality result"
          description="Execution or judging failed for these cases. They are excluded from the quality score and cannot pass the gate." />
      ) : null}
      {error ? (
        <Alert
          type="error"
          showIcon
          title={t("eval.workbench.resultsLoadFailed", "Could not load case results")}
          description={error.message}
          action={<Button onClick={onRetry}>{t("common.retry", "Retry")}</Button>}
        />
      ) : null}

      <div className="eval-run-metrics">
        {metricCards.map(([label, value, kind]) => <div key={label}><span>{label}</span><strong>{formatValue(value, kind)}</strong></div>)}
        <div><span>{t("eval.workbench.averageScore", "Average score")}</span><strong>{averageScore === null ? "—" : averageScore.toFixed(3)}</strong></div>
        <div><span>{t("eval.workbench.scored", "Scored")}</span><strong>{scoredCount ?? "—"}</strong></div>
        <div><span>{t("eval.workbench.needsReview", "Needs review")}</span><strong>{reviewCount ?? "—"}</strong></div>
        <div><span>{t("eval.workbench.skipped", "Skipped")}</span><strong>{skippedCount ?? "—"}</strong></div>
      </div>

      <div className="eval-run-toolbar">
        <Segmented<ResultFilter>
          value={filter}
          onChange={setFilter}
          options={[
            { label: `All ${cases.length}`, value: "all" },
            { label: `Failed ${cases.filter(isFailedEvalCase).length}`, value: "failed" },
            { label: `Review ${cases.filter((item) => item.status === "review").length}`, value: "review" },
            { label: `Passed ${cases.filter((item) => item.status === "passed").length}`, value: "passed" },
            { label: `Unscored ${cases.filter((item) => item.status === "unscored").length}`, value: "unscored" },
          ]}
        />
        <Input
          allowClear
          prefix={<Search size={14} />}
          placeholder={t("eval.workbench.searchRunResults", "Search case, failure, or trace")}
          value={query}
          onChange={(event) => setQuery(event.target.value)}
        />
      </div>

      {visibleCases.length ? (
        <div className="eval-case-results" role="list" aria-label="Evaluation case results">
          {visibleCases.map((item) => (
            <article key={`${item.example_id || item.case_id}:${item.candidate_trace_id}`} className="eval-case-result" role="listitem">
              <div className="eval-case-result-main">
                {!isPending && run.run_mode === "live_candidate" && retryableIds.has(item.case_id) && onRetryFailedCases ? (
                  <Checkbox
                    aria-label={`Select failed case ${item.case_id} for retry`}
                    checked={selectedCaseIds.has(item.case_id)}
                    onChange={(event) => setSelectedCaseIds((current) => {
                      const next = new Set(current);
                      if (event.target.checked) next.add(item.case_id); else next.delete(item.case_id);
                      return next;
                    })}
                  />
                ) : null}
                <Tag color={recordValue(item).execution_status === "failed" ? "error" : statusColor(item.status)}>
                  {recordValue(item).execution_status === "failed" ? "execution failed · unscored" : item.status}
                </Tag>
                <div>
                  <strong>{item.case_id}</strong>
                  <span>{item.trace.model_id || "unknown model"} · {item.trace.total_latency_ms ?? "—"} ms</span>
                </div>
              </div>
              <div className="eval-case-score-list">
                {item.scores.length === 0 && typeof item.aggregate_score === "number" ? (
                  <Tag color={item.status === "failed" ? "error" : "success"}>
                    {t("eval.workbench.aggregateScore", "Aggregate")}: {item.aggregate_score.toFixed(3)}
                  </Tag>
                ) : null}
                {item.scores.map((score) => (
                  <Tag key={`${score.score_name}:${score.target_type || "trace"}:${score.target_id || score.span_id || ""}`} color={score.label === "pass" ? "success" : score.label === "fail" ? "error" : "warning"}>
                    {score.score_name}: {typeof score.numeric_value === "number" ? score.numeric_value.toFixed(3) : score.label || "review"}
                  </Tag>
                ))}
              </div>
              <p title={item.failure_reason || undefined}>
                {item.failure_reason || item.trace.output_preview || t("eval.workbench.noFailureDetail", "No failure detail")}
              </p>
              <Button
                type="link"
                icon={<ExternalLink size={14} />}
                onClick={() => onOpenTrace(item)}
                disabled={!item.candidate_trace_id}
              >
                {t("eval.workbench.openTrace", "Open trace")}
              </Button>
            </article>
          ))}
        </div>
      ) : (
        <Empty
          image={Empty.PRESENTED_IMAGE_SIMPLE}
          description={isPending
            ? t("eval.workbench.resultsPending", "Results will appear when scoring completes.")
            : cases.length === 0
              ? t("eval.workbench.noRunCases", "This run did not return case results.")
              : t("eval.workbench.noMatchingCases", "No cases match this filter.")}
        />
      )}
    </section>
  );
}
