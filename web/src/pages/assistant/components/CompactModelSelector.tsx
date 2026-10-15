/**
 * Compact Model Selector Component
 *
 * Pill-style dropdown for model selection in the input area control bar.
 */

import { useTranslation } from "react-i18next";
import { ChevronDown, Check } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Badge } from "@/components/ui/badge";
import {
  groupModelsByProvider,
  getProviderDisplayName,
  type ModelInfo,
} from "@/api/assistant";

import { assistantModelKey, resolveAssistantModel } from "../modelSelection";

interface CompactModelSelectorProps {
  models: ModelInfo[];
  selectedModel: string;
  onSelect: (modelId: string) => void;
  disabled?: boolean;
}

export function CompactModelSelector({
  models,
  selectedModel,
  onSelect,
  disabled,
}: CompactModelSelectorProps) {
  const { t } = useTranslation();
  const groupedModels = groupModelsByProvider(models);
  const selectedModelInfo = resolveAssistantModel(models, selectedModel);

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          className="h-8 px-2 gap-1.5 rounded-md hover:bg-[hsl(var(--assistant-surface-soft))] transition-colors duration-150"
          disabled={disabled || models.length === 0}
        >
          <span className="text-[13px] font-medium text-[hsl(var(--assistant-text-primary))] max-w-[180px] truncate">
            {selectedModelInfo ? `${selectedModelInfo.name} · ${getProviderDisplayName(selectedModelInfo.provider)}` : t("assistant.selectModel", "Select model")}
          </span>
          <ChevronDown className="h-3.5 w-3.5 text-[hsl(var(--assistant-text-tertiary))]" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="start"
        className="w-[260px] max-h-[350px] overflow-y-auto rounded-lg"
      >
        {Object.entries(groupedModels).map(([provider, providerModels], index) => (
          <div key={provider}>
            {index > 0 && <DropdownMenuSeparator />}
            <DropdownMenuLabel className="text-xs text-slate-500 font-medium">
              {getProviderDisplayName(provider)}
            </DropdownMenuLabel>
            {providerModels.map((model) => (
              <DropdownMenuItem
                key={assistantModelKey(model)}
                className="flex items-center justify-between gap-2 cursor-pointer rounded-lg"
                onClick={() => onSelect(assistantModelKey(model))}
              >
                <div className="flex items-center gap-2 min-w-0">
                  {selectedModelInfo === model ? (
                    <Check className="h-4 w-4 shrink-0 text-[hsl(var(--assistant-accent))]" />
                  ) : (
                    <div className="w-4" />
                  )}
                  <span className="truncate text-sm">{model.name}</span>
                </div>
                <div className="flex items-center gap-1">
                  {model.access_level === "admin" && (
                    <Badge variant="secondary" className="text-[9px] px-1 py-0 bg-amber-100 dark:bg-amber-900/50 text-amber-700 dark:text-amber-300">
                      Admin
                    </Badge>
                  )}
                  {model.access_level === "premium" && (
                    <Badge variant="secondary" className="text-[9px] px-1 py-0 bg-blue-100 dark:bg-blue-900/50 text-blue-700 dark:text-blue-300">
                      Pro
                    </Badge>
                  )}
                  {model.supports_vision && (
                    <Badge variant="secondary" className="text-[9px] px-1 py-0 bg-purple-100 dark:bg-purple-900/50 text-purple-700 dark:text-purple-300">
                      Vision
                    </Badge>
                  )}
                </div>
              </DropdownMenuItem>
            ))}
          </div>
        ))}
        {models.length === 0 && (
          <div className="px-2 py-4 text-sm text-slate-500 text-center">
            {t("assistant.noModels", "No models available")}
          </div>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
