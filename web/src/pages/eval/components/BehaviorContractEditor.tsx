import { Alert, App as AntApp, Button, Checkbox, Input, InputNumber, Select } from "antd";
import { FileCheck2, Plus, RotateCcw, Trash2, WandSparkles } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  importEvalExamples,
  updateEvalExample,
  type AgentTraceDetailResponse,
  type EvalExample,
  type EvalExampleImportItem,
  type EvalReviewStatus,
} from "@/api/eval";

type ContractDraft = {
  caseId: string;
  message: string;
  reference: string;
  rubric: string;
  contains: string;
  notContains: string;
  tools: ToolDraft[];
  requiredSpans: string;
  exitReason: string;
  latencyLimit?: number;
  tokenLimit?: number;
  costLimit?: number;
  critical: boolean;
  owner: string;
  tags: string;
  difficulty: string;
  reviewStatus: EvalReviewStatus;
  confirmed: boolean;
  sourceTraceId?: string;
};

type ToolDraft = {
  name: string;
  mode: "required" | "forbidden";
  argumentsText: string;
  maxCalls?: number;
  status: string;
};

const EMPTY_DRAFT: ContractDraft = {
  caseId: "",
  message: "",
  reference: "",
  rubric: "",
  contains: "",
  notContains: "",
  tools: [],
  requiredSpans: "lifecycle, model_invocation",
  exitReason: "succeeded",
  critical: false,
  owner: "",
  tags: "",
  difficulty: "medium",
  reviewStatus: "pending",
  confirmed: false,
};

function list(value: string): string[] {
  return value.split(/[,\n]/).map((item) => item.trim()).filter(Boolean);
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function strings(value: unknown): string[] {
  if (Array.isArray(value)) return value.filter((item): item is string => typeof item === "string");
  return typeof value === "string" && value ? [value] : [];
}

function argumentSubset(text: string): Record<string, unknown> {
  const parsed: unknown = JSON.parse(text);
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("Tool argument subset must be a JSON object");
  }
  return parsed as Record<string, unknown>;
}

function exampleCaseId(example: EvalExample): string {
  return example.case_id || String(example.metadata?.case_id || example.example_id);
}

function assertionValue(assertions: Array<Record<string, unknown>>, type: string): number | undefined {
  const value = assertions.find((item) => item.type === type)?.value;
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function draftFromExample(example: EvalExample): ContractDraft {
  const metadata = record(example.metadata);
  const trajectory = record(example.expected_trajectory || metadata.expected_trajectory);
  const runtime = record(trajectory.runtime);
  const assertions = Array.isArray(example.assertions)
    ? example.assertions
    : Array.isArray(metadata.assertions) ? metadata.assertions as Array<Record<string, unknown>> : [];
  const tools = Array.isArray(trajectory.tools) ? trajectory.tools.map(record) : [];
  const draftedTools: ToolDraft[] = tools.map((tool) => ({
    name: String(tool.name || ""),
    mode: (tool.forbidden === true ? "forbidden" : "required") as "forbidden" | "required",
    argumentsText: Object.keys(record(tool.arguments_subset)).length ? JSON.stringify(tool.arguments_subset, null, 2) : "",
    maxCalls: typeof tool.max_calls === "number" ? tool.max_calls : undefined,
    status: String(tool.status || ""),
  })).filter((tool) => tool.name);
  for (const assertion of assertions) {
    const mode = assertion.type === "tool_not_called" ? "forbidden" : assertion.type === "tool_called" ? "required" : null;
    const name = String(assertion.value || "");
    if (mode && name && !draftedTools.some((tool) => tool.name === name)) {
      draftedTools.push({ name, mode, argumentsText: "", status: "" });
    }
  }

  return {
    caseId: exampleCaseId(example),
    message: String(example.input.message || example.input.input_preview || ""),
    reference: String(example.expected_output.reference || example.expected_output.output_preview || ""),
    rubric: String(example.expected_output.rubric || ""),
    contains: strings(example.expected_output.contains).join(", "),
    notContains: strings(example.expected_output.not_contains).join(", "),
    tools: draftedTools,
    requiredSpans: strings(trajectory.required_span_kinds).join(", "),
    exitReason: String(runtime.expected_exit_reason || "succeeded"),
    latencyLimit: assertionValue(assertions, "latency_ms_lt"),
    tokenLimit: assertionValue(assertions, "total_tokens_lt"),
    costLimit: assertionValue(assertions, "cost_cents_lt"),
    critical: metadata.critical === true,
    owner: String(metadata.owner || ""),
    tags: strings(metadata.tags).join(", "),
    difficulty: String(metadata.difficulty || "medium"),
    reviewStatus: (metadata.review_status || "pending") as EvalReviewStatus,
    confirmed: metadata.behavior_confirmed === true || metadata.review_status === "approved",
    sourceTraceId: example.source_trace_id || undefined,
  };
}

function toContract(draft: ContractDraft): EvalExampleImportItem {
  const tools = draft.tools.filter((tool) => tool.name.trim()).map((tool, index) => ({
    name: tool.name.trim(),
    required: tool.mode === "required",
    forbidden: tool.mode === "forbidden",
    order: index + 1,
    ...(tool.argumentsText.trim() ? { arguments_subset: argumentSubset(tool.argumentsText) } : {}),
    ...(tool.maxCalls !== undefined ? { max_calls: tool.maxCalls } : {}),
    ...(tool.status.trim() ? { status: tool.status.trim() } : {}),
  }));
  const requiredTools = tools.filter((tool) => tool.required).map((tool) => tool.name);
  const forbiddenTools = tools.filter((tool) => tool.forbidden).map((tool) => tool.name);
  const requiredText = list(draft.contains);
  const forbiddenText = list(draft.notContains);
  const assertions: Array<Record<string, unknown>> = [
    { type: "no_sensitive_output" },
    ...requiredText.map((value) => ({ type: "output_contains", value })),
    ...forbiddenText.map((value) => ({ type: "output_not_contains", value })),
    ...requiredTools.map((value) => ({ type: "tool_called", value })),
    ...forbiddenTools.map((value) => ({ type: "tool_not_called", value })),
  ];
  if (draft.latencyLimit !== undefined) assertions.push({ type: "latency_ms_lt", value: draft.latencyLimit });
  if (draft.tokenLimit !== undefined) assertions.push({ type: "total_tokens_lt", value: draft.tokenLimit });
  if (draft.costLimit !== undefined) assertions.push({ type: "cost_cents_lt", value: draft.costLimit });

  return {
    case_id: draft.caseId.trim(),
    split: "regression",
    input: { message: draft.message.trim() },
    expected_output: {
      ...(draft.reference.trim() ? { reference: draft.reference.trim() } : {}),
      ...(draft.rubric.trim() ? { rubric: draft.rubric.trim() } : {}),
      ...(requiredText.length ? { contains: requiredText } : {}),
      ...(forbiddenText.length ? { not_contains: forbiddenText } : {}),
    },
    expected_trajectory: {
      required_span_kinds: list(draft.requiredSpans),
      tools,
      runtime: { expected_exit_reason: draft.exitReason.trim() || "succeeded" },
    },
    assertions,
    metadata: {
      critical: draft.critical,
      owner: draft.owner.trim() || undefined,
      tags: list(draft.tags),
      difficulty: draft.difficulty,
      review_status: draft.reviewStatus,
      behavior_confirmed: draft.confirmed,
    },
    source_trace_id: draft.sourceTraceId || null,
  };
}

export function BehaviorContractEditor({
  datasetId,
  examples,
  selectedTrace,
  prefillRevision = 0,
  readOnly,
  onSaved,
}: {
  datasetId: string | null;
  examples: EvalExample[];
  selectedTrace?: AgentTraceDetailResponse;
  prefillRevision?: number;
  readOnly: boolean;
  onSaved: () => void | Promise<void>;
}) {
  const { t } = useTranslation();
  const { message } = AntApp.useApp();
  const [selectedExampleId, setSelectedExampleId] = useState<string>();
  const [draft, setDraft] = useState<ContractDraft>(EMPTY_DRAFT);
  const [saving, setSaving] = useState(false);
  const handledPrefillRevision = useRef(0);
  const selectedExample = useMemo(
    () => examples.find((example) => example.example_id === selectedExampleId),
    [examples, selectedExampleId],
  );
  const validationError = useMemo(() => {
    if (!draft.caseId.trim()) return t("eval.behavior.caseIdRequired");
    if (!draft.message.trim()) return t("eval.behavior.messageRequired");
    const incompleteTool = draft.tools.find((tool) => !tool.name.trim());
    if (incompleteTool) return t("eval.behavior.toolNameRequired");
    for (const tool of draft.tools) {
      if (!tool.argumentsText.trim()) continue;
      try {
        argumentSubset(tool.argumentsText);
      } catch {
        return t("eval.behavior.invalidToolArguments", { tool: tool.name });
      }
    }
    if (draft.latencyLimit !== undefined && draft.latencyLimit <= 0) {
      return t("eval.behavior.positiveLatencyRequired");
    }
    if (draft.tokenLimit !== undefined && draft.tokenLimit <= 0) {
      return t("eval.behavior.positiveTokensRequired");
    }
    if (draft.costLimit !== undefined && draft.costLimit <= 0) {
      return t("eval.behavior.positiveCostRequired");
    }
    if (!draft.confirmed) return t("eval.behavior.confirmHint");
    return null;
  }, [draft, t]);
  const invalid = !datasetId || validationError !== null;

  function reset() {
    setSelectedExampleId(undefined);
    setDraft(EMPTY_DRAFT);
  }

  const prefillFromTrace = useCallback(() => {
    const trace = selectedTrace?.trace;
    if (!trace) return;
    const tools = selectedTrace.spans
      .filter((span) => span.span_kind === "tool_execution")
      .map((span): ToolDraft => {
        let argumentsText = "";
        try {
          const parsed: unknown = JSON.parse(span.input_preview);
          if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
            argumentsText = JSON.stringify(parsed, null, 2);
          }
        } catch {
          // Redacted or truncated previews are intentionally left for manual entry.
        }
        return {
          name: span.name.replace(/^tool:/, ""),
          mode: "required",
          argumentsText,
          status: span.status,
        };
      })
      .filter((tool) => tool.name);
    const spanKinds = selectedTrace.spans
      .map((span) => span.span_kind)
      .filter((kind, index, kinds) => kind && kinds.indexOf(kind) === index);
    setSelectedExampleId(undefined);
    setDraft({
      ...EMPTY_DRAFT,
      caseId: `assistant.${trace.trace_id}`,
      message: trace.input_preview,
      tools,
      requiredSpans: spanKinds.join(", "),
      critical: trace.status === "failed",
      reviewStatus: "needs_fix",
      confirmed: false,
      sourceTraceId: trace.trace_id,
    });
  }, [selectedTrace]);

  useEffect(() => {
    if (
      prefillRevision <= 0
      || prefillRevision === handledPrefillRevision.current
      || !selectedTrace?.trace
    ) return;
    handledPrefillRevision.current = prefillRevision;
    prefillFromTrace();
  }, [prefillFromTrace, prefillRevision, selectedTrace?.trace]);

  async function save() {
    if (!datasetId || invalid) return;
    setSaving(true);
    try {
      const contract = toContract(draft);
      if (selectedExample) {
        await updateEvalExample(datasetId, selectedExample.example_id, {
          split: contract.split,
          input: contract.input,
          expected_output: contract.expected_output,
          expected_trajectory: contract.expected_trajectory,
          assertions: contract.assertions,
          review_status: draft.reviewStatus,
          metadata: contract.metadata,
        });
      } else {
        const result = await importEvalExamples(datasetId, [contract], { mode: "skip_duplicates" });
        if (result.imported === 0) throw new Error(t("eval.behavior.duplicateCase"));
      }
      message.success(t("eval.behavior.saved"));
      await onSaved();
    } catch (error) {
      message.error(error instanceof Error ? error.message : String(error));
    } finally {
      setSaving(false);
    }
  }

  return (
    <section className="eval-behavior-editor" data-testid="behavior-contract-editor">
      <div className="eval-golden-import-heading">
        <div>
          <h3>{t("eval.behavior.title")}</h3>
          <p>{t("eval.behavior.description")}</p>
        </div>
        <FileCheck2 size={18} />
      </div>

      <div className="eval-behavior-toolbar">
        <label className="eval-field">
          <span>{t("eval.behavior.existingCase")}</span>
          <Select
            allowClear
            value={selectedExampleId}
            placeholder={t("eval.behavior.newCase")}
            options={examples.map((example) => ({ label: exampleCaseId(example), value: example.example_id }))}
            onChange={(value) => {
              setSelectedExampleId(value);
              const example = examples.find((item) => item.example_id === value);
              setDraft(example ? draftFromExample(example) : EMPTY_DRAFT);
            }}
          />
        </label>
        <Button icon={<WandSparkles size={15} />} disabled={!selectedTrace || readOnly} onClick={prefillFromTrace}>
          {t("eval.behavior.prefillTrace")}
        </Button>
        <Button icon={<RotateCcw size={15} />} onClick={reset}>{t("eval.behavior.newCase")}</Button>
      </div>

      {!datasetId ? <Alert type="warning" showIcon title={t("eval.goldenImport.datasetRequired")} /> : null}
      {validationError ? (
        <Alert
          type={!draft.confirmed && validationError === t("eval.behavior.confirmHint") ? "warning" : "error"}
          showIcon
          title={validationError}
        />
      ) : null}

      <div className="eval-behavior-sections">
        <fieldset>
          <legend>{t("eval.behavior.input")}</legend>
          <label className="eval-field"><span>{t("eval.behavior.caseId")}</span><Input value={draft.caseId} disabled={Boolean(selectedExample)} onChange={(event) => setDraft({ ...draft, caseId: event.target.value })} /></label>
          <label className="eval-field eval-field-wide"><span>{t("eval.behavior.message")}</span><Input.TextArea autoSize={{ minRows: 2, maxRows: 5 }} value={draft.message} onChange={(event) => setDraft({ ...draft, message: event.target.value })} /></label>
        </fieldset>
        <fieldset>
          <legend>{t("eval.behavior.expectedAnswer")}</legend>
          <label className="eval-field"><span>{t("eval.behavior.reference")}</span><Input.TextArea autoSize={{ minRows: 2, maxRows: 4 }} value={draft.reference} onChange={(event) => setDraft({ ...draft, reference: event.target.value })} /></label>
          <label className="eval-field"><span>{t("eval.behavior.rubric")}</span><Input.TextArea autoSize={{ minRows: 2, maxRows: 4 }} value={draft.rubric} onChange={(event) => setDraft({ ...draft, rubric: event.target.value })} /></label>
          <label className="eval-field"><span>{t("eval.behavior.contains")}</span><Input value={draft.contains} onChange={(event) => setDraft({ ...draft, contains: event.target.value })} /></label>
          <label className="eval-field"><span>{t("eval.behavior.notContains")}</span><Input value={draft.notContains} onChange={(event) => setDraft({ ...draft, notContains: event.target.value })} /></label>
        </fieldset>
        <fieldset>
          <legend>{t("eval.behavior.expectedTools")}</legend>
          <div className="eval-tool-expectations">
            {draft.tools.map((tool, index) => (
              <div className="eval-tool-expectation" key={`${index}:${tool.name}`}>
                <Input aria-label={t("eval.behavior.toolName")} placeholder={t("eval.behavior.toolName")} value={tool.name} onChange={(event) => setDraft({ ...draft, tools: draft.tools.map((item, itemIndex) => itemIndex === index ? { ...item, name: event.target.value } : item) })} />
                <Select aria-label={t("eval.behavior.toolMode")} value={tool.mode} options={[{ label: t("eval.behavior.required"), value: "required" }, { label: t("eval.behavior.forbidden"), value: "forbidden" }]} onChange={(value) => setDraft({ ...draft, tools: draft.tools.map((item, itemIndex) => itemIndex === index ? { ...item, mode: value } : item) })} />
                <Input aria-label={t("eval.behavior.toolArguments")} placeholder={t("eval.behavior.toolArguments")} value={tool.argumentsText} onChange={(event) => setDraft({ ...draft, tools: draft.tools.map((item, itemIndex) => itemIndex === index ? { ...item, argumentsText: event.target.value } : item) })} />
                <InputNumber aria-label={t("eval.behavior.maxCalls")} placeholder={t("eval.behavior.maxCalls")} min={0} value={tool.maxCalls} onChange={(value) => setDraft({ ...draft, tools: draft.tools.map((item, itemIndex) => itemIndex === index ? { ...item, maxCalls: value ?? undefined } : item) })} />
                <Input aria-label={t("eval.behavior.toolStatus")} placeholder={t("eval.behavior.toolStatus")} value={tool.status} onChange={(event) => setDraft({ ...draft, tools: draft.tools.map((item, itemIndex) => itemIndex === index ? { ...item, status: event.target.value } : item) })} />
                <Button aria-label={t("eval.behavior.removeTool")} icon={<Trash2 size={14} />} onClick={() => setDraft({ ...draft, tools: draft.tools.filter((_, itemIndex) => itemIndex !== index) })} />
              </div>
            ))}
            <Button icon={<Plus size={14} />} onClick={() => setDraft({ ...draft, tools: [...draft.tools, { name: "", mode: "required", argumentsText: "", status: "" }] })}>{t("eval.behavior.addTool")}</Button>
          </div>
          <label className="eval-field"><span>{t("eval.behavior.requiredSpans")}</span><Input value={draft.requiredSpans} onChange={(event) => setDraft({ ...draft, requiredSpans: event.target.value })} /></label>
          <label className="eval-field"><span>{t("eval.behavior.exitReason")}</span><Input value={draft.exitReason} onChange={(event) => setDraft({ ...draft, exitReason: event.target.value })} /></label>
        </fieldset>
        <fieldset>
          <legend>{t("eval.behavior.constraints")}</legend>
          <label className="eval-field"><span>{t("eval.behavior.maxLatency")}</span><InputNumber min={1} value={draft.latencyLimit} onChange={(value) => setDraft({ ...draft, latencyLimit: value ?? undefined })} /></label>
          <label className="eval-field"><span>{t("eval.behavior.maxTokens")}</span><InputNumber min={1} value={draft.tokenLimit} onChange={(value) => setDraft({ ...draft, tokenLimit: value ?? undefined })} /></label>
          <label className="eval-field"><span>{t("eval.behavior.maxCost")}</span><InputNumber min={0.01} step={0.01} value={draft.costLimit} onChange={(value) => setDraft({ ...draft, costLimit: value ?? undefined })} /></label>
          <label className="eval-field"><span>{t("eval.behavior.owner")}</span><Input value={draft.owner} onChange={(event) => setDraft({ ...draft, owner: event.target.value })} /></label>
          <label className="eval-field"><span>{t("eval.behavior.tags")}</span><Input value={draft.tags} onChange={(event) => setDraft({ ...draft, tags: event.target.value })} /></label>
          <label className="eval-field"><span>{t("eval.behavior.difficulty")}</span><Select value={draft.difficulty} options={["easy", "medium", "hard"].map((value) => ({ label: value, value }))} onChange={(value) => setDraft({ ...draft, difficulty: value })} /></label>
          <label className="eval-field"><span>{t("eval.behavior.reviewStatus")}</span><Select value={draft.reviewStatus} options={["pending", "approved", "rejected", "needs_fix"].map((value) => ({ label: value, value }))} onChange={(value: EvalReviewStatus) => setDraft({ ...draft, reviewStatus: value })} /></label>
          <div className="eval-behavior-checks">
            <Checkbox checked={draft.critical} onChange={(event) => setDraft({ ...draft, critical: event.target.checked })}>{t("eval.behavior.critical")}</Checkbox>
            <Checkbox checked={draft.confirmed} onChange={(event) => setDraft({ ...draft, confirmed: event.target.checked })}>{t("eval.behavior.confirmed")}</Checkbox>
          </div>
        </fieldset>
      </div>

      <Button type="primary" icon={<FileCheck2 size={15} />} loading={saving} disabled={readOnly || invalid} onClick={() => void save()}>
        {selectedExample ? t("eval.behavior.update") : t("eval.behavior.save")}
      </Button>
    </section>
  );
}
