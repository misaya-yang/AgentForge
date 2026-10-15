/**
 * Model Table Component
 *
 * Table for displaying and managing LLM models.
 */

import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import { Eye, Wrench, Pencil, Trash2 } from "lucide-react";
import type { LLMModel } from "@/api/models";
import {
  formatContextWindow,
  formatPrice,
  getAccessLevelDisplayName,
} from "@/api/models";

interface ModelTableProps {
  models: LLMModel[];
  providers: Record<string, string>; // provider_id -> display_name
  onEdit?: (model: LLMModel) => void;
  onDelete?: (model: LLMModel) => void;
  onToggle?: (model: LLMModel, enabled: boolean) => void | Promise<void>;
  loading?: boolean;
}

export function ModelTable({
  models,
  providers,
  onEdit,
  onDelete,
  onToggle,
  loading,
}: ModelTableProps) {
  const { t } = useTranslation();
  const pending = useRef(new Set<string>());
  const [togglingIds, setTogglingIds] = useState<Set<string>>(new Set());
  const rowKey = (model: LLMModel) => JSON.stringify([model.tenant_id, model.provider_id, model.model_id]);

  const handleToggle = async (model: LLMModel, enabled: boolean) => {
    const key = rowKey(model);
    if (!onToggle || loading || pending.current.has(key)) return;
    pending.current.add(key);
    setTogglingIds(new Set(pending.current));
    try {
      await onToggle(model, enabled);
    } catch {
      // The owning page reports mutation errors; consume the event promise here.
    } finally {
      pending.current.delete(key);
      setTogglingIds(new Set(pending.current));
    }
  };

  if (models.length === 0) {
    return (
      <div className="text-center py-12 text-muted-foreground">
        {t("models.empty")}
      </div>
    );
  }

  return (
    <div className="w-full">
      <Table>
        <TableHeader>
          <TableRow className="border-b border-border hover:bg-transparent">
            <TableHead className="w-[200px] text-muted-foreground font-medium">{t("models.table.model")}</TableHead>
            <TableHead className="text-muted-foreground font-medium">{t("models.table.provider")}</TableHead>
            <TableHead className="text-center text-muted-foreground font-medium">{t("models.table.context")}</TableHead>
            <TableHead className="text-center text-muted-foreground font-medium">{t("models.table.outputLimit")}</TableHead>
            <TableHead className="text-center text-muted-foreground font-medium">{t("models.table.price")}</TableHead>
            <TableHead className="text-center text-muted-foreground font-medium">{t("models.table.capabilities")}</TableHead>
            <TableHead className="text-center text-muted-foreground font-medium">{t("models.table.accessLevel")}</TableHead>
            <TableHead className="text-center text-muted-foreground font-medium">{t("models.table.status")}</TableHead>
            <TableHead className="text-right text-muted-foreground font-medium">{t("models.table.actions")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {models.map((model) => (
            <TableRow
              key={rowKey(model)}
              className={cn(
                "border-b border-border transition-colors hover:bg-muted/50 data-[state=selected]:bg-muted",
                !model.is_enabled && "opacity-60 bg-muted/20"
              )}
            >
              <TableCell className="font-medium">
                <div>
                  <div className="text-foreground font-semibold">{model.display_name}</div>
                  <div className="text-xs text-muted-foreground font-mono mt-0.5">
                    {model.model_id}
                  </div>
                </div>
              </TableCell>
              <TableCell>
                <div className="flex items-center gap-2">
                  <div className="h-2 w-2 rounded-full bg-primary/20" />
                  <span className="font-medium text-sm">{providers[model.provider_id] || model.provider_id}</span>
                </div>
              </TableCell>
              <TableCell className="text-center text-foreground font-mono text-sm">
                {formatContextWindow(model.context_window)}
              </TableCell>
              <TableCell className="text-center text-foreground font-mono text-sm">
                {formatContextWindow(model.max_output_tokens)}
              </TableCell>
              <TableCell className="text-center">
                <div className="text-xs text-muted-foreground">
                  <div className="flex items-center justify-center gap-1">{t("models.price.input", "In")}: <span className="font-mono text-foreground font-medium">{formatPrice(model.input_price_per_1k)}</span></div>
                  <div className="flex items-center justify-center gap-1">{t("models.price.output", "Out")}: <span className="font-mono text-foreground font-medium">{formatPrice(model.output_price_per_1k)}</span></div>
                </div>
              </TableCell>
              <TableCell className="text-center">
                <div className="flex items-center justify-center gap-1">
                  {model.supports_vision && (
                    <Badge variant="outline" className="text-[10px] px-1.5 border-purple-200 bg-purple-50 text-purple-700 dark:bg-purple-900/30 dark:text-purple-300 dark:border-purple-800 font-semibold shadow-xs">
                      <Eye className="h-3 w-3 mr-1" />
                      {t("models.capabilities.vision")}
                    </Badge>
                  )}
                  {model.supports_tools && (
                    <Badge variant="outline" className="text-[10px] px-1.5 border-blue-200 bg-blue-50 text-blue-700 dark:bg-blue-900/30 dark:text-blue-300 dark:border-blue-800 font-semibold shadow-xs">
                      <Wrench className="h-3 w-3 mr-1" />
                      {t("models.capabilities.tools")}
                    </Badge>
                  )}
                </div>
              </TableCell>
              <TableCell className="text-center">
                <Badge
                  variant="secondary"
                  className={cn(
                    "text-[10px] font-semibold border shadow-xs",
                    model.access_level === "admin" &&
                    "bg-orange-50 text-orange-700 border-orange-200 dark:bg-orange-950/30 dark:text-orange-300 dark:border-orange-800",
                    model.access_level === "premium" &&
                    "bg-indigo-50 text-indigo-700 border-indigo-200 dark:bg-indigo-950/30 dark:text-indigo-300 dark:border-indigo-800",
                    model.access_level === "public" &&
                    "bg-teal-50 text-teal-700 border-teal-200 dark:bg-teal-950/30 dark:text-teal-300 dark:border-teal-800"
                  )}
                >
                  {getAccessLevelDisplayName(model.access_level)}
                </Badge>
              </TableCell>
              <TableCell className="text-center">
                <Switch
                  checked={model.is_enabled}
                  onCheckedChange={(checked) => handleToggle(model, checked)}
                  disabled={loading || !onToggle || togglingIds.has(rowKey(model))}
                  className="data-[state=checked]:bg-green-500"
                />
              </TableCell>
              <TableCell className="text-right">
                <div className="flex items-center justify-end gap-2">
                  <Button
                    variant="outline"
                    size="sm"
                    className="h-8 px-2.5 text-xs font-medium border-primary/20 hover:bg-primary/5 hover:text-primary transition-colors"
                    onClick={() => onEdit?.(model)}
                  >
                    <Pencil className="h-3.5 w-3.5 mr-1.5" />
                    {t("common.configure")}
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-8 w-8 text-muted-foreground/50 hover:text-destructive hover:bg-destructive/10 transition-colors"
                    onClick={() => onDelete?.(model)}
                    aria-label={t("common.delete")}
                    title={t("common.delete")}
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </Button>
                </div>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  );
}
