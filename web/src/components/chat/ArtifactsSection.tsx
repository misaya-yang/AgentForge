import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";
import { ArtifactList, type ArtifactData } from "@/components/agent/ArtifactCard";
import { Sparkles } from "lucide-react";

export function ArtifactsSection({ artifacts }: { artifacts: ArtifactData[] }) {
  const { t } = useTranslation();

  if (!artifacts || artifacts.length === 0) return null;

  return (
    <motion.div
      initial={{ opacity: 0, y: 8 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ delay: 0.2 }}
      className="mt-4 pt-4 border-t border-slate-200/50 dark:border-zinc-700/50"
    >
      <div className="flex items-center gap-2 text-[10px] font-semibold uppercase tracking-wider text-emerald-600 dark:text-emerald-400 mb-3">
        <Sparkles className="h-3 w-3" />
        {t("playground.artifacts", "Generated Files")}
      </div>
      <ArtifactList
        artifacts={artifacts}
        variant="compact"
        onArtifactClick={(artifact) => {
          if (artifact.url) {
            window.open(artifact.url, "_blank");
          }
        }}
      />
    </motion.div>
  );
}
