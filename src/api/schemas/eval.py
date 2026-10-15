from __future__ import annotations

from datetime import datetime
from typing import Any, Literal
from uuid import UUID

from pydantic import BaseModel, ConfigDict, Field, StrictBool, StrictInt, model_validator

TraceFamily = Literal["assistant", "langgraph_proxy", "rag"]
TraceStatus = Literal["running", "succeeded", "failed", "cancelled", "timeout"]
SpanStatus = Literal["running", "succeeded", "failed", "cancelled", "skipped"]
ScoreType = Literal["numeric", "categorical", "boolean", "text"]
ScorerType = Literal["human", "llm", "rule", "system"]
EvaluatorType = Literal["human", "rule", "trajectory", "span", "llm", "llm_judge", "composite", "ragas"]
TraceExportFormat = Literal["openinference", "otel", "langsmith-jsonl"]
EvalRunStatus = Literal["queued", "running", "succeeded", "failed", "cancelled"]
EvalRunMode = Literal["rescore_trace", "live_candidate"]
ScoreTargetType = Literal["trace", "span", "thread", "dataset_run", "example"]
EvalReviewStatus = Literal["pending", "approved", "rejected", "needs_fix"]
EvalGateStatus = Literal["pass", "fail", "warning"]
EvalGateMetricsSchemaVersion = Literal["eval-gate-metrics/v2"]


class AgentTraceSummary(BaseModel):
    trace_id: str
    trace_family: TraceFamily
    workflow_kind: str
    tenant_id: str
    user_id: str
    thread_id: str | None = None
    session_id: str | None = None
    run_id: str | None = None
    request_id: str | None = None
    agent_id: str | None = None
    agent_version_id: str | None = None
    agent_draft_revision: int | None = None
    publication_id: str | None = None
    channel: Literal["preview", "hosted", "embed", "api", "builtin"] | None = None
    runtime_fingerprint: str | None = None
    agent_spec_hash: str | None = None
    model_id: str | None = None
    provider: str | None = None
    status: TraceStatus
    started_at: datetime | None = None
    ended_at: datetime | None = None
    first_token_latency_ms: int = 0
    total_latency_ms: int = 0
    input_tokens: int = 0
    output_tokens: int = 0
    total_tokens: int = 0
    total_cost_cents: int = 0
    input_preview: str = ""
    output_preview: str = ""
    redaction_state: dict[str, Any] = Field(default_factory=dict)
    metadata: dict[str, Any] = Field(default_factory=dict)
    metrics: dict[str, Any] = Field(default_factory=dict)
    privacy: dict[str, Any] = Field(default_factory=dict)
    source_adapter: str | None = None
    scores_count: int = 0
    created_at: datetime | None = None
    updated_at: datetime | None = None


class AgentTraceSpan(BaseModel):
    span_id: str
    trace_id: str
    parent_span_id: str | None = None
    span_kind: str
    name: str
    status: SpanStatus
    sequence_no: int = 0
    started_at: datetime | None = None
    ended_at: datetime | None = None
    duration_ms: int = 0
    input_preview: str = ""
    output_preview: str = ""
    attributes: dict[str, Any] = Field(default_factory=dict)
    error_type: str | None = None
    error_message: str | None = None
    created_at: datetime | None = None


class AgentTraceEvent(BaseModel):
    event_id: str
    trace_id: str
    span_id: str | None = None
    event_type: str
    sequence_no: int
    occurred_at: datetime | None = None
    payload: dict[str, Any] = Field(default_factory=dict)
    payload_size_bytes: int = 0
    redacted: bool = True
    created_at: datetime | None = None


class AgentTraceScore(BaseModel):
    score_id: str
    trace_id: str
    span_id: str | None = None
    score_name: str
    score_type: ScoreType = "numeric"
    numeric_value: float | None = None
    boolean_value: bool | None = None
    categorical_value: str | None = None
    text_value: str | None = None
    label: str | None = None
    explanation: str | None = None
    scorer_type: ScorerType = "human"
    evaluator_version: str | None = None
    target_type: ScoreTargetType = "trace"
    target_id: str | None = None
    evaluator_id: str | None = None
    evaluator_name: str | None = None
    score_source: str = "human"
    confidence: float | None = None
    created_by: str
    metadata: dict[str, Any] = Field(default_factory=dict)
    created_at: datetime | None = None


class AgentTraceListResponse(BaseModel):
    traces: list[AgentTraceSummary]
    total: int
    limit: int
    offset: int


class AgentTraceDetailResponse(BaseModel):
    trace: AgentTraceSummary
    spans: list[AgentTraceSpan] = Field(default_factory=list)
    events: list[AgentTraceEvent] = Field(default_factory=list)
    scores: list[AgentTraceScore] = Field(default_factory=list)


class AgentTraceScoreCreate(BaseModel):
    span_id: str | None = None
    score_name: str = Field(..., min_length=1, max_length=96)
    score_type: ScoreType = "numeric"
    numeric_value: float | None = None
    boolean_value: bool | None = None
    categorical_value: str | None = Field(default=None, max_length=96)
    text_value: str | None = Field(default=None, max_length=2000)
    label: str | None = Field(default=None, max_length=96)
    explanation: str | None = Field(default=None, max_length=2000)
    scorer_type: ScorerType = "human"
    evaluator_version: str | None = Field(default=None, max_length=64)
    target_type: ScoreTargetType = "trace"
    target_id: str | None = Field(default=None, max_length=128)
    evaluator_id: str | None = None
    evaluator_name: str | None = Field(default=None, max_length=128)
    score_source: str = Field(default="human", max_length=32)
    confidence: float | None = Field(default=None, ge=0, le=1)
    metadata: dict[str, Any] = Field(default_factory=dict)


class TraceArtifactRef(BaseModel):
    artifact_id: str
    artifact_type: str
    name: str | None = None
    url: str | None = None
    metadata: dict[str, Any] = Field(default_factory=dict)


class TraceEnvelope(BaseModel):
    trace_id: str | None = None
    trace_family: TraceFamily = "assistant"
    workflow_kind: str = "ai_assistant_chat"
    thread_id: str | None = None
    session_id: str | None = None
    run_id: str | None = None
    request_id: str | None = None
    user_id: str | None = None
    model_id: str | None = None
    provider: str | None = None
    status: TraceStatus = "succeeded"
    started_at: datetime | None = None
    ended_at: datetime | None = None
    input_preview: str = ""
    output_preview: str = ""
    metrics: dict[str, Any] = Field(default_factory=dict)
    privacy: dict[str, Any] = Field(default_factory=dict)
    redaction_state: dict[str, Any] = Field(default_factory=dict)
    metadata: dict[str, Any] = Field(default_factory=dict)
    source_adapter: str = "api"
    spans: list[AgentTraceSpan] = Field(default_factory=list)
    events: list[AgentTraceEvent] = Field(default_factory=list)
    scores: list[AgentTraceScore] = Field(default_factory=list)
    artifacts: list[TraceArtifactRef] = Field(default_factory=list)


class AgentTraceIngestRequest(BaseModel):
    trace: TraceEnvelope
    enqueue: bool = True


class AgentTraceIngestResponse(BaseModel):
    trace_id: str
    status: str
    job_id: str | None = None


class EvalTraceExportResponse(BaseModel):
    trace_id: str
    format: TraceExportFormat
    redaction_policy: dict[str, Any] = Field(default_factory=dict)
    payload: dict[str, Any] | list[dict[str, Any]]


class EvalTraceThreadResponse(BaseModel):
    thread_id: str
    traces: list[AgentTraceSummary]
    total: int
    metrics: dict[str, Any] = Field(default_factory=dict)


class EvalDatasetCreate(BaseModel):
    model_config = ConfigDict(populate_by_name=True)

    name: str = Field(..., min_length=1, max_length=160)
    description: str = Field(default="", max_length=4000)
    version: str = Field(default="v1", min_length=1, max_length=64)
    json_schema: dict[str, Any] = Field(default_factory=dict, alias="schema")
    metadata: dict[str, Any] = Field(default_factory=dict)


class EvalDataset(BaseModel):
    model_config = ConfigDict(populate_by_name=True)

    dataset_id: str
    tenant_id: str
    name: str
    description: str = ""
    version: str = "v1"
    json_schema: dict[str, Any] = Field(default_factory=dict, alias="schema")
    metadata: dict[str, Any] = Field(default_factory=dict)
    created_by: str
    created_at: datetime | None = None
    updated_at: datetime | None = None


class EvalExampleFromTraceCreate(BaseModel):
    split: str = Field(default="regression", max_length=32)
    source_trace_id: str
    source_span_id: str | None = None
    trace_family: TraceFamily = "assistant"
    expected_output: dict[str, Any] | None = None
    metadata: dict[str, Any] = Field(default_factory=dict)


class EvalExampleImportItem(BaseModel):
    case_id: str = Field(..., min_length=1, max_length=160)
    split: str = Field(default="regression", max_length=32)
    input: dict[str, Any] = Field(default_factory=dict)
    expected_output: dict[str, Any] = Field(default_factory=dict)
    expected_trajectory: dict[str, Any] = Field(default_factory=dict)
    assertions: list[dict[str, Any]] = Field(default_factory=list)
    metadata: dict[str, Any] = Field(default_factory=dict)
    source_trace_id: str | None = None
    source_span_id: str | None = None


class EvalKbFailureSourceVersion(BaseModel):
    kb_dataset_id: str = Field(min_length=1)
    document_id: str = Field(min_length=1)
    segment_id: str = Field(min_length=1)
    source_version: StrictInt = Field(gt=0)
    source_hash: str = Field(pattern=r"^[0-9a-fA-F]{64}$")


class EvalKbFailureSave(BaseModel):
    case_id: str = Field(min_length=1, max_length=160)
    kb_dataset_id: str = Field(min_length=1)
    query: str = Field(min_length=1)
    expected_answer: str = Field(min_length=1)
    source: Literal["kb-hit-test", "kb-qa"]
    observed_segment_ids: list[str] = Field(default_factory=list)
    source_versions: list[EvalKbFailureSourceVersion] = Field(default_factory=list)
    source_trace_id: str | None = None
    kb_trace_id: str | None = None
    query_fingerprint: str | None = None
    observed_answer: str | None = None
    failure_reason: str | None = None
    expected_revision: StrictInt = Field(ge=0)


class EvalExampleUpdate(BaseModel):
    split: str | None = Field(default=None, max_length=32)
    input: dict[str, Any] | None = None
    expected_output: dict[str, Any] | None = None
    expected_trajectory: dict[str, Any] | None = None
    assertions: list[dict[str, Any]] | None = None
    tags: list[str] | None = None
    difficulty: str | None = Field(default=None, max_length=32)
    owner: str | None = Field(default=None, max_length=128)
    review_status: EvalReviewStatus | None = None
    metadata: dict[str, Any] = Field(default_factory=dict)


class EvalExample(BaseModel):
    example_id: str
    dataset_id: str
    tenant_id: str
    split: str = "regression"
    input: dict[str, Any] = Field(default_factory=dict)
    expected_output: dict[str, Any] = Field(default_factory=dict)
    metadata: dict[str, Any] = Field(default_factory=dict)
    source_trace_id: str | None = None
    source_span_id: str | None = None
    created_by: str
    created_at: datetime | None = None


class EvalKbFailureSaveResponse(BaseModel):
    example: EvalExample
    created: bool
    revision: int


class EvalExamplesImportRequest(BaseModel):
    examples: list[EvalExampleImportItem] = Field(default_factory=list, max_length=500)
    mode: Literal["skip_duplicates", "append"] = "skip_duplicates"


class EvalExamplesImportResponse(BaseModel):
    imported: int
    skipped: int = 0
    examples: list[EvalExample] = Field(default_factory=list)


class EvalExamplesExportResponse(BaseModel):
    dataset: EvalDataset
    schema_version: str = "eval-golden-v1"
    examples: list[EvalExampleImportItem] = Field(default_factory=list)


class EvalEvaluatorCreate(BaseModel):
    name: str = Field(..., min_length=1, max_length=128)
    evaluator_type: EvaluatorType = "human"
    rubric: str = Field(default="", max_length=8000)
    version: str = Field(default="v1", min_length=1, max_length=64)
    sampling_config: dict[str, Any] = Field(default_factory=dict)
    filter_config: dict[str, Any] = Field(default_factory=dict)
    metadata: dict[str, Any] = Field(default_factory=dict)


class EvalEvaluator(BaseModel):
    evaluator_id: str
    tenant_id: str
    name: str
    evaluator_type: EvaluatorType = "human"
    rubric: str = ""
    version: str = "v1"
    sampling_config: dict[str, Any] = Field(default_factory=dict)
    filter_config: dict[str, Any] = Field(default_factory=dict)
    metadata: dict[str, Any] = Field(default_factory=dict)
    created_by: str
    created_at: datetime | None = None
    updated_at: datetime | None = None


class EvalExperimentCreate(BaseModel):
    name: str = Field(..., min_length=1, max_length=160)
    description: str = Field(default="", max_length=4000)
    dataset_id: str | None = None
    target_config: dict[str, Any] = Field(default_factory=dict)
    metadata: dict[str, Any] = Field(default_factory=dict)


class EvalExperimentRun(BaseModel):
    run_id: str
    experiment_id: str | None = None
    tenant_id: str
    evaluator_id: str | None = None
    dataset_id: str | None = None
    status: EvalRunStatus = "queued"
    run_mode: EvalRunMode = "rescore_trace"
    repetitions: int = 1
    baseline_run_id: str | None = None
    dataset_manifest_hash: str | None = None
    evaluator_suite_hash: str | None = None
    candidate_fingerprint: dict[str, Any] = Field(default_factory=dict)
    target_snapshot: dict[str, Any] = Field(default_factory=dict)
    score_summary: dict[str, Any] = Field(default_factory=dict)
    metrics: dict[str, Any] = Field(default_factory=dict)
    progress: dict[str, int] = Field(default_factory=dict)
    runtime_fingerprint: dict[str, Any] = Field(default_factory=dict)
    gate_status: str | None = None
    attribution_status: str | None = None
    error_message: str | None = None
    created_by: str
    started_at: datetime | None = None
    finished_at: datetime | None = None
    created_at: datetime | None = None
    updated_at: datetime | None = None


class EvalExperimentRunResultsResponse(BaseModel):
    run: EvalExperimentRun
    cases: list[dict[str, Any]] = Field(default_factory=list)
    total: int = 0
    limit: int = 50
    offset: int = 0


class EvalExperiment(BaseModel):
    experiment_id: str
    tenant_id: str
    dataset_id: str | None = None
    name: str
    description: str = ""
    baseline_run_id: str | None = None
    baseline_promoted_by: str | None = None
    baseline_promoted_at: datetime | None = None
    target_config: dict[str, Any] = Field(default_factory=dict)
    metadata: dict[str, Any] = Field(default_factory=dict)
    created_by: str
    created_at: datetime | None = None
    updated_at: datetime | None = None
    runs: list[EvalExperimentRun] = Field(default_factory=list)


class EvalEvaluatorRunRequest(BaseModel):
    experiment_id: str | None = None
    dataset_id: str | None = None
    trace_id: str | None = None
    target_snapshot: dict[str, Any] = Field(default_factory=dict)
    metadata: dict[str, Any] = Field(default_factory=dict)


class EvalAsyncJobResponse(BaseModel):
    job_id: str
    status: EvalRunStatus = "queued"
    run_id: str | None = None


class EvalCandidateConfig(BaseModel):
    model_config = ConfigDict(extra="forbid")

    system_prompt_override: str | None = Field(default=None, max_length=16_000)
    agent_id: UUID | None = None
    agent_version_id: UUID | None = None

    @model_validator(mode="after")
    def validate_agent_version_pair(self) -> EvalCandidateConfig:
        if (self.agent_id is None) != (self.agent_version_id is None):
            raise ValueError("agent_id and agent_version_id must be provided together")
        if self.agent_id is not None and self.system_prompt_override is not None:
            raise ValueError("Agent Version candidates do not accept prompt overrides")
        return self


class EvalExperimentRunCreate(BaseModel):
    dataset_id: str | None = None
    evaluator_ids: list[str] = Field(default_factory=list, min_length=1, max_length=10)
    run_mode: EvalRunMode = "rescore_trace"
    repetitions: int | None = Field(default=None, ge=1, le=10)
    baseline_run_id: str | None = None
    candidate_config: EvalCandidateConfig = Field(default_factory=EvalCandidateConfig)
    target_snapshot: dict[str, Any] = Field(default_factory=dict)
    candidate_label: str = Field(default="candidate", max_length=96)
    baseline_label: str | None = Field(default=None, max_length=96)
    metadata: dict[str, Any] = Field(default_factory=dict)


class EvalExperimentRunBatchResponse(BaseModel):
    jobs: list[EvalAsyncJobResponse] = Field(default_factory=list)


class EvalExperimentRetryRequest(BaseModel):
    case_ids: list[str] = Field(min_length=1, max_length=100)
    acknowledge_replay: StrictBool


class EvalExperimentRunComparisonResponse(BaseModel):
    baseline_run_id: str
    candidate_run_id: str
    baseline_summary: dict[str, Any] = Field(default_factory=dict)
    candidate_summary: dict[str, Any] = Field(default_factory=dict)
    compatibility: dict[str, Any] = Field(default_factory=dict)
    changed_dimensions: list[str] = Field(default_factory=list)
    attribution: str = "unverifiable"
    deltas: dict[str, Any] = Field(default_factory=dict)
    metric_diffs: dict[str, Any] = Field(default_factory=dict)
    regression_summary: dict[str, Any] = Field(default_factory=dict)
    statistics: dict[str, Any] = Field(default_factory=dict)
    gate: dict[str, Any] = Field(default_factory=dict)
    case_diffs: list[dict[str, Any]] = Field(default_factory=list)


class EvalBaselinePromotionRequest(BaseModel):
    run_id: str


class EvalBaselinePromotionResponse(BaseModel):
    experiment_id: str
    baseline_run_id: str
    previous_baseline_run_id: str | None = None
    promoted_by: str
    promoted_at: datetime | None = None


class EvalGateMetricsV2(BaseModel):
    """Versioned exact-count receipt consumed by release gates."""

    model_config = ConfigDict(extra="allow", strict=True)

    schema_version: EvalGateMetricsSchemaVersion
    case_count: int = Field(..., ge=0)
    score_sum: float = Field(..., ge=0, allow_inf_nan=False)
    failed_case_count: int = Field(..., ge=0)
    overall_score: float = Field(..., ge=0, le=1, allow_inf_nan=False)
    pass_rate: float = Field(..., ge=0, le=1, allow_inf_nan=False)
    trajectory_case_count: int = Field(..., ge=0)
    trajectory_failed_count: int = Field(..., ge=0)
    trajectory_pass_rate: float = Field(..., ge=0, le=1, allow_inf_nan=False)
    critical_case_count: int = Field(..., ge=0)
    critical_failed_count: int = Field(..., ge=0)
    critical_pass_rate: float | None = Field(default=None, ge=0, le=1, allow_inf_nan=False)
    stateful_case_count: int = Field(..., ge=0)
    stateful_failed_count: int = Field(..., ge=0)
    stateful_pass_rate: float | None = Field(default=None, ge=0, le=1, allow_inf_nan=False)

    @model_validator(mode="after")
    def validate_exact_count_subsets(self) -> EvalGateMetricsV2:
        if self.trajectory_case_count != self.case_count:
            raise ValueError("trajectory_case_count must equal case_count")
        if self.critical_case_count > self.case_count:
            raise ValueError("critical_case_count must not exceed case_count")
        if self.stateful_case_count > self.case_count:
            raise ValueError("stateful_case_count must not exceed case_count")
        for label, value in (
            ("trajectory_failed_count", self.trajectory_failed_count),
            ("critical_failed_count", self.critical_failed_count),
            ("stateful_failed_count", self.stateful_failed_count),
        ):
            if value > self.failed_case_count:
                raise ValueError(f"{label} must not exceed failed_case_count")
        return self


class EvalGateDryRunRequest(BaseModel):
    baseline_run_id: str | None = None
    candidate_run_id: str | None = None
    result_payload: dict[str, Any] = Field(default_factory=dict)
    thresholds: dict[str, float] = Field(default_factory=dict)


class EvalGateDryRunResponse(BaseModel):
    status: EvalGateStatus
    thresholds: dict[str, float] = Field(default_factory=dict)
    metrics: dict[str, Any] = Field(default_factory=dict)
    failures: list[str] = Field(default_factory=list)
    skipped_thresholds: list[str] = Field(default_factory=list)
    coverage: dict[str, Any] = Field(default_factory=dict)
    compatibility: dict[str, Any] = Field(default_factory=dict)
    authoritative_gate: dict[str, Any] = Field(default_factory=dict)
    report: dict[str, Any] = Field(default_factory=dict)


class EvalTraceFeedbackRequest(BaseModel):
    trace_ids: list[str] = Field(..., min_length=1, max_length=50)
    trace_family: TraceFamily = "assistant"
    split: str = Field(default="regression", max_length=32)
    dataset_id: str | None = None
    proposed_by: str = Field(default="eval-feedback-api", max_length=128)
    low_score_threshold: float = Field(default=0.75, ge=0, le=1)
    latency_threshold_ms: int = Field(default=30_000, ge=1)


class EvalTraceFailurePattern(BaseModel):
    trace_id: str
    trace_family: TraceFamily
    failure_mode: str
    reasons: list[str] = Field(default_factory=list)
    severity: str = "medium"


class EvalTraceFeedbackResponse(BaseModel):
    trace_family: TraceFamily
    dataset_id: str | None = None
    patterns: list[EvalTraceFailurePattern] = Field(default_factory=list)
    clusters: list[dict[str, Any]] = Field(default_factory=list)
    dataset_cases: list[EvalExampleImportItem] = Field(default_factory=list)
    import_request: EvalExamplesImportRequest | None = None
    proposals: list[dict[str, Any]] = Field(default_factory=list)
    redaction_policy: dict[str, Any] = Field(default_factory=dict)


class EvalDatasetListResponse(BaseModel):
    datasets: list[EvalDataset]
    total: int
    limit: int
    offset: int


class EvalExampleListResponse(BaseModel):
    examples: list[EvalExample]
    total: int
    limit: int
    offset: int


class EvalEvaluatorListResponse(BaseModel):
    evaluators: list[EvalEvaluator]
    total: int
    limit: int
    offset: int


class EvalExperimentListResponse(BaseModel):
    experiments: list[EvalExperiment]
    total: int
    limit: int
    offset: int


class EvalTraceMonitoringSummary(BaseModel):
    total_traces: int = 0
    failed_traces: int = 0
    succeeded_traces: int = 0
    assistant_traces: int = 0
    langgraph_traces: int = 0
    rag_traces: int = 0
    avg_latency_ms: int = 0
    p95_latency_ms: int = 0
    total_tokens: int = 0
    total_cost_cents: int = 0
    scored_traces: int = 0
    window_days: int = 7


class EvalDashboardResponse(BaseModel):
    metrics: dict[str, Any] = Field(default_factory=dict)
    run_health: dict[str, Any] = Field(default_factory=dict)
    queue_health: dict[str, Any] = Field(default_factory=dict)
    runtime_health: dict[str, Any] = Field(default_factory=dict)
    latest_gate_status: dict[str, Any] = Field(default_factory=dict)


class KbRagasMetricSummary(BaseModel):
    metric: str
    average_score: float = 0.0
    scored_count: int = 0
    pass_count: int = 0
    fail_count: int = 0
    review_count: int = 0


class KbRagasKnowledgeSummaryResponse(BaseModel):
    window_days: int = 7
    dataset_id: str | None = None
    rag_traces: int = 0
    ragas_scored_traces: int = 0
    metrics: list[KbRagasMetricSummary] = Field(default_factory=list)
    latest_judge_model: str | None = None


class KbRagasBatchScoreRequest(BaseModel):
    evaluator_id: str
    limit: int = Field(default=50, ge=1, le=200)
    only_unscored: bool = True


class KbRagasBatchScoreResponse(BaseModel):
    queued: int = 0
    skipped: int = 0
    jobs: list[EvalAsyncJobResponse] = Field(default_factory=list)


class KbRagasJudgeSelector(BaseModel):
    """Safe caller selector; endpoints and credentials are server-owned."""

    model_config = ConfigDict(extra="forbid")

    provider: str | None = Field(default=None, min_length=1, max_length=32)
    model: str | None = Field(default=None, min_length=1, max_length=128)


class KbRagasScoreRetrievalRequest(BaseModel):
    query: str = Field(..., min_length=1, max_length=4000)
    contexts: list[str] = Field(..., min_length=1, max_length=32)
    answer: str | None = Field(default=None, max_length=8000)
    metrics: list[str] | None = None
    ground_truth: str | None = Field(default=None, max_length=8000)
    dataset_id: str | None = None
    llm_config: KbRagasJudgeSelector | None = None


class KbRagasScoreRetrievalResult(BaseModel):
    metric: str
    score: float
    explanation: str
    label: str


class KbRagasScoreRetrievalResponse(BaseModel):
    results: list[KbRagasScoreRetrievalResult] = Field(default_factory=list)
    judge_model: str = ""
