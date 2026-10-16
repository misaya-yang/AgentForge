/**
 * Retrieval hit-test tab of the dataset detail page.
 *
 * All console state (query, topK, mode, weights, rerank/mmr, presets, results,
 * RAGAS scoring) lives in the shared useHitTestConsole bundle so edits made
 * here stay visible in the QA tab and vice versa.
 *
 * Extracted verbatim from DatasetDetail.tsx (C2 split); no behavior change.
 * The component stays mounted while other tabs are visible (the shell hides
 * it with `hidden`) so all state survives tab switches exactly as before.
 */

import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";
import {
  HelpCircle,
  ImageIcon,
  Loader2,
  Target,
  BarChart3,
  Search,
} from "lucide-react";

import { SaveKnowledgeFailureDialog } from "@/pages/knowledge/detail/SaveKnowledgeFailureDialog";
import { HIT_TEST_EVAL_SOURCE } from "@/pages/knowledge/detail/evalCaseStore";
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
import { RetrievalResultCard } from "@/pages/knowledge/detail/RetrievalResultCard";
import type { HitTestConsole } from "@/pages/knowledge/detail/useHitTestConsole";

interface RetrievalTabProps {
  datasetId?: string;
  hitTest: HitTestConsole;
}

export function RetrievalTab({ datasetId, hitTest }: RetrievalTabProps) {
  const { t } = useTranslation();
  const nav = useNavigate();
  const {
    query,
    setQuery,
    topK,
    setTopK,
    scoreThreshold,
    setScoreThreshold,
    rerank,
    setRerank,
    hitRerankModel,
    setHitRerankModel,
    rerankModelOptions,
    rerankModelConfigStatus,
    mmr,
    setMmr,
    hitLoading,
    retrievalPresets,
    retrievalPresetStatus,
    retrievalPresetError,
    setRetrievalPresetReloadKey,
    selectedPreset,
    applyPreset,
    markRetrievalConfigCustom,
    hitResults,
    ragasLoading,
    ragasResults,
    ragasJudgeModel,
    hitMeta,
    hitTraceId,
    hitQueryFingerprint,
    hitExecutedQuery,
    invalidateHitTestResults,
    runHitTest,
    runRagasScore,
  } = hitTest;
  const hitFailed = typeof hitMeta.error === "string";
  const hitErrorMessage = hitMeta.error === "rerank_preset_unavailable"
    ? t("knowledge.detail.rerankPresetUnavailable")
    : hitMeta.error === "rerank_model_unconfigured"
      ? t("knowledge.detail.rerankModelUnconfigured")
      : typeof hitMeta.error === "string" ? hitMeta.error : "";
  const isHitCount = (value: unknown): value is number => typeof value === "number" && Number.isInteger(value) && value >= 0;
  const hitCount = (value: unknown) => isHitCount(value) ? value : t("knowledge.detail.hitCountUnknown");

  return (
    <div className="grid grid-cols-1 gap-6 lg:grid-cols-12">
      {/* 左侧：知识库配置调试 */}
      <div className="lg:col-span-4">
        <Card className="p-5 bg-card">
          <h3 className="font-semibold text-foreground mb-6">{t("knowledge.detail.retrievalConfig")}</h3>

          <div className="space-y-6">
            {/* Retrieval presets are explicit opt-in starting points. */}
            <div>
              <Label className="text-sm text-muted-foreground flex items-center gap-1">
                {t("knowledge.detail.retrievalPreset")} {t("knowledge.detail.apiProjection")}
                <HelpCircle className="h-3.5 w-3.5 text-muted-foreground/70" aria-hidden="true" />
              </Label>
              <Select
                value={selectedPreset}
                onValueChange={applyPreset}
                disabled={retrievalPresetStatus !== "ready"}
              >
                <SelectTrigger
                  className="mt-2 bg-card"
                  aria-label={t("knowledge.detail.retrievalPreset")}
                  data-testid="retrieval-preset"
                >
                  <SelectValue
                    placeholder={
                      retrievalPresetStatus === "loading"
                        ? t("knowledge.eval.loadingPresets")
                        : t("knowledge.eval.selectPreset")
                    }
                  />
                </SelectTrigger>
                <SelectContent>
                  {retrievalPresets.map((preset) => (
                    <SelectItem key={preset.name} value={preset.name}>
                      {preset.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {retrievalPresetStatus === "error" ? (
                <div className="mt-2 flex flex-col gap-2" role="alert">
                  <p className="text-xs text-destructive">
                    {t("knowledge.eval.presetsFailedDetail", { error: retrievalPresetError })}
                  </p>
                  <Button
                    type="button"
                    size="sm"
                    variant="outline"
                    className="w-fit"
                    onClick={() => setRetrievalPresetReloadKey((value) => value + 1)}
                  >
                    {t("knowledge.eval.retry")}
                  </Button>
                </div>
              ) : (
                <p className="text-xs text-muted-foreground mt-1.5">
                  {selectedPreset
                    ? `${retrievalPresets.find((preset) => preset.name === selectedPreset)?.summary ?? ""} ${t(
                        "knowledge.eval.projectionHint"
                      )}`
                    : t("knowledge.eval.presetOptional")}
                </p>
              )}
            </div>

            {/* 选择排序模型 */}
            <div>
              <Label className="text-sm text-muted-foreground flex items-center gap-1">
                {t("knowledge.detail.selectRerankModel")}
                <HelpCircle className="h-3.5 w-3.5 text-muted-foreground/70" />
              </Label>
              <Select
                value={hitRerankModel}
                disabled={rerankModelOptions.length === 0}
                onValueChange={(value) => {
                  markRetrievalConfigCustom();
                  setHitRerankModel(value);
                }}
              >
                <SelectTrigger className="mt-2 bg-card" aria-label={t("knowledge.detail.selectRerankModel")}>
                  <SelectValue placeholder={
                    rerankModelConfigStatus === "loading"
                      ? t("knowledge.detail.rerankModelsLoading")
                      : t("knowledge.detail.rerankModelNoOptions")
                  } />
                </SelectTrigger>
                <SelectContent>
                  {rerankModelOptions.map((model) => (
                    <SelectItem key={model} value={model}>{model}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {rerankModelOptions.length === 0 && (
                <p
                  className="mt-1.5 text-xs text-muted-foreground"
                  role={rerankModelConfigStatus === "error" ? "alert" : "status"}
                >
                  {rerankModelConfigStatus === "loading"
                    ? t("knowledge.detail.rerankModelsLoading")
                    : rerankModelConfigStatus === "error"
                      ? t("knowledge.detail.rerankModelLoadFailed")
                      : t("knowledge.detail.rerankModelUnconfigured")}
                </p>
              )}
            </div>

            {/* 相似度阈值 - 阿里云风格 */}
            <div>
              <Label className="text-sm text-muted-foreground flex items-center gap-1">
                {t("knowledge.detail.similarityThreshold")}
                <HelpCircle className="h-3.5 w-3.5 text-muted-foreground/70" />
              </Label>
              <div className="mt-2 flex items-center gap-3">
                <input
                  type="range"
                  min={0}
                  max={1}
                  step={0.01}
                  value={scoreThreshold}
                  onChange={(e) => {
                    markRetrievalConfigCustom();
                    setScoreThreshold(Number(e.target.value));
                  }}
                  aria-label={t("knowledge.detail.similarityThreshold")}
                  className="flex-1 h-1.5 bg-primary/10 rounded-lg appearance-none cursor-pointer accent-primary"
                />
                <Input
                  type="number"
                  value={scoreThreshold}
                  onChange={(e) => {
                    markRetrievalConfigCustom();
                    const value = Number(e.target.value);
                    setScoreThreshold(Number.isFinite(value) ? Math.min(1, Math.max(0, value)) : 0);
                  }}
                  className="w-20 h-9 text-center"
                  step={0.01}
                  min={0}
                  max={1}
                />
              </div>
              <div className="flex justify-between text-xs text-muted-foreground/70 mt-1">
                <span>0</span>
                <span>1</span>
              </div>
            </div>

            {/* 最大召回数量 - 阿里云风格 */}
            <div>
              <Label className="text-sm text-muted-foreground flex items-center gap-1">
                {t("knowledge.detail.maxRecall")}
                <HelpCircle className="h-3.5 w-3.5 text-muted-foreground/70" />
              </Label>
              <div className="mt-2 flex items-center gap-3">
                <input
                  type="range"
                  min={1}
                  max={20}
                  step={1}
                  value={topK}
                  onChange={(e) => {
                    markRetrievalConfigCustom();
                    setTopK(Number(e.target.value));
                  }}
                  aria-label={t("knowledge.detail.maxRecall")}
                  className="flex-1 h-1.5 bg-primary/10 rounded-lg appearance-none cursor-pointer accent-primary"
                />
                <Input
                  type="number"
                  value={topK}
                  onChange={(e) => {
                    markRetrievalConfigCustom();
                    const value = Number(e.target.value);
                    setTopK(Number.isFinite(value) ? Math.min(20, Math.max(1, value)) : 5);
                  }}
                  className="w-20 h-9 text-center"
                  min={1}
                  max={20}
                />
              </div>
              <div className="flex justify-between text-xs text-muted-foreground/70 mt-1">
                <span>1</span>
                <span>20</span>
              </div>
            </div>

            {/* 输入 */}
            <div>
              <Label className="text-sm text-muted-foreground">{t("knowledge.detail.inputLabel")}</Label>
              <Textarea
                placeholder={t("knowledge.detail.inputPlaceholder")}
                value={query}
                onChange={(e) => {
                  invalidateHitTestResults();
                  setQuery(e.target.value);
                }}
                rows={4}
                className="mt-2 resize-none"
              />
              <div className="flex justify-end mt-1">
                <button
                  type="button"
                  className="text-muted-foreground/70 hover:text-muted-foreground"
                  aria-label={t("knowledge.detail.image")}
                >
                  <ImageIcon className="h-4 w-4" aria-hidden="true" />
                </button>
              </div>
            </div>

            {/* 高级选项 */}
            <div className="pt-4 border-t border-border/60">
              <div className="flex items-center gap-6">
                <label className="flex items-center gap-2 cursor-pointer">
                  <Switch
                    checked={rerank}
                    disabled={rerankModelOptions.length === 0}
                    onCheckedChange={(checked) => {
                      markRetrievalConfigCustom();
                      setRerank(checked);
                    }}
                  />
                  <span className="text-sm text-foreground/80">Rerank</span>
                </label>
                <label className="flex items-center gap-2 cursor-pointer">
                  <Switch
                    checked={mmr}
                    onCheckedChange={(checked) => {
                      markRetrievalConfigCustom();
                      setMmr(checked);
                    }}
                  />
                  <span className="text-sm text-foreground/80">MMR</span>
                </label>
              </div>
            </div>

            {/* 测试按钮 - 阿里云风格 */}
            <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
              <Button
                onClick={runHitTest}
                disabled={hitLoading || !query.trim()}
                className="h-10 bg-primary/10 hover:bg-primary/20 text-primary font-medium border-0"
              >
                {hitLoading ? (
                  <><Loader2 className="h-4 w-4 mr-2 animate-spin" /> {t("knowledge.detail.testing")}</>
                ) : (
                  <><Target className="h-4 w-4 mr-2" /> {t("knowledge.detail.test")}</>
                )}
              </Button>
              <Button
                variant="outline"
                onClick={runRagasScore}
                disabled={ragasLoading || hitLoading || !query.trim() || hitResults.length === 0}
                className="h-10 border-primary/30 text-primary hover:bg-primary/5"
              >
                {ragasLoading ? (
                  <><Loader2 className="h-4 w-4 mr-2 animate-spin" /> {t("knowledge.detail.ragasScoring")}</>
                ) : (
                  <><BarChart3 className="h-4 w-4 mr-2" /> {t("knowledge.detail.ragasScore")}</>
                )}
              </Button>
            </div>

            {ragasResults.length > 0 && (
              <div className="p-4 rounded-lg border border-border bg-card/80 space-y-3">
                <div className="flex items-center justify-between gap-2">
                  <h4 className="text-xs font-semibold text-foreground/80">{t("knowledge.detail.ragasResults")}</h4>
                  {ragasJudgeModel ? (
                    <Badge variant="outline" className="font-mono text-[10px]">{ragasJudgeModel}</Badge>
                  ) : null}
                </div>
                <div className="grid gap-2">
                  {ragasResults.map((result) => (
                    <div
                      key={result.metric}
                      className="flex items-start justify-between gap-3 rounded-md border border-border/70 bg-muted/30 px-3 py-2"
                    >
                      <div className="min-w-0">
                        <p className="text-xs font-semibold text-foreground">
                          {t(`eval.ragas.metrics.${result.metric}`, result.metric)}
                        </p>
                        <p className="text-[11px] text-muted-foreground mt-1 line-clamp-3">{result.explanation}</p>
                      </div>
                      <div className="text-right shrink-0">
                        <p className="text-sm font-bold text-foreground tabular-nums">
                          {Math.round(result.score * 100)}%
                        </p>
                        <Badge
                          className={
                            result.label === "pass"
                              ? "bg-emerald-100 dark:bg-emerald-950/40 text-emerald-700 dark:text-emerald-300"
                              : result.label === "fail"
                                ? "bg-red-100 dark:bg-red-950/40 text-red-700 dark:text-red-300"
                                : "bg-amber-100 dark:bg-amber-950/40 text-amber-700 dark:text-amber-300"
                          }
                        >
                          {t(`eval.ragas.labels.${result.label}`, result.label)}
                        </Badge>
                      </div>
                    </div>
                  ))}
                </div>
                <Button
                  variant="ghost"
                  size="sm"
                  className="w-full text-xs"
                  onClick={() => nav(`/eval?dataset_id=${encodeURIComponent(datasetId || "")}`)}
                >
                  {t("knowledge.detail.openEvalConsole")}
                </Button>
              </div>
            )}

            {Object.keys(hitMeta).length > 0 && !hitFailed && (
              <div className="p-4 bg-linear-to-r from-muted/70 to-primary/5 rounded-lg border border-border">
                <h4 className="text-xs font-semibold text-foreground/80 mb-3 flex items-center gap-2">
                  <BarChart3 className="h-3.5 w-3.5" />
                  {t("knowledge.detail.retrievalStats")}
                </h4>
                <div className="grid grid-cols-2 gap-3 text-xs">
                  <div className="flex items-center justify-between p-2 bg-card rounded border">
                    <span className="text-muted-foreground">{t("knowledge.detail.mode")}</span>
                    <Badge variant="outline" className="font-mono">
                      {String(hitMeta.mode)}
                    </Badge>
                  </div>
                  <div className="flex items-center justify-between p-2 bg-card rounded border">
                    <span className="text-muted-foreground">Rerank</span>
                    <Badge
                      className={
                        typeof hitMeta.rerank_applied_provider === "string"
                          ? "bg-emerald-100 text-emerald-700"
                          : "bg-secondary/60 text-muted-foreground"
                      }
                    >
                      {typeof hitMeta.rerank_applied_provider === "string"
                        ? t("knowledge.eval.executed")
                        : hitMeta.rerank
                          ? t("knowledge.eval.requestedUnverified")
                          : t("knowledge.detail.disabled")}
                    </Badge>
                  </div>
                  <div className="flex items-center justify-between p-2 bg-card rounded border">
                    <span className="text-muted-foreground">MMR</span>
                    <Badge className={hitMeta.mmr ? "bg-accent/10 text-accent/90" : "bg-secondary/60 text-muted-foreground"}>
                      {hitMeta.mmr ? t("knowledge.detail.enabled") : t("knowledge.detail.disabled")}
                    </Badge>
                  </div>
                  {hitMeta.score_threshold !== undefined && hitMeta.score_threshold !== null && (
                    <div className="flex items-center justify-between p-2 bg-card rounded border">
                      <span className="text-muted-foreground">{t("knowledge.detail.threshold")}</span>
                      <span className="font-mono text-foreground/80">{String(hitMeta.score_threshold)}</span>
                    </div>
                  )}
                </div>

                {/* Hit counts */}
                <div className="mt-3 pt-3 border-t border-border grid grid-cols-2 gap-3">
                  {isHitCount(hitMeta.vector_hits_count) && (
                    <div className="p-2 bg-primary/5 rounded text-center">
                      <div className="text-lg font-bold text-primary/90">{hitMeta.vector_hits_count}</div>
                      <div className="text-xs text-primary">{t("knowledge.detail.vectorHits")}</div>
                      {isHitCount(hitMeta.vector_hits_raw_count) && hitMeta.vector_hits_raw_count !== hitMeta.vector_hits_count && (
                        <div className="text-xs text-primary/70">{t("knowledge.detail.vectorRaw", { count: hitMeta.vector_hits_raw_count })}</div>
                      )}
                    </div>
                  )}
                  {isHitCount(hitMeta.keyword_hits_count) && (
                    <div className="p-2 bg-amber-500/10 dark:bg-amber-500/15 rounded text-center">
                      <div className="text-lg font-bold text-amber-700 dark:text-amber-400">{hitMeta.keyword_hits_count}</div>
                      <div className="text-xs text-amber-600 dark:text-amber-500">{t("knowledge.detail.keywordHits")}</div>
                      {isHitCount(hitMeta.keyword_hits_raw_count) && hitMeta.keyword_hits_raw_count !== hitMeta.keyword_hits_count && (
                        <div className="text-xs text-amber-400">{t("knowledge.detail.keywordRaw", { count: hitMeta.keyword_hits_raw_count })}</div>
                      )}
                    </div>
                  )}
                </div>

                {typeof hitMeta.collection_name === 'string' && hitMeta.collection_name && (
                  <div className="mt-2 text-xs text-muted-foreground/70 truncate font-mono">
                    {t("knowledge.detail.collection", { name: hitMeta.collection_name })}
                  </div>
                )}
                {typeof hitMeta.error === 'string' && hitMeta.error && (
                  <div className="mt-2 p-2 bg-red-500/10 dark:bg-red-500/15 text-red-600 dark:text-red-400 rounded text-xs">
                    {t("knowledge.detail.error", { msg: hitErrorMessage })}
                  </div>
                )}
                {typeof hitMeta.ragas_error === 'string' && hitMeta.ragas_error && (
                  <div className="mt-2 p-2 bg-red-500/10 dark:bg-red-500/15 text-red-600 dark:text-red-400 rounded text-xs">
                    {t("knowledge.detail.ragasError", { msg: hitMeta.ragas_error })}
                  </div>
                )}
              </div>
            )}
          </div>
        </Card>
      </div>

      {/* 右侧：结果 */}
      <div className="lg:col-span-8">
        <Card className="p-0 h-[calc(100vh-200px)] overflow-hidden shadow-xs">
          <div className="px-5 py-4 border-b border-border/60 bg-card flex items-center justify-between sticky top-0">
            <div className="flex items-center gap-3">
              <h3 className="font-bold text-foreground">{t("knowledge.detail.retrievalResults")}</h3>
              {hitResults.length > 0 && (
                <Badge className="bg-emerald-100 dark:bg-emerald-950/40 text-emerald-700 dark:text-emerald-300 border-emerald-200 dark:border-emerald-900/60">
                  {t("knowledge.detail.resultCount", { count: hitResults.length })}
                </Badge>
              )}
            </div>
            {hitExecutedQuery && (
              <div className="flex flex-wrap items-center gap-2">
                <SaveKnowledgeFailureDialog
                  kbDatasetId={datasetId}
                  query={hitExecutedQuery}
                  observedHits={hitResults}
                  source={HIT_TEST_EVAL_SOURCE}
                  sourceTraceId={hitTraceId || undefined}
                  queryFingerprint={hitQueryFingerprint || undefined}
                  disabled={hitLoading}
                  testId="send-hits-to-eval"
                />
                {hitResults.length > 0 && (
                  <div className="flex items-center gap-2 text-xs text-muted-foreground">
                    <span>{t("knowledge.detail.highestScore", { score: Math.max(...hitResults.map(h => h.score)).toFixed(4) })}</span>
                    <span>·</span>
                    <span>{t("knowledge.detail.lowestScore", { score: Math.min(...hitResults.map(h => h.score)).toFixed(4) })}</span>
                  </div>
                )}
              </div>
            )}
          </div>

          <div className="p-5 space-y-4 overflow-auto h-[calc(100%-70px)] bg-muted/30">
            {hitResults.map((hit, i) => (
              <RetrievalResultCard
                key={`${hit.segment_id}-${i}`}
                hit={hit}
                index={i}
                highlightTerms={query.trim().split(/\s+/).filter(t => t.length > 0)}
                datasetId={datasetId}
                traceId={hitTraceId || undefined}
                queryFingerprint={hitQueryFingerprint || undefined}
              />
            ))}
            {hitResults.length === 0 && !hitLoading && (
              <div className="text-center py-20">
                <div className="w-20 h-20 mx-auto rounded-2xl bg-linear-to-br from-emerald-100 to-teal-100 flex items-center justify-center mb-4">
                  <Search className="h-10 w-10 text-emerald-400" />
                </div>
                <p className="text-lg font-medium text-muted-foreground">
                  {t(hitFailed ? "knowledge.detail.retrievalFailed"
                    : hitExecutedQuery ? "knowledge.detail.noResults" : "knowledge.detail.retrievalReady")}
                </p>
                <p className="text-sm text-muted-foreground/70 mt-2 max-w-sm mx-auto">
                  {t(hitFailed ? "knowledge.detail.retrievalFailedHint"
                    : hitExecutedQuery ? "knowledge.detail.retrievalEmptyHint" : "knowledge.detail.noResultsHint")}
                </p>
                {!hitFailed && hitExecutedQuery && (
                  <p className="text-sm text-muted-foreground/70 mt-2">
                    {t("knowledge.detail.vectorHitsCount", {
                      vector: hitCount(hitMeta.vector_hits_count), keyword: hitCount(hitMeta.keyword_hits_count),
                    })}
                  </p>
                )}
                {typeof hitMeta.error === 'string' && hitMeta.error && (
                  <div role="alert" className="mt-4 p-3 bg-red-500/10 dark:bg-red-500/15 border border-red-500/20 rounded-lg text-sm text-red-600 dark:text-red-400 max-w-md mx-auto">
                    {hitErrorMessage}
                  </div>
                )}
              </div>
            )}
          </div>
        </Card>
      </div>
    </div>
  );
}
