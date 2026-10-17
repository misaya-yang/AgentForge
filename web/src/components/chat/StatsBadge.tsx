import { motion } from "motion/react";
import { useTranslation } from "react-i18next";
import { Clock, Zap, MessageSquare } from "lucide-react";
import type { ChatMessage } from "@/components/ChatWindow";

export function StatsBadge({
  stats,
}: {
  stats: NonNullable<ChatMessage["stats"]>;
}) {
  const { t } = useTranslation();

  const hasStats =
    stats.reportedRouting !== undefined ||
    stats.durationMs != null ||
    stats.firstTokenMs != null ||
    stats.totalTokens != null ||
    stats.tokenUsageSource !== undefined;
  if (!hasStats) return null;
  const hasKnownSource = Object.values(stats.tokenProvenance ?? {}).some(source => source !== "unknown");
  const sourceLabel = (field: "input_tokens" | "output_tokens" | "total_tokens") =>
    hasKnownSource ? ` (${t(`playground.stats.tokenSources.${stats.tokenProvenance?.[field] ?? "unknown"}`)})` : "";

  return (
    <motion.div
      data-message-stats="true"
      initial={{ opacity: 0, y: 6 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ delay: 0.2, duration: 0.35, ease: "easeOut" }}
      className="flex flex-wrap items-center gap-1.5 text-[10px] mt-2.5 ml-1"
    >
      {stats.reportedRouting && (
        <span data-reported-routing="upstream_reported"
          title={t("playground.modelFailover.reportedHelp")}
          className="inline-flex flex-wrap items-center gap-1 px-2 py-0.5 rounded-md bg-slate-100/80 dark:bg-zinc-800/60 text-slate-500 dark:text-zinc-400">
          {t("playground.modelFailover.reportedSelection")}: {stats.reportedRouting.provider_id} / {stats.reportedRouting.model_id}
        </span>
      )}
      {/* Duration */}
      {stats.durationMs != null && (
        <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded-md bg-slate-100/80 dark:bg-zinc-800/60 text-slate-500 dark:text-zinc-400 border border-slate-200/40 dark:border-zinc-700/40">
          <Clock className="h-2.5 w-2.5" />
          <span className="font-medium tabular-nums">
            {(stats.durationMs / 1000).toFixed(2)}s
          </span>
        </span>
      )}

      {/* TTFT */}
      {stats.firstTokenMs != null && (
        <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded-md bg-blue-50/80 dark:bg-blue-950/30 text-blue-600 dark:text-blue-400 border border-blue-200/40 dark:border-blue-800/30">
          <Zap className="h-2.5 w-2.5" />
          <span className="font-medium tabular-nums">
            {t("playground.stats.ttft", "TTFT")}: {stats.firstTokenMs}ms
          </span>
        </span>
      )}

      {/* Playground observes token sources without making billing/measurement claims. */}
      {stats.tokenUsageSource !== undefined && (
        <span
          data-token-usage-source={stats.tokenUsageSource}
          title={t("playground.stats.usageSourceHelp")}
          className="inline-flex flex-wrap items-center gap-1 px-2 py-0.5 rounded-md bg-slate-100/80 dark:bg-zinc-800/60 text-slate-500 dark:text-zinc-400 border border-slate-200/40 dark:border-zinc-700/40"
        >
          <MessageSquare className="h-2.5 w-2.5" />
          <span>{t(stats.tokenUsageSource === "reported" ? (hasKnownSource ? "playground.stats.serverReported" : "playground.stats.serverSourceUnknown") : "playground.stats.sourceUnknown")}</span>
          <span className="font-medium tabular-nums">{t("playground.stats.tokens")}: {stats.totalTokens ?? t("playground.stats.unknown")}{stats.totalTokens !== undefined && sourceLabel("total_tokens")}</span>
          <span className="tabular-nums">
            ({t("playground.stats.input")}: {stats.inputTokens ?? t("playground.stats.unknown")}{stats.inputTokens !== undefined && sourceLabel("input_tokens")}; {t("playground.stats.output")}: {stats.outputTokens ?? t("playground.stats.unknown")}{stats.outputTokens !== undefined && sourceLabel("output_tokens")})
          </span>
        </span>
      )}
      {stats.tokenUsageSource !== undefined && stats.textTokenEstimate &&
        (stats.inputTokens === undefined || stats.outputTokens === undefined || stats.totalTokens === undefined) && (
        <span
          data-token-usage-source="estimated"
          title={t("playground.stats.textEstimateHelp")}
          className="inline-flex items-center gap-1 px-2 py-0.5 rounded-md text-slate-500 dark:text-zinc-400 border border-dashed border-slate-300 dark:border-zinc-600"
        >
          <span>{t("playground.stats.textEstimate")}: ~{stats.textTokenEstimate.totalTokens} {t("playground.stats.tokens")}</span>
          <span className="tabular-nums">({stats.textTokenEstimate.inputTokens}/{stats.textTokenEstimate.outputTokens})</span>
        </span>
      )}

      {/* Token count */}
      {stats.tokenUsageSource === undefined && stats.totalTokens != null && (
        <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded-md bg-slate-100/80 dark:bg-zinc-800/60 text-slate-500 dark:text-zinc-400 border border-slate-200/40 dark:border-zinc-700/40">
          <MessageSquare className="h-2.5 w-2.5" />
          <span className="font-medium tabular-nums">{stats.totalTokens}</span>
          <span className="text-slate-400 dark:text-zinc-500">
            {t("playground.stats.tokens", "tokens")}
          </span>
          {stats.inputTokens != null && stats.outputTokens != null && (
            <span className="text-slate-400 dark:text-zinc-500 text-[9px] tabular-nums">
              ({stats.inputTokens}/{stats.outputTokens})
            </span>
          )}
        </span>
      )}
    </motion.div>
  );
}
