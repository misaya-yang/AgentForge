/**
 * SlideOutlinePreview - Manus-style slide outline preview component
 *
 * Displays a collapsible preview of PPT slide structure with:
 * - Presentation title and slide count
 * - Numbered slide list with titles
 * - Expand/collapse functionality
 * - Icon indicators for slide types
 */

import { useState } from "react";
import { motion, AnimatePresence } from "framer-motion";
import {
  ChevronDown,
  ChevronUp,
  Presentation,
  FileText,
  LayoutGrid,
  Columns,
  StickyNote,
} from "lucide-react";
import { cn } from "@/lib/utils";
import type { SlideOutline, SlideOutlineItem } from "../types";
import { useTranslation } from "react-i18next";

// =============================================================================
// Props
// =============================================================================

interface SlideOutlinePreviewProps {
  outline: SlideOutline;
  isGenerating?: boolean;
  className?: string;
}

// =============================================================================
// Slide Type Icons
// =============================================================================

const SlideTypeIcon = ({ type }: { type: SlideOutlineItem["type"] }) => {
  const iconClass = "h-4 w-4 text-slate-500";
  switch (type) {
    case "title":
      return <Presentation className={iconClass} />;
    case "content":
      return <FileText className={iconClass} />;
    case "two_column":
      return <Columns className={iconClass} />;
    case "section":
      return <LayoutGrid className={iconClass} />;
    case "blank":
      return <StickyNote className={iconClass} />;
    default:
      return <FileText className={iconClass} />;
  }
};

// =============================================================================
// Main Component
// =============================================================================

export function SlideOutlinePreview({
  outline,
  isGenerating = false,
  className,
}: SlideOutlinePreviewProps) {
  const { t } = useTranslation();
  const [isExpanded, setIsExpanded] = useState(true);

  return (
    <div
      className={cn(
        "rounded-xl border border-slate-200 dark:border-slate-700 bg-slate-50 dark:bg-slate-800/50 overflow-hidden",
        className
      )}
    >
      {/* Header */}
      <button
        onClick={() => setIsExpanded(!isExpanded)}
        className="w-full flex items-center justify-between p-4 hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors"
      >
        <div className="flex items-center gap-3">
          {/* Icon */}
          <div className="w-10 h-10 rounded-lg bg-orange-100 dark:bg-orange-900/30 flex items-center justify-center">
            <Presentation className="h-5 w-5 text-orange-600 dark:text-orange-400" />
          </div>

          {/* Title and info */}
          <div className="text-left">
            <h4 className="font-medium text-slate-900 dark:text-slate-100">
              {t("assistant.slideOutline.title")}
            </h4>
            <p className="text-sm text-slate-500 dark:text-slate-400">
              {t("assistant.slideOutline.subtitle", { title: outline.title, count: outline.totalSlides })}
            </p>
          </div>
        </div>

        {/* Expand/collapse indicator */}
        <div className="flex items-center gap-2">
          {isGenerating && (
            <span className="text-xs text-blue-600 dark:text-blue-400 bg-blue-100 dark:bg-blue-900/30 px-2 py-1 rounded">
              {t("assistant.generating")}
            </span>
          )}
          <span className="text-sm text-slate-500 dark:text-slate-400">
            {isExpanded ? t("common.collapse") : t("common.expand")}
          </span>
          {isExpanded ? (
            <ChevronUp className="h-5 w-5 text-slate-400" />
          ) : (
            <ChevronDown className="h-5 w-5 text-slate-400" />
          )}
        </div>
      </button>

      {/* Slide list */}
      <AnimatePresence initial={false}>
        {isExpanded && (
          <motion.div
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.2 }}
            className="overflow-hidden"
          >
            <div className="px-4 pb-4 space-y-1">
              {outline.slides.map((slide, index) => (
                <motion.div
                  key={slide.number}
                  initial={{ opacity: 0, x: -10 }}
                  animate={{ opacity: 1, x: 0 }}
                  transition={{ delay: index * 0.03 }}
                  className="flex items-center gap-3 p-2 rounded-lg hover:bg-slate-100 dark:hover:bg-slate-700/50 transition-colors"
                >
                  {/* Slide number */}
                  <div className="w-6 h-6 rounded-full bg-slate-200 dark:bg-slate-700 flex items-center justify-center shrink-0">
                    <span className="text-xs font-medium text-slate-600 dark:text-slate-300">
                      {slide.number}
                    </span>
                  </div>

                  {/* Slide type icon */}
                  <SlideTypeIcon type={slide.type} />

                  {/* Slide title */}
                  <div className="flex-1 min-w-0">
                    <p className="text-sm font-medium text-slate-700 dark:text-slate-200 truncate">
                      {slide.title}
                    </p>
                    {slide.subtitle && (
                      <p className="text-xs text-slate-500 dark:text-slate-400 truncate">
                        {slide.subtitle}
                      </p>
                    )}
                  </div>

                  {/* Bullet count badge */}
                  {slide.bulletCount && slide.bulletCount > 0 && (
                    <span className="text-xs text-slate-400 dark:text-slate-500 bg-slate-200 dark:bg-slate-700 px-1.5 py-0.5 rounded">
                      {t("assistant.slideOutline.points", { count: slide.bulletCount })}
                    </span>
                  )}
                </motion.div>
              ))}
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

export default SlideOutlinePreview;
