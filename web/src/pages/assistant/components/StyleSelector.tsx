/**
 * Style Selector Component
 *
 * Modal dialog for selecting assistant personality style.
 * Inspired by Grok's style customization feature.
 */

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Settings2, Check } from "lucide-react";
import { cn } from "@/lib/utils";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { ASSISTANT_STYLES, type AssistantStyle } from "../styles";

interface StyleSelectorProps {
  selectedStyle: string;
  onSelect: (styleId: string) => void;
  disabled?: boolean;
}

export function StyleSelector({
  selectedStyle,
  onSelect,
  disabled,
}: StyleSelectorProps) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const currentStyle = ASSISTANT_STYLES.find((s) => s.id === selectedStyle);

  const handleSelect = (styleId: string) => {
    onSelect(styleId);
    setOpen(false);
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          className="h-7 px-2 gap-1.5 rounded-md bg-transparent hover:bg-[hsl(var(--assistant-surface-soft))] text-[hsl(var(--assistant-text-secondary))] hover:text-[hsl(var(--assistant-text-primary))] transition-colors duration-150"
          disabled={disabled}
        >
          <Settings2 className="h-3.5 w-3.5 text-[hsl(var(--assistant-text-tertiary))]" />
          <span className="text-[12.5px] font-medium">
            {currentStyle ? t(currentStyle.nameKey) : ""}
          </span>
        </Button>
      </DialogTrigger>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="text-center">
            {t("assistant.styleTitle")}
          </DialogTitle>
        </DialogHeader>
        <div className="grid grid-cols-1 gap-3 py-4">
          {ASSISTANT_STYLES.map((style) => (
            <StyleCard
              key={style.id}
              style={style}
              isSelected={selectedStyle === style.id}
              onClick={() => handleSelect(style.id)}
            />
          ))}
        </div>
        <p className="text-xs text-center text-slate-500 dark:text-slate-400">
          {t("assistant.styleHint")}
        </p>
      </DialogContent>
    </Dialog>
  );
}

function StyleCard({
  style,
  isSelected,
  onClick,
}: {
  style: AssistantStyle;
  isSelected: boolean;
  onClick: () => void;
}) {
  const { t } = useTranslation();
  return (
    <button
      onClick={onClick}
      className={cn(
        "relative flex items-start gap-3 p-4 rounded-lg border text-left transition-colors duration-150",
        isSelected
          ? "border-[hsl(var(--assistant-accent))]/50 bg-[hsl(var(--assistant-accent))]/8"
          : "border-[hsl(var(--assistant-border))] hover:border-[hsl(var(--assistant-border))]/80 bg-[hsl(var(--assistant-surface-bg))] hover:bg-[hsl(var(--assistant-surface-soft))]"
      )}
    >
      {isSelected && (
        <div className="absolute top-3 right-3">
          <Check className="h-4 w-4 text-[hsl(var(--assistant-accent))]" />
        </div>
      )}
      <div className="flex-1 pr-6">
        <div className="font-medium text-[hsl(var(--assistant-text-primary))]">
          {t(style.nameKey)}
        </div>
        <div className="text-sm text-[hsl(var(--assistant-text-secondary))] mt-1">
          {t(style.descriptionKey)}
        </div>
      </div>
    </button>
  );
}
