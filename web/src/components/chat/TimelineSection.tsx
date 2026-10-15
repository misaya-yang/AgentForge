import { motion, AnimatePresence } from "framer-motion";
import { useTranslation } from "react-i18next";
import { cn } from "@/lib/utils";
import { AgentTimeline, type TimelineState } from "@/components/agent/AgentTimeline";
import { ChevronDown, Activity } from "lucide-react";

export function TimelineSection({
  timeline,
  isExpanded,
  onToggle,
}: {
  timeline: TimelineState;
  isExpanded: boolean;
  onToggle: () => void;
}) {
  const { t } = useTranslation();
  const stepCount = timeline.steps.length;
  const isRunning = timeline.status === "running";

  if (stepCount === 0) return null;

  return (
    <div className="mb-4 w-full">
      {/* Collapsible Header - glassmorphism style */}
      <button
        type="button"
        onClick={onToggle}
        className={cn(
          "flex items-center gap-3 w-full px-4 py-2.5 rounded-xl transition-all duration-200",
          "text-left text-xs font-medium",
          "bg-linear-to-r from-blue-500/5 via-cyan-500/5 to-sky-500/5",
          "hover:from-blue-500/10 hover:via-cyan-500/10 hover:to-sky-500/10",
          "border border-blue-500/20 dark:border-blue-400/20",
          "backdrop-blur-xs",
          isRunning && "border-blue-500/40 shadow-[0_0_15px_-3px] shadow-blue-500/20"
        )}
      >
        <div className={cn(
          "flex items-center justify-center h-6 w-6 rounded-lg transition-all duration-200",
          isRunning
            ? "bg-linear-to-br from-blue-500 to-cyan-600 text-white shadow-lg shadow-blue-500/30"
            : "bg-blue-500/10 text-blue-600 dark:text-blue-400"
        )}>
          <Activity className={cn("h-3.5 w-3.5", isRunning && "animate-pulse")} />
        </div>
        <span className="flex-1 text-slate-600 dark:text-zinc-300">
          {isRunning
            ? t("playground.timeline.running", "Agent working...")
            : t("playground.timeline.completed", "{{count}} steps completed", { count: stepCount })}
        </span>
        <motion.div
          animate={{ rotate: isExpanded ? 180 : 0 }}
          transition={{ duration: 0.2 }}
        >
          <ChevronDown className="h-4 w-4 text-blue-500/60" />
        </motion.div>
      </button>

      {/* Timeline Content */}
      <AnimatePresence>
        {isExpanded && (
          <motion.div
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.25, ease: "easeInOut" }}
            className="overflow-hidden"
          >
            <div className="pt-3 pl-2">
              <AgentTimeline
                state={timeline}
                compact
              />
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}
