import { Alert, Button, Descriptions, Empty, Space, Spin, Tag, Timeline, Typography, message } from "antd";
import type { ReactNode } from "react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  AgentTraceDetailResponse,
  AgentTraceEvent,
  AgentTraceSpan,
  AgentTraceSummary,
  TraceStatus,
} from "@/api/eval";

import {
  compactJson,
  datasetIdFromDetail,
  formatDate,
  formatDuration,
  formatTraceCost,
  formatTraceTokens,
  locatorText,
  ragQueryFromDetail,
  retrievalContextsFromDetail,
  retrievalDocumentCountFromDetail,
  traceLocator,
} from "./tracePresentation";

const { Paragraph, Text } = Typography;
const VISIBLE_SPAN_LIMIT = 80;

interface AssistantTraceDetailProps {
  detail?: AgentTraceDetailResponse;
  loading: boolean;
  error?: Error | null;
}

function statusColor(status: TraceStatus | AgentTraceSpan["status"]) {
  if (status === "succeeded") return "green";
  if (status === "failed" || status === "timeout") return "red";
  if (status === "cancelled" || status === "skipped") return "orange";
  return "blue";
}

function PreviewBlock({ label, value, emptyText }: { label: string; value?: string; emptyText: string }) {
  return (
    <div className="eval-preview-block">
      <div className="eval-preview-label">{label}</div>
      <Paragraph className="eval-preview-text" ellipsis={{ rows: 4, expandable: true, symbol: "more" }}>
        {value || emptyText}
      </Paragraph>
    </div>
  );
}

function JsonBlock({ value, defaultOpen = false }: { value: unknown; defaultOpen?: boolean }) {
  const { t } = useTranslation();
  const json = useMemo(() => compactJson(value), [value]);
  const copy = async () => {
    await navigator.clipboard.writeText(json);
    message.success(t("eval.detail.copied"));
  };
  return (
    <details className="eval-json-details" open={defaultOpen}>
      <summary>
        <span>{t("eval.detail.jsonDetails")}</span>
        <Button size="small" type="text" onClick={copy}>
          {t("eval.detail.copyJson")}
        </Button>
      </summary>
      <pre className="eval-json-block">{json}</pre>
    </details>
  );
}

function RedactionBanner({ state }: { state: Record<string, unknown> }) {
  const { t } = useTranslation();
  const keys = Object.keys(state || {});
  return (
    <Alert
      className="eval-redaction-banner"
      type="info"
      showIcon
      title={t("eval.detail.redactedTitle")}
      description={
        keys.length > 0
          ? t("eval.detail.redactedDescriptionWithKeys", { keys: keys.join(", ") })
          : t("eval.detail.redactedDescription")
      }
    />
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value && typeof value === "object" && !Array.isArray(value));
}

function RuntimeTrajectoryPanel({ trace }: { trace: AgentTraceSummary }) {
  const { t } = useTranslation();
  const runtime = isRecord(trace.metadata?.runtime_trajectory)
    ? trace.metadata.runtime_trajectory
    : null;
  if (!runtime) return null;
  const memory = isRecord(runtime.memory) ? runtime.memory : {};
  const writer = isRecord(runtime.trace_writer_health) ? runtime.trace_writer_health : {};
  const toolSafety = isRecord(runtime.tool_safety) ? runtime.tool_safety : {};
  return (
    <div className="eval-runtime-panel" aria-label={t("eval.detail.runtime.title")}>
      <div className="eval-section-title eval-section-title-compact">
        <h3>{t("eval.detail.runtime.title")}</h3>
        <span>{t("eval.detail.runtime.schema")}</span>
      </div>
      <Descriptions
        className="eval-descriptions"
        size="small"
        column={2}
        bordered
        items={[
          {
            key: "exit",
            label: t("eval.detail.runtime.exitReason"),
            children: String(runtime.exit_reason || "-"),
          },
          {
            key: "context",
            label: t("eval.detail.runtime.contextSnapshot"),
            children: String(runtime.context_snapshot_id || "-"),
          },
          {
            key: "memory",
            label: t("eval.detail.runtime.memory"),
            children: String(memory.runtime_memory_provenance_count ?? memory.history_message_count ?? "-"),
          },
          {
            key: "writer",
            label: t("eval.detail.runtime.traceWriterIssues"),
            children: String(writer.issue_count ?? 0),
          },
          {
            key: "tool",
            label: t("eval.detail.runtime.toolSafety"),
            children: String(toolSafety.direct_registry_denied ?? "-"),
          },
          {
            key: "resume",
            label: t("eval.detail.runtime.resumeReady"),
            children: String(runtime.resume_ready ?? false),
          },
        ]}
      />
      <JsonBlock value={runtime} />
    </div>
  );
}

function RetrievalContextPanel({ detail }: { detail: AgentTraceDetailResponse }) {
  const { t } = useTranslation();
  const contexts = retrievalContextsFromDetail(detail);
  const query = ragQueryFromDetail(detail);
  const datasetId = datasetIdFromDetail(detail);
  if (detail.trace.trace_family !== "rag") return null;
  return (
    <div className="eval-ragas-retrieval-panel" aria-label={t("eval.ragas.retrievedContexts")}>
      <div className="eval-section-title eval-section-title-compact">
        <h3>{t("eval.ragas.retrievalSection")}</h3>
        <span>{t("eval.ragas.contextCount", { count: contexts.length })}</span>
      </div>
      <Descriptions
        className="eval-descriptions"
        size="small"
        column={1}
        bordered
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
      <div className="eval-ragas-context-list eval-ragas-context-list-compact">
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
    </div>
  );
}

function TranscriptLocatorPanel({ trace }: { trace: AgentTraceSummary }) {
  const { t } = useTranslation();
  const locator = traceLocator(trace);
  if (Object.keys(locator).length === 0) return null;
  return (
    <div className="eval-locator-panel" aria-label={t("eval.detail.locator.title")}>
      <div className="eval-section-title eval-section-title-compact">
        <h3>{t("eval.detail.locator.title")}</h3>
        <span>{t("eval.detail.locator.bounded")}</span>
      </div>
      <Descriptions
        className="eval-descriptions eval-locator-descriptions"
        size="small"
        column={2}
        bordered
        items={[
          {
            key: "turn",
            label: t("eval.detail.locator.turn"),
            children: locatorText(locator, "turn_index"),
          },
          {
            key: "message",
            label: t("eval.detail.locator.message"),
            children: locatorText(locator, "message_index"),
          },
          {
            key: "history",
            label: t("eval.detail.locator.history"),
            children: locatorText(locator, "history_message_count"),
          },
          {
            key: "fingerprint",
            label: t("eval.detail.locator.fingerprint"),
            children: locatorText(locator, "transcript_fingerprint"),
          },
        ]}
      />
      <div className="eval-preview-grid">
        <PreviewBlock
          label={t("eval.detail.locator.currentMessage")}
          value={locatorText(locator, "current_message_preview")}
          emptyText={t("eval.detail.noPreview")}
        />
        <PreviewBlock
          label={t("eval.detail.locator.excerpt")}
          value={locatorText(locator, "transcript_excerpt")}
          emptyText={t("eval.detail.noPreview")}
        />
      </div>
    </div>
  );
}

interface SpanTreeNode {
  span: AgentTraceSpan;
  children: SpanTreeNode[];
}

function sortSpanNodes(nodes: SpanTreeNode[]): SpanTreeNode[] {
  return [...nodes]
    .sort((left, right) => left.span.sequence_no - right.span.sequence_no)
    .map((node) => ({ ...node, children: sortSpanNodes(node.children) }));
}

function buildSpanTree(spans: AgentTraceSpan[]): SpanTreeNode[] {
  const nodes = new Map<string, SpanTreeNode>(
    spans.map((span) => [span.span_id, { span, children: [] }])
  );
  const roots: SpanTreeNode[] = [];
  for (const span of spans) {
    const node = nodes.get(span.span_id);
    if (!node) continue;
    const parentId = span.parent_span_id;
    if (parentId && nodes.has(parentId)) {
      nodes.get(parentId)!.children.push(node);
    } else {
      roots.push(node);
    }
  }
  return sortSpanNodes(roots);
}

function SpanTreeBranch({ node, depth = 0 }: { node: SpanTreeNode; depth?: number }) {
  return (
    <div className="eval-span-tree-branch" style={{ marginLeft: depth > 0 ? 14 : 0 }}>
      <SpanTimelineItem span={node.span} depth={depth} />
      {node.children.map((child) => (
        <SpanTreeBranch key={child.span.span_id} node={child} depth={depth + 1} />
      ))}
    </div>
  );
}

function SpanTimelineItem({ span, depth = 0 }: { span: AgentTraceSpan; depth?: number }) {
  const { t, i18n } = useTranslation();
  return (
    <div className="eval-timeline-card">
      <div className="eval-timeline-title">
        <span>{span.name}</span>
        <Space size={6} wrap>
          <Tag>{span.span_kind}</Tag>
          <Tag color={statusColor(span.status)}>{t(`eval.status.${span.status}`, span.status)}</Tag>
          <Tag>{formatDuration(span.duration_ms)}</Tag>
        </Space>
      </div>
      <div className="eval-timeline-meta">
        {depth > 0 ? <Tag>{t("eval.detail.childSpan", "child")}</Tag> : null}
        {t("eval.detail.sequenceMeta", {
          sequence: span.sequence_no,
          time: formatDate(span.started_at, i18n.language),
        })}
      </div>
      {span.error_message ? <Text type="danger">{span.error_message}</Text> : null}
      <div className="eval-preview-grid">
        <PreviewBlock label={t("eval.detail.inputPreview")} value={span.input_preview} emptyText={t("eval.detail.noPreview")} />
        <PreviewBlock label={t("eval.detail.outputPreview")} value={span.output_preview} emptyText={t("eval.detail.noPreview")} />
      </div>
      {Object.keys(span.attributes || {}).length > 0 ? <JsonBlock value={span.attributes} /> : null}
    </div>
  );
}

function EventTimelineItem({ event }: { event: AgentTraceEvent }) {
  const { t, i18n } = useTranslation();
  return (
    <div className="eval-event-row">
      <div className="eval-event-main">
        <span className="eval-event-type">{event.event_type}</span>
        <span className="eval-timeline-meta">
          {t("eval.detail.sequenceMeta", {
            sequence: event.sequence_no,
            time: formatDate(event.occurred_at, i18n.language),
          })}
        </span>
      </div>
      <Space size={6} wrap>
        <Tag>{t("eval.detail.bytes", { count: event.payload_size_bytes.toLocaleString() })}</Tag>
        <Tag color={event.redacted ? "green" : "red"}>
          {event.redacted ? t("eval.detail.redacted") : t("eval.detail.raw")}
        </Tag>
      </Space>
      <JsonBlock value={event.payload} />
    </div>
  );
}

export function AssistantTraceDetail({ detail, loading, error }: AssistantTraceDetailProps) {
  const { t, i18n } = useTranslation();
  const [showAllSpans, setShowAllSpans] = useState(false);

  if (loading) {
    return (
      <section className="eval-panel eval-detail-empty">
        <Spin description={t("eval.detail.loading")} />
      </section>
    );
  }

  if (error) {
    return (
      <section className="eval-panel eval-detail-empty" role="alert">
        <Alert
          type="error"
          showIcon
          title={t("eval.detail.unavailableTitle")}
          description={error.message || t("eval.detail.unavailableDescription")}
        />
      </section>
    );
  }

  if (!detail) {
    return (
      <section className="eval-panel eval-detail-empty">
        <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t("eval.detail.empty")} />
      </section>
    );
  }

  const { trace, spans, events } = detail;
  const visibleSpans = showAllSpans ? spans : spans.slice(0, VISIBLE_SPAN_LIMIT);
  const spanTree = buildSpanTree(visibleSpans);
  const hiddenSpanCount = Math.max(spans.length - visibleSpans.length, 0);
  const timelineItems: { key: string; dot?: ReactNode; content: ReactNode; color?: string }[] =
    spanTree.length > 0
      ? spanTree.map((node) => ({
          key: node.span.span_id,
          color: statusColor(node.span.status),
          content: <SpanTreeBranch node={node} />,
        }))
      : events.slice(0, 8).map((event) => ({
          key: event.event_id,
          color: event.event_type.includes("error") ? "red" : "blue",
          content: <EventTimelineItem event={event} />,
        }));

  return (
    <section className="eval-panel eval-trace-detail" aria-label="Assistant trace detail">
      <div className="eval-panel-heading">
        <div>
          <h2>{t("eval.detail.title")}</h2>
          <p>{trace.request_id || trace.trace_id}</p>
        </div>
        <Space size={6} wrap>
          <Tag color={statusColor(trace.status)}>{t(`eval.status.${trace.status}`, trace.status)}</Tag>
          <Tag>{trace.workflow_kind}</Tag>
          <Tag>{t("eval.score.count", { count: trace.scores_count })}</Tag>
        </Space>
      </div>

      <RedactionBanner state={trace.redaction_state} />

      <div className="eval-metric-grid" aria-label="Trace metrics">
        <div className="eval-metric-card">
          <span>{t("eval.detail.metrics.totalLatency")}</span>
          <strong>{formatDuration(trace.total_latency_ms)}</strong>
        </div>
        <div className="eval-metric-card">
          <span>{t("eval.detail.metrics.firstToken")}</span>
          <strong>{formatDuration(trace.first_token_latency_ms)}</strong>
        </div>
        <div className="eval-metric-card">
          <span>{t("eval.detail.metrics.tokens")}</span>
          <strong>{formatTraceTokens(trace)}</strong>
        </div>
        <div className="eval-metric-card">
          <span>{t("eval.detail.metrics.cost")}</span>
          <strong>{formatTraceCost(trace)}</strong>
        </div>
      </div>

      <Descriptions
        className="eval-descriptions"
        size="small"
        column={1}
        bordered
        items={[
          { key: "session", label: t("eval.detail.fields.session"), children: trace.session_id || "-" },
          { key: "run", label: t("eval.detail.fields.run"), children: trace.run_id || "-" },
          { key: "request", label: t("eval.detail.fields.request"), children: trace.request_id || "-" },
          { key: "user", label: t("eval.detail.fields.user"), children: trace.user_id || "-" },
          { key: "model", label: t("eval.detail.fields.model"), children: trace.model_id || "-" },
          { key: "provider", label: t("eval.detail.fields.provider"), children: trace.provider || "-" },
          { key: "started", label: t("eval.detail.fields.started"), children: formatDate(trace.started_at, i18n.language) },
          { key: "ended", label: t("eval.detail.fields.ended"), children: formatDate(trace.ended_at, i18n.language) },
          { key: "updated", label: t("eval.detail.fields.updated"), children: formatDate(trace.updated_at, i18n.language) },
        ]}
      />

      <TranscriptLocatorPanel trace={trace} />

      <RuntimeTrajectoryPanel trace={trace} />

      <div className="eval-preview-grid">
        <PreviewBlock label={t("eval.detail.inputPreview")} value={trace.input_preview} emptyText={t("eval.detail.noPreview")} />
        <PreviewBlock label={t("eval.detail.outputPreview")} value={trace.output_preview} emptyText={t("eval.detail.noPreview")} />
      </div>

      <RetrievalContextPanel detail={detail} />

      <div className="eval-section-title">
        <h3>{t("eval.detail.executionTimeline")}</h3>
        <Space size={8} wrap>
          <span>
            {spans.length > 0
              ? t("eval.detail.spanCount", { count: spans.length })
              : t("eval.detail.eventFallbackCount", { count: events.length })}
          </span>
          {hiddenSpanCount > 0 || showAllSpans ? (
            <Button size="small" onClick={() => setShowAllSpans((value) => !value)}>
              {showAllSpans
                ? t("eval.detail.showLessSpans")
                : `${t("eval.detail.showAllSpans")} (${hiddenSpanCount})`}
            </Button>
          ) : null}
        </Space>
      </div>
      <Timeline className="eval-timeline" mode="start" items={timelineItems} />

      <div className="eval-section-title">
        <h3>{t("eval.detail.orderedEvents")}</h3>
        <span>{t("eval.detail.eventCount", { count: events.length })}</span>
      </div>
      <div className="eval-event-list">
        {events.length > 0 ? (
          events.map((event) => <EventTimelineItem key={event.event_id} event={event} />)
        ) : (
          <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t("eval.detail.noEvents")} />
        )}
      </div>
    </section>
  );
}
