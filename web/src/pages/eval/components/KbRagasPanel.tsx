import { Alert, App as AntApp, Button, Descriptions, Empty, Input, Progress, Select, Space, Spin, Tag } from "antd";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Gauge, Layers, Play, SearchCheck, Sparkles } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  batchScoreKbRagasDataset,
  getKbRagasKnowledgeSummary,
  type AgentTraceDetailResponse,
  type AgentTraceSummary,
  type EvalEvaluator,
} from "@/api/eval";

import {
  KB_RAGAS_METRICS,
  datasetIdFromDetail,
  formatScorePercent,
  kbRagasMetricValue,
  kbRagasScoresFromDetail,
  ragQueryFromDetail,
  ragasLabelColor,
  retrievalContextsFromDetail,
  retrievalDocumentCountFromDetail,
} from "./tracePresentation";

interface KbRagasPanelProps {
  traces: AgentTraceSummary[];
  traceTotal: number;
  selectedTraceId?: string;
  detail?: AgentTraceDetailResponse;
  detailLoading: boolean;
  ragasEvaluators: EvalEvaluator[];
  selectedEvaluatorId?: string;
  onSelectEvaluator: (evaluatorId?: string) => void;
  onQueueEvaluator: () => void;
  queueLoading: boolean;
  onCreateRagasEvaluator: () => void;
  createLoading: boolean;
  initialDatasetId?: string;
  canRunEvaluations: boolean;
}

function MetricCard({
  metric,
  score,
  explanation,
  label,
}: {
  metric: string;
  score?: number | null;
  explanation?: string | null;
  label?: string | null;
}) {
  const { t } = useTranslation();
  const percent = typeof score === "number" && Number.isFinite(score) ? Math.round(score * 100) : 0;
  return (
    <article className="eval-ragas-metric-card" data-testid={`kb-ragas-metric-${metric}`}>
      <div className="eval-ragas-metric-head">
        <strong>{t(`eval.ragas.metrics.${metric}`, metric)}</strong>
        {label ? <Tag color={ragasLabelColor(label)}>{t(`eval.ragas.labels.${label}`, label)}</Tag> : null}
      </div>
      <div className="eval-ragas-metric-value">{formatScorePercent(score)}</div>
      <Progress
        percent={percent}
        size="small"
        showInfo={false}
        strokeColor={
          label === "pass"
            ? "hsl(var(--success))"
            : label === "fail"
              ? "hsl(var(--destructive))"
              : "hsl(var(--warning))"
        }
        railColor="hsl(var(--muted) / 0.45)"
      />
      <p className="eval-ragas-metric-copy">{explanation || t("eval.ragas.noExplanation")}</p>
    </article>
  );
}

export function KbRagasPanel({
  traces,
  traceTotal,
  selectedTraceId,
  detail,
  detailLoading,
  ragasEvaluators,
  selectedEvaluatorId,
  onSelectEvaluator,
  onQueueEvaluator,
  queueLoading,
  onCreateRagasEvaluator,
  createLoading,
  initialDatasetId,
  canRunEvaluations,
}: KbRagasPanelProps) {
  const { t } = useTranslation();
  const { message } = AntApp.useApp();
  const queryClient = useQueryClient();
  const [batchDatasetId, setBatchDatasetId] = useState(initialDatasetId || "");
  const ragasScores = useMemo(() => kbRagasScoresFromDetail(detail), [detail]);
  const contexts = useMemo(() => retrievalContextsFromDetail(detail), [detail]);
  const query = ragQueryFromDetail(detail);
  const datasetId = datasetIdFromDetail(detail);
  const scoredTraceCount = traces.filter((trace) => trace.scores_count > 0).length;
  const onlineEnabledCount = ragasEvaluators.filter((evaluator) => {
    const online = evaluator.sampling_config?.online;
    return online && typeof online === "object" && !Array.isArray(online) &&
      (online as Record<string, unknown>).enabled === true;
  }).length;

  const kbSummaryQuery = useQuery({
    queryKey: ["eval", "kb-ragas-summary", batchDatasetId || "all"],
    queryFn: () =>
      getKbRagasKnowledgeSummary({
        days: 7,
        dataset_id: batchDatasetId.trim() || undefined,
      }),
    staleTime: 20_000,
  });

  const batchMutation = useMutation({
    mutationFn: () => {
      const datasetId = batchDatasetId.trim() || datasetIdFromDetail(detail) || "";
      if (!datasetId) throw new Error(t("eval.ragas.batchDatasetRequired"));
      if (!selectedEvaluatorId) throw new Error(t("eval.workbench.createEvaluatorFirst"));
      return batchScoreKbRagasDataset(datasetId, {
        evaluator_id: selectedEvaluatorId,
        limit: 50,
        only_unscored: true,
      });
    },
    onSuccess: async (result) => {
      message.success(t("eval.ragas.batchQueued", { queued: result.queued, skipped: result.skipped }));
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["eval", "kb-ragas-summary"] }),
        queryClient.invalidateQueries({ queryKey: ["eval", "traces"] }),
      ]);
    },
    onError: (error: unknown) => {
      message.error(error instanceof Error ? error.message : String(error));
    },
  });

  const kbSummary = kbSummaryQuery.data;

  return (
    <section className="eval-panel eval-workbench-panel eval-ragas-panel" data-testid="kb-ragas-panel">
      <div className="eval-panel-heading">
        <div>
          <h2>{t("eval.ragas.title")}</h2>
          <p>{t("eval.ragas.description")}</p>
        </div>
        <div className="eval-workbench-icon">
          <Gauge size={22} />
        </div>
      </div>

      <div className="eval-ragas-summary-grid" aria-label={t("eval.ragas.summaryAria")}>
        <div className="eval-ragas-summary-card">
          <span>{t("eval.ragas.summary.ragTraces")}</span>
          <strong>{kbSummary?.rag_traces ?? traceTotal}</strong>
        </div>
        <div className="eval-ragas-summary-card">
          <span>{t("eval.ragas.summary.scoredTraces")}</span>
          <strong>{kbSummary?.ragas_scored_traces ?? scoredTraceCount}</strong>
        </div>
        <div className="eval-ragas-summary-card">
          <span>{t("eval.ragas.summary.evaluators")}</span>
          <strong>{ragasEvaluators.length}</strong>
        </div>
        <div className="eval-ragas-summary-card">
          <span>{t("eval.ragas.summary.onlineSampling")}</span>
          <strong>{onlineEnabledCount}</strong>
        </div>
      </div>

      {kbSummary?.metrics?.length ? (
        <div className="eval-ragas-metrics-grid" aria-label={t("eval.ragas.aggregateMetrics")}>
          {kbSummary.metrics.map((metric) => (
            <MetricCard
              key={metric.metric}
              metric={metric.metric}
              score={metric.scored_count > 0 ? metric.average_score : null}
              explanation={t("eval.ragas.aggregateMetricCopy", {
                scored: metric.scored_count,
                pass: metric.pass_count,
                fail: metric.fail_count,
              })}
              label={
                metric.scored_count > 0
                  ? metric.pass_count >= metric.fail_count ? "pass" : "fail"
                  : metric.review_count > 0 ? "review" : null
              }
            />
          ))}
        </div>
      ) : null}

      <div className="eval-ragas-workbench">
        <div className="eval-ragas-workbench-controls">
          <Select
            className="eval-workbench-wide"
            allowClear
            aria-label={t("eval.ragas.selectEvaluator")}
            placeholder={t("eval.ragas.selectEvaluator")}
            value={selectedEvaluatorId}
            options={ragasEvaluators.map((evaluator) => ({
              label: `${evaluator.name} (${evaluator.version})`,
              value: evaluator.evaluator_id,
            }))}
            onChange={(value) => onSelectEvaluator(value)}
          />
          <Input
            value={batchDatasetId}
            aria-label={t("eval.ragas.batchDatasetId")}
            placeholder={t("eval.ragas.batchDatasetPlaceholder")}
            onChange={(event) => setBatchDatasetId(event.target.value)}
          />
          <Space size={10} wrap>
            <Button
              type="primary"
              icon={<Play size={15} />}
              onClick={onQueueEvaluator}
              loading={queueLoading}
              disabled={!canRunEvaluations || !selectedEvaluatorId || !selectedTraceId}
            >
              {t("eval.ragas.queueSelectedTrace")}
            </Button>
            <Button
              icon={<Layers size={15} />}
              onClick={() => batchMutation.mutate()}
              loading={batchMutation.isPending}
              disabled={!canRunEvaluations || !selectedEvaluatorId}
            >
              {t("eval.ragas.batchScoreDataset")}
            </Button>
            <Button
              icon={<Sparkles size={15} />}
              onClick={onCreateRagasEvaluator}
              loading={createLoading}
              disabled={!canRunEvaluations}
            >
              {t("eval.ragas.createEvaluator")}
            </Button>
          </Space>
        </div>
        {kbSummary?.latest_judge_model ? (
          <Tag color="blue">{t("eval.ragas.judgeModel", { model: kbSummary.latest_judge_model })}</Tag>
        ) : null}

        {ragasEvaluators.length === 0 ? (
          <Alert type="info" showIcon title={t("eval.ragas.noEvaluatorTitle")} description={t("eval.ragas.noEvaluatorDescription")} />
        ) : null}
      </div>

      <div className="eval-ragas-selected">
        <div className="eval-section-title">
          <h3>{t("eval.ragas.selectedTrace")}</h3>
          <span>{selectedTraceId || t("eval.workbench.context.noSelection")}</span>
        </div>

        {!selectedTraceId ? (
          <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t("eval.ragas.selectTraceHint")} />
        ) : detailLoading ? (
          <div className="eval-ragas-loading">
            <Spin size="small" />
            <span>{t("eval.ragas.loadingDetail")}</span>
          </div>
        ) : detail ? (
          <>
            <Descriptions
              className="eval-workbench-descriptions"
              size="small"
              bordered
              column={1}
              items={[
                { key: "query", label: t("eval.ragas.fields.query"), children: query || t("eval.detail.noPreview") },
                { key: "dataset", label: t("eval.ragas.fields.dataset"), children: datasetId || "-" },
                {
                  key: "documents",
                  label: t("eval.ragas.fields.documents"),
                  children: String(retrievalDocumentCountFromDetail(detail)),
                },
              ]}
            />

            <div className="eval-ragas-metrics-grid">
              {KB_RAGAS_METRICS.map((metric) => {
                const score = kbRagasMetricValue(ragasScores, metric);
                return (
                  <MetricCard
                    key={metric}
                    metric={metric}
                    score={score?.numeric_value}
                    explanation={score?.explanation}
                    label={score?.label}
                  />
                );
              })}
            </div>

            {ragasScores.length === 0 ? (
              <Alert
                type="warning"
                showIcon
                icon={<SearchCheck size={16} />}
                title={t("eval.ragas.unscoredTitle")}
                description={t("eval.ragas.unscoredDescription")}
              />
            ) : null}

            <div className="eval-section-title eval-section-title-compact">
              <h3>{t("eval.ragas.retrievedContexts")}</h3>
              <span>{t("eval.ragas.contextCount", { count: contexts.length })}</span>
            </div>
            <div className="eval-ragas-context-list" aria-label={t("eval.ragas.retrievedContexts")}>
              {contexts.length > 0 ? (
                contexts.map((context, index) => (
                  <article className="eval-ragas-context-card" key={`${index}-${context.slice(0, 24)}`}>
                    <div className="eval-ragas-context-index">{index + 1}</div>
                    <p>{context}</p>
                  </article>
                ))
              ) : (
                <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t("eval.ragas.noContexts")} />
              )}
            </div>
          </>
        ) : (
          <Alert type="error" showIcon title={t("eval.ragas.detailUnavailableTitle")} description={t("eval.ragas.detailUnavailableDescription")} />
        )}
      </div>
    </section>
  );
}
