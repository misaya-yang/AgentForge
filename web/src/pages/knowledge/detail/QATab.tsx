/**
 * QA chat tab of the dataset detail page.
 *
 * Retrieval parameters (topK / mode / weights / rerank / mmr) come from the
 * shared useHitTestConsole bundle, so edits here stay visible in the
 * Retrieval tab and vice versa — and runQA sends whatever the console holds.
 *
 * Extracted verbatim from DatasetDetail.tsx (C2 split); no behavior change.
 * The component stays mounted while other tabs are visible (the shell hides
 * it with `hidden`) so chat state survives tab switches exactly as before.
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Sliders,
  MessageSquare,
  Sparkles,
  Clock,
  User,
  Bot,
  Loader2,
  Send,
  Zap,
  Brain,
  Database,
} from "lucide-react";

import { qaQueryStream } from "@/api/knowledge";
import { SaveKnowledgeFailureDialog } from "@/pages/knowledge/detail/SaveKnowledgeFailureDialog";
import { QA_EVAL_SOURCE } from "@/pages/knowledge/detail/evalCaseStore";
import type { LLMConfig, QAResponse } from "@/types/knowledge";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { StreamOutput } from "@/components/StreamOutput";
import type { HitTestConsole } from "@/pages/knowledge/detail/useHitTestConsole";
import { KnowledgeSourceButton } from "./KnowledgeSourceButton";
import { KnowledgeFeedbackControl } from "@/pages/knowledge/detail/KnowledgeFeedbackControl";

import { finishQAStream, qaStreamEvent, type QAStreamOutcome } from "./qaStreamOutcome";

type QAChatMessage = {
  id: string;
  role: "user" | "assistant";
  content: string;
  status: "pending" | "done" | "error";
  query?: string;
  datasetId?: string;
  response?: QAResponse;
};

// Empty value → the server applies its deployment default.
const QA_MODEL_OPTIONS: { value: string; labelKey: string; provider: LLMConfig["provider"] }[] = [
  { value: "", labelKey: "common.serverDefault", provider: "dashscope" },
];

const QA_SYSTEM_PROMPT_KEYS = {
  strict: "knowledge.detail.qaStrictPrompt",
  flexible: "knowledge.detail.qaFlexiblePrompt",
};

interface QATabProps {
  datasetId?: string;
  hitTest: HitTestConsole;
  identityScope?: { isCurrent: () => boolean };
}

export function QATab({ datasetId, hitTest, identityScope }: QATabProps) {
  const { t } = useTranslation();
  const qaChatEndRef = useRef<HTMLDivElement | null>(null);
  const {
    topK,
    setTopK,
    mode,
    setMode,
    denseWeight,
    setDenseWeight,
    bm25Weight,
    setBm25Weight,
    fusionMethod,
    setFusionMethod,
    rerank,
    setRerank,
    mmr,
    setMmr,
    markRetrievalConfigCustom,
  } = hitTest;

  const [qaQueryInput, setQaQueryInput] = useState("");
  const [qaLoading, setQaLoading] = useState(false);
  const [storedMessages, setQaMessages] = useState<QAChatMessage[]>([]);
  const [storedHistory, setQaHistory] = useState<Array<{ query: string; response: QAResponse }>>([]);
  const [qaModel, setQaModel] = useState(QA_MODEL_OPTIONS[0].value);
  const [qaTemperature, setQaTemperature] = useState(0.1);
  const [qaMaxTokens, setQaMaxTokens] = useState(2048);
  const [qaShowSources, setQaShowSources] = useState(true);
  const [qaAutoScroll, setQaAutoScroll] = useState(true);
  const [qaStrictMode, setQaStrictMode] = useState(false);
  const activeScope = useRef({ datasetId, identityScope });
  activeScope.current = { datasetId, identityScope };
  const [viewScope, setViewScope] = useState({ datasetId, identityScope });
  const currentView = viewScope.datasetId === datasetId && viewScope.identityScope === identityScope
    && (identityScope?.isCurrent() ?? true);
  const qaMessages = currentView ? storedMessages : [];
  const qaHistory = currentView ? storedHistory : [];
  const activeRequest = useRef<AbortController | null>(null);
  const requestSequence = useRef(0);
  useEffect(() => {
    setViewScope({ datasetId, identityScope });
    setQaMessages([]); setQaHistory([]); setQaQueryInput(""); setQaLoading(false);
    return () => { activeRequest.current?.abort(); activeRequest.current = null; };
  }, [datasetId, identityScope]);

  useEffect(() => {
    if (!qaAutoScroll) return;
    qaChatEndRef.current?.scrollIntoView({ behavior: "smooth", block: "end" });
  }, [qaMessages, qaLoading, qaAutoScroll]);

  async function runQA() {
    if (!datasetId || !qaQueryInput.trim() || activeRequest.current || !currentView
      || !(identityScope?.isCurrent() ?? true)) return;
    const controller = new AbortController();
    activeRequest.current = controller;
    const current = () => !controller.signal.aborted && activeRequest.current === controller
      && activeScope.current.datasetId === datasetId && activeScope.current.identityScope === identityScope
      && (identityScope?.isCurrent() ?? true);
    const sequence = ++requestSequence.current;
    const queryText = qaQueryInput.trim();
    const userMessageId = `user-${sequence}`;
    const assistantMessageId = `assistant-${sequence}`;
    const requestPayload = {
      query: queryText,
      top_k: topK,
      mode,
      fusion_method: mode === "hybrid" ? fusionMethod : undefined,
      dense_weight: mode === "hybrid" ? denseWeight : undefined,
      bm25_weight: mode === "hybrid" ? bm25Weight : undefined,
      rerank,
      mmr,
      llm_config: {
        provider: QA_MODEL_OPTIONS.find((o) => o.value === qaModel)?.provider ?? "dashscope",
        model: qaModel,
        temperature: qaTemperature,
        max_tokens: qaMaxTokens,
        system_prompt: qaSystemPrompt,
      },
      include_raw_results: true,
    };

    setQaQueryInput("");
    setQaLoading(true);
    setQaMessages((prev) => [
      ...prev,
      { id: userMessageId, role: "user", content: queryText, status: "done" },
      { id: assistantMessageId, role: "assistant", content: "", status: "pending", query: queryText, datasetId },
    ]);
    const updateAssistant = (patch: Partial<QAChatMessage>) => {
      if (!current()) return;
      setQaMessages((prev) =>
        prev.map((msg) => (msg.id === assistantMessageId ? { ...msg, ...patch } : msg))
      );
    };

    let outcome: QAStreamOutcome = { phase: "receiving", text: "" };
    try {
      try {
        for await (const event of qaQueryStream(datasetId, requestPayload, controller.signal)) {
          if (!current()) return;
          outcome = qaStreamEvent(outcome, event, queryText);
          updateAssistant({ content: outcome.text });
          if (outcome.phase !== "receiving") break;
        }
      } catch {
        // A dispatched request may have executed. Never silently replay via /qa.
        outcome = finishQAStream(outcome);
      }
      outcome = finishQAStream(outcome);
      if (!current()) return;
      if (outcome.phase === "completed") {
        const response = outcome.response;
        updateAssistant({ content: response.answer, response, status: "done" });
        setQaHistory(previous => [...previous, { query: queryText, response }]);
      } else {
        updateAssistant({ content: outcome.text, status: "error" });
      }
    } finally {
      if (current()) setQaLoading(false);
      if (activeRequest.current === controller) activeRequest.current = null;
    }
  }

  function handleQaKeyDown(event: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      if (!qaLoading) runQA();
    }
  }

  function handleClearQaChat() {
    setQaMessages([]);
  }

  const lastQaResponse = useMemo(() => {
    for (let i = qaMessages.length - 1; i >= 0; i -= 1) {
      const msg = qaMessages[i];
      if (msg.response) return msg.response;
    }
    return null;
  }, [qaMessages]);

  const qaTurns = useMemo(
    () => qaMessages.filter((msg) => msg.role === "assistant").length,
    [qaMessages]
  );
  const qaSystemPrompt = useMemo(
    () => t(qaStrictMode ? QA_SYSTEM_PROMPT_KEYS.strict : QA_SYSTEM_PROMPT_KEYS.flexible),
    [qaStrictMode, t]
  );

  return (
    <div className="grid grid-cols-1 gap-6 lg:grid-cols-12">
      {/* 左侧：配置 */}
      <div className="space-y-6 lg:col-span-4">
        <Card className="p-0 overflow-hidden shadow-xs border-border">
          <div className="px-5 py-4 bg-linear-to-r from-muted/70 via-card to-primary/10 border-b border-border/60">
            <h3 className="font-bold text-foreground flex items-center gap-2">
              <div className="w-8 h-8 rounded-lg bg-primary/10 border border-primary/20 flex items-center justify-center">
                <Sliders className="h-4 w-4 text-primary" />
              </div>
              {t("knowledge.detail.qaConfig")}
            </h3>
            <p className="text-xs text-muted-foreground mt-1">{t("knowledge.detail.qaConfigHint")}</p>
          </div>

          <div className="p-5 space-y-6">
            <div className="space-y-3">
              <div className="flex items-center justify-between">
                <Label className="text-sm font-medium text-foreground/80">{t("knowledge.detail.qaModel")}</Label>
                <Badge variant="outline" className="text-xs">{(QA_MODEL_OPTIONS.find((o) => o.value === qaModel)?.provider ?? "dashscope").toUpperCase()}</Badge>
              </div>
              <Select value={qaModel} onValueChange={setQaModel}>
                <SelectTrigger className="border-border">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {QA_MODEL_OPTIONS.map((option) => (
                    <SelectItem key={option.value} value={option.value}>
                      {option.labelKey ? t(option.labelKey) : option.value}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <div className="grid grid-cols-2 gap-3">
                <div>
                  <Label className="text-xs text-muted-foreground">{t("knowledge.detail.qaTemperature")}</Label>
                  <Input
                    type="number"
                    step={0.1}
                    min={0}
                    max={1}
                    value={qaTemperature}
                    onChange={(e) => {
                      const value = e.target.valueAsNumber;
                      setQaTemperature((prev) => (Number.isNaN(value) ? prev : value));
                    }}
                    className="mt-1.5 border-border"
                  />
                </div>
                <div>
                  <Label className="text-xs text-muted-foreground">Max Tokens</Label>
                  <Input
                    type="number"
                    min={256}
                    max={4096}
                    step={128}
                    value={qaMaxTokens}
                    onChange={(e) => {
                      const value = e.target.valueAsNumber;
                      setQaMaxTokens((prev) => (Number.isNaN(value) ? prev : value));
                    }}
                    className="mt-1.5 border-border"
                  />
                </div>
              </div>
            </div>

            <div className="space-y-4">
              <div className="flex items-center justify-between">
                <Label className="text-sm font-medium text-foreground/80">{t("knowledge.detail.qaRetrievalSettings")}</Label>
                <Badge variant="outline" className="text-xs font-mono">{mode}</Badge>
              </div>
              <div className="grid grid-cols-2 gap-4">
                <div>
                  <Label className="text-xs font-medium text-muted-foreground">Top K</Label>
                  <Input
                    type="number"
                    value={topK}
                    onChange={(e) => {
                      markRetrievalConfigCustom();
                      setTopK(Number(e.target.value || 5));
                    }}
                    className="mt-1.5 border-border"
                    min={1}
                    max={20}
                  />
                </div>
                <div>
                  <Label className="text-xs font-medium text-muted-foreground">{t("knowledge.detail.qaRetrievalMode")}</Label>
                  <Select
                    value={mode}
                    onValueChange={(value) => {
                      markRetrievalConfigCustom();
                      setMode(value as typeof mode);
                    }}
                  >
                    <SelectTrigger className="mt-1.5 border-border">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="hybrid">{t("knowledge.detail.qaHybrid")}</SelectItem>
                      <SelectItem value="dense">{t("knowledge.detail.qaDenseOnly")}</SelectItem>
                      <SelectItem value="bm25">{t("knowledge.detail.qaBm25Only")}</SelectItem>
                    </SelectContent>
                  </Select>
                </div>
              </div>

              {mode === "hybrid" && (
                <div className="space-y-4 p-3 bg-primary/5 rounded-lg border border-primary/20">
                  <div className="flex items-center justify-between text-xs text-muted-foreground">
                    <span className="font-medium">{t("knowledge.detail.qaWeightConfig")}</span>
                    <Select
                      value={fusionMethod}
                      onValueChange={(value) => {
                        markRetrievalConfigCustom();
                        setFusionMethod(value as typeof fusionMethod);
                      }}
                    >
                      <SelectTrigger className="h-7 w-28 text-xs">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="weighted">{t("knowledge.detail.qaWeightedAvg")}</SelectItem>
                        <SelectItem value="rrf">RRF</SelectItem>
                      </SelectContent>
                    </Select>
                  </div>

                  <div>
                    <div className="flex items-center justify-between mb-1">
                      <Label className="text-xs text-primary/90">{t("knowledge.detail.qaDenseWeight")}</Label>
                      <span className="text-xs font-mono text-primary">{(denseWeight * 100).toFixed(0)}%</span>
                    </div>
                    <input
                      type="range"
                      min="0"
                      max="100"
                      value={denseWeight * 100}
                      onChange={(e) => {
                        markRetrievalConfigCustom();
                        const newDense = Number(e.target.value) / 100;
                        setDenseWeight(newDense);
                        setBm25Weight(1 - newDense);
                      }}
                      className="w-full h-2 bg-primary/20 rounded-lg appearance-none cursor-pointer accent-primary"
                    />
                  </div>

                  <div>
                    <div className="flex items-center justify-between mb-1">
                      <Label className="text-xs text-amber-700">{t("knowledge.detail.qaBm25Weight")}</Label>
                      <span className="text-xs font-mono text-amber-600">{(bm25Weight * 100).toFixed(0)}%</span>
                    </div>
                    <input
                      type="range"
                      min="0"
                      max="100"
                      value={bm25Weight * 100}
                      onChange={(e) => {
                        markRetrievalConfigCustom();
                        const newBm25 = Number(e.target.value) / 100;
                        setBm25Weight(newBm25);
                        setDenseWeight(1 - newBm25);
                      }}
                      className="w-full h-2 bg-amber-200 rounded-lg appearance-none cursor-pointer accent-amber-600"
                    />
                  </div>
                </div>
              )}
            </div>

            <div className="space-y-3">
              <Label className="text-sm font-medium text-foreground/80">{t("knowledge.detail.qaStrategy")}</Label>
              <div className="grid grid-cols-2 gap-3">
                <label className="flex items-center gap-2 cursor-pointer rounded-lg border border-border px-3 py-2 bg-card">
                  <Switch
                    checked={rerank}
                    onCheckedChange={(checked) => {
                      markRetrievalConfigCustom();
                      setRerank(checked);
                    }}
                  />
                  <span className="text-sm font-medium text-foreground/80">Rerank</span>
                </label>
                <label className="flex items-center gap-2 cursor-pointer rounded-lg border border-border px-3 py-2 bg-card">
                  <Switch
                    checked={mmr}
                    onCheckedChange={(checked) => {
                      markRetrievalConfigCustom();
                      setMmr(checked);
                    }}
                  />
                  <span className="text-sm font-medium text-foreground/80">MMR</span>
                </label>
                <label className="flex items-center gap-2 cursor-pointer rounded-lg border border-border px-3 py-2 bg-card">
                  <Switch checked={qaShowSources} onCheckedChange={setQaShowSources} />
                  <span className="text-sm font-medium text-foreground/80">{t("knowledge.detail.qaShowSources")}</span>
                </label>
                <label className="flex items-center gap-2 cursor-pointer rounded-lg border border-border px-3 py-2 bg-card">
                  <Switch checked={qaAutoScroll} onCheckedChange={setQaAutoScroll} />
                  <span className="text-sm font-medium text-foreground/80">{t("knowledge.detail.qaAutoScroll")}</span>
                </label>
                <label className="flex items-center gap-2 cursor-pointer rounded-lg border border-border px-3 py-2 bg-card">
                  <Switch checked={qaStrictMode} onCheckedChange={setQaStrictMode} />
                  <span className="text-sm font-medium text-foreground/80">{t("knowledge.detail.qaStrictMode")}</span>
                </label>
              </div>
              <p className="text-xs text-muted-foreground">
                {t("knowledge.detail.qaStrictModeHint")}
              </p>
            </div>

            <div className="space-y-3 rounded-xl border border-border/60 bg-muted/40 p-4">
              <div className="flex items-center justify-between text-xs text-muted-foreground">
                <span>{t("knowledge.detail.qaSessionStats")}</span>
                {lastQaResponse?.timing?.total_ms && (
                  <span className="font-mono">{t("knowledge.detail.qaRecentTiming", { ms: lastQaResponse.timing.total_ms })}</span>
                )}
              </div>
              <div className="grid grid-cols-2 gap-3">
                <div className="rounded-lg border border-border bg-card p-3">
                  <p className="text-xs text-muted-foreground">{t("knowledge.detail.qaMessages")}</p>
                  <p className="text-lg font-semibold text-foreground">{qaMessages.length}</p>
                </div>
                <div className="rounded-lg border border-border bg-card p-3">
                  <p className="text-xs text-muted-foreground">{t("knowledge.detail.qaRounds")}</p>
                  <p className="text-lg font-semibold text-foreground">{qaTurns}</p>
                </div>
                <div className="rounded-lg border border-border bg-card p-3">
                  <p className="text-xs text-muted-foreground">{t("knowledge.detail.qaRecentTokens")}</p>
                  <p className="text-lg font-semibold text-foreground">{lastQaResponse?.tokens_used ?? "-"}</p>
                </div>
                <div className="rounded-lg border border-border bg-card p-3">
                  <p className="text-xs text-muted-foreground">{t("knowledge.detail.qaRetrievalSegments")}</p>
                  <p className="text-lg font-semibold text-foreground">{lastQaResponse?.context_segments?.length ?? 0}</p>
                </div>
              </div>
              <Button
                variant="outline"
                className="w-full"
                onClick={handleClearQaChat}
                disabled={qaMessages.length === 0}
              >
                {t("knowledge.detail.qaClearChat")}
              </Button>
            </div>
          </div>
        </Card>

        {qaHistory.length > 0 && (
          <Card className="p-0 overflow-hidden border-border">
            <div className="px-5 py-4 border-b border-border/60 bg-card flex items-center gap-2">
              <Clock className="h-4 w-4 text-muted-foreground/70" />
              <h4 className="text-sm font-semibold text-foreground/80">{t("knowledge.detail.qaRecentQuestions")}</h4>
            </div>
            <div className="p-4 space-y-2 max-h-64 overflow-auto">
              {qaHistory.slice().reverse().map((h, i) => (
                <button
                  key={`${h.query}-${i}`}
                  className="w-full text-left p-3 rounded-lg bg-muted/40 hover:bg-primary/5 border border-transparent hover:border-primary/20 transition-[background-color,border-color] duration-150"
                  onClick={() => setQaQueryInput(h.query)}
                >
                  <p className="text-sm text-foreground/80 truncate font-medium">{h.query}</p>
                  <div className="flex items-center gap-2 mt-1 flex-wrap">
                    <Badge variant="outline" className="text-xs font-mono">
                      {h.response.timing.total_ms}ms
                    </Badge>
                    <span className="text-xs text-muted-foreground/70">
                      {t("knowledge.detail.qaSegmentsCount", { count: h.response.context_segments.length })}
                    </span>
                    <span className="text-xs text-muted-foreground/70 font-mono">
                      {h.response.model}
                    </span>
                  </div>
                </button>
              ))}
            </div>
          </Card>
        )}
      </div>

      {/* 右侧：对话 */}
      <div className="lg:col-span-8">
        <Card className="p-0 h-[calc(100vh-200px)] flex flex-col overflow-hidden border-border">
          <div className="px-5 py-4 border-b border-border/60 bg-card/90 backdrop-blur-sm flex items-center justify-between">
            <div className="flex items-center gap-3">
              <div className="w-9 h-9 rounded-xl bg-primary/10 border border-primary/20 flex items-center justify-center">
                <MessageSquare className="h-4 w-4 text-primary" />
              </div>
              <div>
                <h3 className="font-semibold text-foreground">{t("knowledge.detail.qaConversation")}</h3>
                <p className="text-xs text-muted-foreground">{t("knowledge.detail.qaStreamHint")}</p>
              </div>
            </div>
            <div className="flex items-center gap-2 text-xs">
              <Badge variant="outline" className="font-mono">{qaModel}</Badge>
              {qaLoading && (
                <Badge className="bg-primary/5 text-primary border-primary/20">{t("knowledge.detail.qaGenerating")}</Badge>
              )}
            </div>
          </div>

          <div className="flex-1 overflow-auto bg-linear-to-b from-card via-card to-muted/40 px-4 py-6">
            {qaMessages.length === 0 ? (
              <div className="flex flex-col items-center justify-center text-center h-full">
                <div className="w-20 h-20 rounded-2xl bg-primary/10 border border-primary/20 flex items-center justify-center mb-4">
                  <Sparkles className="h-10 w-10 text-primary/70" />
                </div>
                <p className="text-lg font-medium text-foreground/80">{t("knowledge.detail.qaStartTitle")}</p>
                <p className="text-sm text-muted-foreground/70 mt-1 max-w-md">
                  {t("knowledge.detail.qaStartHint")}
                </p>
                <div className="mt-5 flex flex-wrap gap-2 justify-center">
                  {[
                    t("knowledge.detail.qaSuggestion1"),
                    t("knowledge.detail.qaSuggestion2"),
                    t("knowledge.detail.qaSuggestion3"),
                  ].map((suggestion) => (
                    <button
                      key={suggestion}
                      className="px-3 py-1.5 rounded-full border border-border bg-card text-xs text-muted-foreground hover:border-primary/30 hover:text-primary transition-colors"
                      onClick={() => setQaQueryInput(suggestion)}
                    >
                      {suggestion}
                    </button>
                  ))}
                </div>
              </div>
            ) : (
              <div className="space-y-5">
                {qaMessages.map((msg) => {
                  const isUser = msg.role === "user";
                  const bubbleStyles = isUser
                    ? "bg-primary text-white rounded-tr-sm"
                    : msg.status === "error"
                      ? "bg-red-500/10 dark:bg-red-500/15 text-red-700 dark:text-red-400 border border-red-500/20"
                      : "bg-card text-foreground/80 border border-border";

                  return (
                    <div key={msg.id} className={`flex ${isUser ? "justify-end" : "justify-start"}`}>
                      <div className={`max-w-[85%] flex gap-3 ${isUser ? "flex-row-reverse" : "flex-row"}`}>
                        <div className={`h-9 w-9 rounded-full flex items-center justify-center ${isUser ? "bg-primary text-white" : "bg-card border border-border"}`}>
                          {isUser ? <User className="h-4 w-4" /> : <Bot className="h-4 w-4 text-primary" />}
                        </div>
                        <div className="flex flex-col gap-2">
                          <div className={`rounded-2xl px-4 py-3 text-sm shadow-xs ${bubbleStyles}`}>
                            {msg.role === "assistant" ? (
                              msg.content ? (
                                <StreamOutput text={msg.content} />
                              ) : msg.status !== "pending" ? (
                                <p>{t(msg.status === "done" ? "knowledge.detail.qaEmptyAnswer" : "knowledge.detail.qaInterruptedEmpty")}</p>
                              ) : (
                                <div className="flex items-center gap-2 text-sm text-muted-foreground">
                                  <Loader2 className="h-4 w-4 animate-spin" />
                                  {t("knowledge.detail.qaGeneratingAnswer")}
                                </div>
                              )
                            ) : (
                              <div className="whitespace-pre-wrap">{msg.content}</div>
                            )}
                          </div>

                          {msg.role === "assistant" && msg.status === "error" && <div role="status" className="space-y-2 text-sm">
                            <p>{t("knowledge.detail.qaInterrupted")}</p>
                            <Button variant="outline" size="sm" disabled={qaLoading} onClick={() => {
                              if (currentView && activeScope.current.datasetId === datasetId
                                && activeScope.current.identityScope === identityScope
                                && (identityScope?.isCurrent() ?? true)) setQaQueryInput(msg.query ?? "");
                            }}>{t("knowledge.detail.qaReuseQuestion")}</Button>
                          </div>}

                          {msg.role === "assistant" && msg.status === "error" && msg.query && (
                            <SaveKnowledgeFailureDialog
                              kbDatasetId={datasetId}
                              query={msg.query}
                              observedHits={[]}
                              source={QA_EVAL_SOURCE}
                              testId={`send-qa-to-eval-${msg.id}`}
                            />
                          )}

                          {msg.role === "assistant" && msg.response && (
                            <div className="space-y-2">
                              <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
                                <Badge variant="outline" className="font-mono text-xs">
                                  {msg.response.model}
                                </Badge>
                                <span className="flex items-center gap-1">
                                  <Zap className="h-3 w-3" />
                                  {t("knowledge.detail.qaRetrievalTiming", { ms: msg.response.timing.retrieval_ms })}
                                </span>
                                <span className="flex items-center gap-1">
                                  <Brain className="h-3 w-3" />
                                  LLM {msg.response.timing.llm_ms}ms
                                </span>
                                <span>{t("knowledge.detail.qaTotalTiming", { ms: msg.response.timing.total_ms })}</span>
                                {msg.response.tokens_used && <span>Tokens {msg.response.tokens_used}</span>}
                                <SaveKnowledgeFailureDialog
                                  kbDatasetId={datasetId}
                                  query={msg.query || msg.response.query}
                                  observedHits={msg.response.context_segments}
                                  source={QA_EVAL_SOURCE}
                                  sourceTraceId={msg.response.trace_id || undefined}
                                  queryFingerprint={msg.response.query_fingerprint || undefined}
                                  observedAnswer={msg.response.answer}
                                  testId={`send-qa-to-eval-${msg.id}`}
                                />
                              </div>

                              {datasetId && msg.response.trace_id && msg.response.query_fingerprint && (
                                <KnowledgeFeedbackControl
                                  compact
                                  datasetId={datasetId}
                                  traceId={msg.response.trace_id}
                                  queryFingerprint={msg.response.query_fingerprint}
                                  targetType="qa_answer"
                                />
                              )}

                              {qaShowSources && msg.response.context_segments.length > 0 && (
                                <details className="rounded-lg border border-border bg-muted/40">
                                  <summary className="cursor-pointer px-3 py-2 text-xs text-muted-foreground flex items-center gap-2">
                                    <Database className="h-3.5 w-3.5" />
                                    {t("knowledge.detail.qaSourceSegments", { count: msg.response.context_segments.length })}
                                  </summary>
                                  <div className="px-3 pb-3 space-y-2">
                                    {msg.response.context_segments.map((seg, segIndex) => (
                                      <div key={seg.segment_id} className="rounded-md border border-border bg-card p-2 text-xs text-muted-foreground">
                                        <div className="flex items-center justify-between mb-1">
                                          <span className="inline-flex items-center justify-center w-5 h-5 rounded-md bg-primary/10 text-primary text-[10px] font-semibold">
                                            {segIndex + 1}
                                          </span>
                                          <Badge className="bg-primary/5 text-primary font-mono text-[10px]">
                                            {seg.score.toFixed(4)}
                                          </Badge>
                                        </div>
                                        <p className="line-clamp-3 whitespace-pre-wrap">{seg.text}</p>
                                        <div className="mt-2"><KnowledgeSourceButton datasetId={msg.datasetId} hit={seg} /></div>
                                      </div>
                                    ))}
                                  </div>
                                </details>
                              )}
                            </div>
                          )}
                        </div>
                      </div>
                    </div>
                  );
                })}
                <div ref={qaChatEndRef} />
              </div>
            )}
          </div>

          <div className="border-t border-border/60 bg-card p-4">
            <div className="flex flex-col gap-3 sm:flex-row">
              <Textarea
                placeholder={t("knowledge.detail.qaInputPlaceholder")}
                value={qaQueryInput}
                onChange={(e) => setQaQueryInput(e.target.value)}
                onKeyDown={handleQaKeyDown}
                rows={2}
                className="flex-1 resize-none border-border focus:border-primary focus:ring-primary"
              />
              <Button
                onClick={runQA}
                disabled={qaLoading || !qaQueryInput.trim()}
                className="h-11 bg-primary hover:bg-primary/90 text-white"
              >
                {qaLoading ? (
                  <><Loader2 className="h-4 w-4 mr-2 animate-spin" /> {t("knowledge.detail.qaGeneratingBtn")}</>
                ) : (
                  <><Send className="h-4 w-4 mr-2" /> {t("knowledge.detail.qaSend")}</>
                )}
              </Button>
            </div>
            <div className="mt-2 flex items-center justify-between text-xs text-muted-foreground/70">
              <span>{t("knowledge.detail.qaInputHint")}</span>
              <span className="font-mono">TopK {topK} · {mode}</span>
            </div>
          </div>
        </Card>
      </div>
    </div>
  );
}
