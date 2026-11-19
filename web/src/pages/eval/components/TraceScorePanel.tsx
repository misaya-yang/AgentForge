import { Alert, Button, Descriptions, Drawer, Empty, Form, Input, InputNumber, Select, Space, Spin, Tabs, Tag, Typography } from "antd";
import { Database, Download, Send, ShieldCheck } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  AgentTraceDetailResponse,
  AgentTraceScore,
  AgentTraceScoreCreate,
  AgentTraceSummary,
  EvalTraceExportResponse,
  ScoreType,
} from "@/api/eval";

import {
  KB_RAGAS_METRICS,
  compactJson,
  formatDate,
  formatScorePercent,
  isKbRagasScore,
  kbRagasMetricValue,
  ragasLabelColor,
} from "./tracePresentation";

const { Text } = Typography;

interface TraceScorePanelProps {
  traceId?: string;
  trace?: AgentTraceSummary | null;
  detail?: AgentTraceDetailResponse;
  scores: AgentTraceScore[];
  loading: boolean;
  submitting: boolean;
  error?: Error | null;
  exportPreview?: EvalTraceExportResponse | null;
  exportLoading?: boolean;
  datasetActionLoading?: boolean;
  activeDatasetName?: string | null;
  onSubmit: (payload: AgentTraceScoreCreate) => Promise<void>;
  onExport?: () => void;
  onPromoteToGolden?: () => void;
  onAddToReview?: () => void;
  onCreateFailureCase?: () => void;
  readOnly?: boolean;
}

interface ScoreFormValues {
  score_name: string;
  score_type: ScoreType;
  numeric_value?: number | null;
  boolean_value?: boolean | null;
  categorical_value?: string;
  text_value?: string;
  label?: string;
  explanation?: string;
}

function renderScoreValue(score: AgentTraceScore) {
  if (score.score_type === "numeric") return score.numeric_value ?? "-";
  if (score.score_type === "boolean") return score.boolean_value === null ? "-" : String(score.boolean_value);
  if (score.score_type === "categorical") return score.categorical_value || "-";
  return score.text_value || "-";
}

function JsonBlock({ value }: { value: unknown }) {
  return <pre className="eval-json-block eval-inspector-json">{compactJson(value)}</pre>;
}

export function TraceScorePanel({
  traceId,
  trace,
  detail,
  scores,
  loading,
  submitting,
  error,
  exportPreview,
  exportLoading,
  datasetActionLoading,
  activeDatasetName,
  onSubmit,
  onExport,
  onPromoteToGolden,
  onAddToReview,
  onCreateFailureCase,
  readOnly = false,
}: TraceScorePanelProps) {
  const { t, i18n } = useTranslation();
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [form] = Form.useForm<ScoreFormValues>();
  const scoreType = Form.useWatch("score_type", form) || "numeric";
  const defaultValues = useMemo<ScoreFormValues>(
    () => ({
      score_name: "quality",
      score_type: "numeric",
      numeric_value: 0.8,
      label: "reviewed",
    }),
    []
  );

  const submit = async (values: ScoreFormValues) => {
    const payload: AgentTraceScoreCreate = {
      score_name: values.score_name,
      score_type: values.score_type,
      numeric_value: values.score_type === "numeric" ? values.numeric_value ?? null : null,
      boolean_value: values.score_type === "boolean" ? values.boolean_value ?? null : null,
      categorical_value: values.score_type === "categorical" ? values.categorical_value || null : null,
      text_value: values.score_type === "text" ? values.text_value || null : null,
      label: values.label || null,
      explanation: values.explanation || null,
      scorer_type: "human",
      metadata: { source: "eval_console" },
    };
    await onSubmit(payload);
    form.resetFields();
    form.setFieldsValue(defaultValues);
    setDrawerOpen(false);
  };

  const ragasScores = useMemo(() => scores.filter(isKbRagasScore), [scores]);
  const otherScores = useMemo(() => scores.filter((score) => !isKbRagasScore(score)), [scores]);

  const scoreList = (
    <div className="eval-inspector-tab">
      <div className="eval-inspector-action-row">
        <div>
          <strong>{t("eval.score.records", { count: scores.length })}</strong>
          <span>{traceId ? t("eval.score.traceSelected") : t("eval.score.noTrace")}</span>
        </div>
        <Button type="primary" size="small" disabled={readOnly || !traceId} onClick={() => setDrawerOpen(true)}>
          {t("eval.workbench.inspector.addScore")}
        </Button>
      </div>

      {error ? (
        <Alert
          type="error"
          showIcon
          title={t("eval.score.unavailableTitle")}
          description={error.message || t("eval.score.unavailableDescription")}
        />
      ) : null}

      {trace?.trace_family === "rag" ? (
        <div className="eval-ragas-inspector-block" aria-label={t("eval.ragas.title")}>
          <div className="eval-section-title eval-section-title-compact">
            <h3>{t("eval.ragas.title")}</h3>
            <span>{t("eval.ragas.scoreCount", { count: ragasScores.length })}</span>
          </div>
          <div className="eval-ragas-metrics-grid eval-ragas-metrics-grid-compact">
            {KB_RAGAS_METRICS.map((metric) => {
              const score = kbRagasMetricValue(ragasScores, metric);
              return (
                <article className="eval-ragas-metric-card" key={metric}>
                  <div className="eval-ragas-metric-head">
                    <strong>{t(`eval.ragas.metrics.${metric}`, metric)}</strong>
                    {score?.label ? (
                      <Tag color={ragasLabelColor(score.label)}>
                        {t(`eval.ragas.labels.${score.label}`, score.label)}
                      </Tag>
                    ) : null}
                  </div>
                  <div className="eval-ragas-metric-value">{formatScorePercent(score?.numeric_value)}</div>
                  <p className="eval-ragas-metric-copy">
                    {score?.explanation || t("eval.ragas.noExplanation")}
                  </p>
                </article>
              );
            })}
          </div>
        </div>
      ) : null}

      <div className="eval-score-list" aria-label="Trace score records">
        {loading ? (
          <div className="eval-score-state">
            <Spin size="small" />
          </div>
        ) : otherScores.length > 0 ? (
          otherScores.map((score) => (
            <article className="eval-score-record" key={score.score_id}>
              <Space size={6} wrap>
                <strong>{score.score_name}</strong>
                <Tag>{t(`eval.score.types.${score.score_type}`, score.score_type)}</Tag>
                {score.label ? <Tag color="blue">{score.label}</Tag> : null}
              </Space>
              <div className="eval-score-description">
                <strong>{renderScoreValue(score)}</strong>
                <span>{score.explanation || t("eval.score.noExplanation")}</span>
                <Text type="secondary">
                  {score.created_by} · {formatDate(score.created_at, i18n.language)}
                </Text>
              </div>
            </article>
          ))
        ) : ragasScores.length === 0 ? (
          <div className="eval-score-state">
            <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t("eval.score.empty")} />
          </div>
        ) : null}
      </div>
    </div>
  );

  const metadataTab = (
    <div className="eval-inspector-tab">
      {trace ? (
        <>
          <Descriptions
            className="eval-workbench-descriptions"
            size="small"
            column={1}
            bordered
            items={[
              { key: "trace", label: t("eval.workbench.inspector.trace"), children: trace.trace_id },
              { key: "family", label: t("eval.workbench.context.family"), children: trace.trace_family },
              { key: "workflow", label: t("eval.workbench.inspector.workflow"), children: trace.workflow_kind },
              { key: "source", label: t("eval.workbench.inspector.sourceAdapter"), children: trace.source_adapter || "-" },
            ]}
          />
          <h3>{t("eval.workbench.inspector.traceMetadata")}</h3>
          <JsonBlock value={trace.metadata || {}} />
          <h3>{t("eval.workbench.inspector.metrics")}</h3>
          <JsonBlock value={trace.metrics || {}} />
          <h3>{t("eval.workbench.inspector.privacy")}</h3>
          <JsonBlock value={trace.privacy || trace.redaction_state || {}} />
        </>
      ) : (
        <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t("eval.workbench.inspector.noTrace")} />
      )}
    </div>
  );

  const exportTab = (
    <div className="eval-inspector-tab">
      <p className="eval-inspector-copy">{t("eval.workbench.inspector.exportDescription")}</p>
      <Button
        icon={<Download size={15} />}
        onClick={onExport}
        loading={exportLoading}
        disabled={!traceId}
        block
      >
        {t("eval.workbench.exportOpenInference")}
      </Button>
      {exportPreview ? (
        <>
          <Tag color="blue">{t("eval.workbench.exportFormat", { format: exportPreview.format })}</Tag>
          <JsonBlock value={exportPreview.payload} />
        </>
      ) : null}
    </div>
  );

  const datasetTab = (
    <div className="eval-inspector-tab">
      <p className="eval-inspector-copy">{t("eval.workbench.inspector.datasetDescription")}</p>
      <Descriptions
        className="eval-workbench-descriptions"
        size="small"
        column={1}
        bordered
        items={[
          { key: "dataset", label: t("eval.workbench.currentDataset"), children: activeDatasetName || "-" },
          { key: "trace", label: t("eval.workbench.selectedTrace"), children: traceId || "-" },
          { key: "spans", label: t("eval.detail.executionTimeline"), children: String(detail?.spans.length ?? 0) },
        ]}
      />
      <Space direction="vertical" size={8} style={{ width: "100%" }}>
        <Button
          icon={<Database size={15} />}
          onClick={onPromoteToGolden}
          loading={datasetActionLoading}
          disabled={readOnly || !traceId || !activeDatasetName}
          block
        >
          {t("eval.workbench.promoteToGolden")}
        </Button>
        <Button
          icon={<Send size={15} />}
          onClick={onAddToReview}
          loading={datasetActionLoading}
          disabled={readOnly || !traceId || !activeDatasetName}
          block
        >
          {t("eval.workbench.addToReview")}
        </Button>
        <Button
          icon={<ShieldCheck size={15} />}
          onClick={onCreateFailureCase}
          loading={datasetActionLoading}
          disabled={readOnly || !traceId || !activeDatasetName}
          block
        >
          {t("eval.workbench.createFailureCase")}
        </Button>
      </Space>
    </div>
  );

  return (
    <aside className="eval-panel eval-score-panel eval-inspector-panel" aria-label={t("eval.workbench.inspector.title")}>
      <div className="eval-panel-heading">
        <div>
          <h2>{t("eval.workbench.inspector.title")}</h2>
          <p>{t("eval.workbench.inspector.description")}</p>
        </div>
        <Tag>{traceId ? t("eval.score.traceSelected") : t("eval.score.noTrace")}</Tag>
      </div>

      <Tabs
        className="eval-inspector-tabs"
        defaultActiveKey="scores"
        items={[
          { key: "scores", label: t("eval.workbench.inspector.scores"), children: scoreList },
          { key: "metadata", label: t("eval.workbench.inspector.metadata"), children: metadataTab },
          { key: "export", label: t("eval.workbench.inspector.export"), children: exportTab },
          { key: "dataset", label: t("eval.workbench.inspector.dataset"), children: datasetTab },
        ]}
      />

      <Drawer
        title={t("eval.workbench.inspector.scoreDrawerTitle")}
        open={drawerOpen}
        onClose={() => setDrawerOpen(false)}
        size="large"
        destroyOnHidden
      >
        <Form
          form={form}
          className="eval-score-form"
          layout="vertical"
          initialValues={defaultValues}
          onFinish={submit}
          disabled={readOnly || !traceId || submitting}
        >
          <div className="eval-score-form-grid">
            <Form.Item
              label={t("eval.score.form.scoreName")}
              name="score_name"
              rules={[{ required: true, message: t("eval.score.form.scoreNameRequired") }]}
            >
              <Input maxLength={96} placeholder={t("eval.score.form.scoreNamePlaceholder")} />
            </Form.Item>
            <Form.Item label={t("eval.score.form.type")} name="score_type" rules={[{ required: true }]}>
              <Select
                options={[
                  { label: t("eval.score.types.numeric"), value: "numeric" },
                  { label: t("eval.score.types.boolean"), value: "boolean" },
                  { label: t("eval.score.types.categorical"), value: "categorical" },
                  { label: t("eval.score.types.text"), value: "text" },
                ]}
              />
            </Form.Item>
          </div>

          {scoreType === "numeric" ? (
            <Form.Item label={t("eval.score.form.numericValue")} name="numeric_value">
              <InputNumber min={0} max={1} step={0.05} style={{ width: "100%" }} />
            </Form.Item>
          ) : null}
          {scoreType === "boolean" ? (
            <Form.Item label={t("eval.score.form.booleanValue")} name="boolean_value">
              <Select
                options={[
                  { label: t("eval.score.boolean.true"), value: true },
                  { label: t("eval.score.boolean.false"), value: false },
                ]}
              />
            </Form.Item>
          ) : null}
          {scoreType === "categorical" ? (
            <Form.Item label={t("eval.score.form.category")} name="categorical_value">
              <Input maxLength={96} placeholder={t("eval.score.form.categoryPlaceholder")} />
            </Form.Item>
          ) : null}
          {scoreType === "text" ? (
            <Form.Item label={t("eval.score.form.textValue")} name="text_value">
              <Input.TextArea maxLength={2000} rows={3} placeholder={t("eval.score.form.textPlaceholder")} />
            </Form.Item>
          ) : null}

          <Form.Item label={t("eval.score.form.label")} name="label">
            <Input maxLength={96} placeholder={t("eval.score.form.labelPlaceholder")} />
          </Form.Item>
          <Form.Item label={t("eval.score.form.explanation")} name="explanation">
            <Input.TextArea maxLength={2000} rows={3} placeholder={t("eval.score.form.explanationPlaceholder")} />
          </Form.Item>

          <Button
            type="primary"
            htmlType="submit"
            icon={<Send size={15} />}
            loading={submitting}
            disabled={readOnly || !traceId}
            block
          >
            {t("eval.score.form.submit")}
          </Button>
        </Form>
      </Drawer>
    </aside>
  );
}
