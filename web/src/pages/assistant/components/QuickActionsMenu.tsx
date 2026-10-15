/**
 * Quick Actions Menu Component
 *
 * ChatGPT-style popup menu for quick access to:
 * - File upload (添加照片和文件)
 * - Knowledge base (公司知识库) - with nested KB selector
 * - Web search (网页搜索)
 */

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { motion, AnimatePresence } from "framer-motion";
import { Plus, ImagePlus, Database, Globe, ChevronRight, ChevronLeft, Check, Palette, Link2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";
import type { DatasetInfo } from "@/api/assistant";

interface QuickActionsMenuProps {
  onFileUpload: () => void;
  onImageGenerate: () => void;
  onToggleWebSearch: () => void;
  onOpenConnectors?: () => void;
  webSearchEnabled: boolean;
  kbAvailable: boolean;
  webSearchAvailable: boolean;
  disabled: boolean;
  connectorCount?: number;
  // KB selection props
  datasets: DatasetInfo[];
  selectedDatasets: string[];
  onToggleDataset: (datasetId: string) => void;
}

export function QuickActionsMenu({
  onFileUpload,
  onImageGenerate,
  onToggleWebSearch,
  onOpenConnectors,
  webSearchEnabled,
  kbAvailable,
  webSearchAvailable,
  disabled,
  connectorCount = 0,
  datasets,
  selectedDatasets,
  onToggleDataset,
}: QuickActionsMenuProps) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [showKBList, setShowKBList] = useState(false);

  const selectedKBCount = selectedDatasets.length;

  return (
    <Popover open={open} onOpenChange={(value) => {
      setOpen(value);
      if (!value) setShowKBList(false); // Reset when closing
    }}>
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className={cn(
            "h-9 w-9 rounded-md transition-colors duration-150",
            open
              ? "bg-[hsl(var(--assistant-surface-soft))] text-[hsl(var(--assistant-text-primary))]"
              : "hover:bg-[hsl(var(--assistant-surface-soft))] text-[hsl(var(--assistant-text-secondary))]"
          )}
          disabled={disabled}
          aria-label={t("assistant.addFilesAndSources", "Add files and sources")}
        >
          <motion.div
            animate={{ rotate: open ? 45 : 0 }}
            transition={{ duration: 0.15, ease: [0.16, 1, 0.3, 1] }}
          >
            <Plus className="h-4 w-4" />
          </motion.div>
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="start"
        side="top"
        className="w-64 p-1.5 rounded-xl border-slate-200 dark:border-slate-700 shadow-xl"
      >
        <AnimatePresence mode="wait">
          {!showKBList ? (
            // Main menu
            <motion.div
              key="main"
              initial={{ opacity: 0, x: -10 }}
              animate={{ opacity: 1, x: 0 }}
              exit={{ opacity: 0, x: -10 }}
              transition={{ duration: 0.15 }}
              className="space-y-0.5"
            >
              {/* File Upload - 添加照片和文件 */}
              <button
                onClick={() => {
                  onFileUpload();
                  setOpen(false);
                }}
                className="w-full flex items-center gap-3 px-3 py-2.5 rounded-lg hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors text-left group"
              >
                <ImagePlus className="h-5 w-5 text-slate-500 dark:text-slate-400" />
                <span className="flex-1 text-sm text-slate-700 dark:text-slate-200">
                  {t("assistant.addPhotosFiles")}
                </span>
              </button>

              {/* Image Generation - 生成图片 */}
              <button
                onClick={() => {
                  onImageGenerate();
                  setOpen(false);
                }}
                className="w-full flex items-center gap-3 px-3 py-2.5 rounded-lg hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors text-left group"
              >
                <Palette className="h-5 w-5 text-violet-500" />
                <span className="flex-1 text-sm text-slate-700 dark:text-slate-200">
                  {t("assistant.generateImage")}
                </span>
              </button>

              {/* Knowledge Base - 公司知识库 */}
              {kbAvailable && datasets.length > 0 && (
                <button
                  onClick={() => setShowKBList(true)}
                  className="w-full flex items-center gap-3 px-3 py-2.5 rounded-lg hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors text-left group"
                >
                  <Database className={cn(
                    "h-5 w-5",
                    selectedKBCount > 0
                      ? "text-emerald-500"
                      : "text-slate-500 dark:text-slate-400"
                  )} />
                  <span className="flex-1 text-sm text-slate-700 dark:text-slate-200">
                    {t("assistant.companyKB")}
                  </span>
                  {selectedKBCount > 0 ? (
                    <Badge className="bg-emerald-100 dark:bg-emerald-900/50 text-emerald-700 dark:text-emerald-300 text-[10px] px-1.5 py-0">
                      {selectedKBCount}
                    </Badge>
                  ) : (
                    <ChevronRight className="h-4 w-4 text-slate-400" />
                  )}
                </button>
              )}

              {/* Web Search - 网页搜索 */}
              {webSearchAvailable && (
                <button
                  onClick={() => {
                    onToggleWebSearch();
                    setOpen(false);
                  }}
                  className="w-full flex items-center gap-3 px-3 py-2.5 rounded-lg hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors text-left group"
                >
                  <Globe className={cn(
                    "h-5 w-5",
                    webSearchEnabled
                      ? "text-blue-500"
                      : "text-slate-500 dark:text-slate-400"
                  )} />
                  <span className="flex-1 text-sm text-slate-700 dark:text-slate-200">
                    {t("assistant.webSearchMenu")}
                  </span>
                  {webSearchEnabled && (
                    <Badge className="bg-blue-100 dark:bg-blue-900/50 text-blue-700 dark:text-blue-300 text-[10px] px-1.5 py-0">
                      ON
                    </Badge>
                  )}
                </button>
              )}

              {/* Connectors - 连接器 (Confluence, Outlook, GitHub, etc.) */}
              {onOpenConnectors && (
                <button
                  onClick={() => {
                    onOpenConnectors();
                    setOpen(false);
                  }}
                  className="w-full flex items-center gap-3 px-3 py-2.5 rounded-lg hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors text-left group"
                >
                  <Link2 className={cn(
                    "h-5 w-5",
                    connectorCount > 0
                      ? "text-purple-500"
                      : "text-slate-500 dark:text-slate-400"
                  )} />
                  <span className="flex-1 text-sm text-slate-700 dark:text-slate-200">
                    {t("assistant.connectors", "连接器")}
                  </span>
                  {connectorCount > 0 && (
                    <Badge className="bg-purple-100 dark:bg-purple-900/50 text-purple-700 dark:text-purple-300 text-[10px] px-1.5 py-0">
                      {connectorCount}
                    </Badge>
                  )}
                </button>
              )}
            </motion.div>
          ) : (
            // KB selection list
            <motion.div
              key="kb-list"
              initial={{ opacity: 0, x: 10 }}
              animate={{ opacity: 1, x: 0 }}
              exit={{ opacity: 0, x: 10 }}
              transition={{ duration: 0.15 }}
            >
              {/* Back button */}
              <button
                onClick={() => setShowKBList(false)}
                className="w-full flex items-center gap-2 px-3 py-2 rounded-lg hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors text-left border-b border-slate-100 dark:border-slate-700 mb-1"
              >
                <ChevronLeft className="h-4 w-4 text-slate-400" />
                <span className="text-sm font-medium text-slate-600 dark:text-slate-300">
                  {t("assistant.selectKB")}
                </span>
              </button>

              {/* KB list */}
              <div className="max-h-[200px] overflow-y-auto space-y-0.5">
                {datasets.map((dataset) => {
                  const isSelected = selectedDatasets.includes(dataset.dataset_id);
                  return (
                    <button
                      key={dataset.dataset_id}
                      onClick={() => onToggleDataset(dataset.dataset_id)}
                      aria-pressed={isSelected}
                      className={cn(
                        "w-full flex items-center gap-3 px-3 py-2 rounded-lg transition-colors text-left",
                        isSelected
                          ? "bg-emerald-50 dark:bg-emerald-900/20"
                          : "hover:bg-slate-100 dark:hover:bg-slate-800"
                      )}
                    >
                      <div className={cn(
                        "h-4 w-4 rounded border flex items-center justify-center shrink-0",
                        isSelected
                          ? "bg-emerald-500 border-emerald-500"
                          : "border-slate-300 dark:border-slate-600"
                      )}>
                        {isSelected && <Check className="h-3 w-3 text-white" />}
                      </div>
                      <div className="flex-1 min-w-0">
                        <span className="text-sm text-slate-700 dark:text-slate-200 truncate block">
                          {dataset.name}
                        </span>
                        {dataset.description && (
                          <span className="text-xs text-slate-500 dark:text-slate-400 truncate block">
                            {dataset.description}
                          </span>
                        )}
                      </div>
                    </button>
                  );
                })}
              </div>
            </motion.div>
          )}
        </AnimatePresence>
      </PopoverContent>
    </Popover>
  );
}
