/**
 * Provider Card Component
 *
 * Displays a provider card with status, API key indicator, and actions.
 */

import { useLayoutEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Card, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import {
  Key,
  Settings,
  Trash2,
  Loader2,
  CheckCircle2,
  XCircle,
  Zap,
  RefreshCw,
} from "lucide-react";
import type { Provider } from "@/api/providers";
import { testProviderConnection, getApiTypeDisplayName } from "@/api/providers";

// Provider brand colors for visual differentiation
const PROVIDER_BRANDS: Record<string, { bg: string; text: string; darkBg: string; darkText: string; initial: string }> = {
  openai:          { bg: "bg-emerald-50", text: "text-emerald-700", darkBg: "dark:bg-emerald-950/40", darkText: "dark:text-emerald-400", initial: "OA" },
  anthropic:       { bg: "bg-amber-50", text: "text-amber-700", darkBg: "dark:bg-amber-950/40", darkText: "dark:text-amber-400", initial: "AN" },
  deepseek:        { bg: "bg-indigo-50", text: "text-indigo-700", darkBg: "dark:bg-indigo-950/40", darkText: "dark:text-indigo-400", initial: "DS" },
  // Vertex entry must come BEFORE "google" — getProviderBrand uses a
  // substring match in insertion order, and "google-vertex" contains
  // "google". Without this ordering the Vertex card would inherit the
  // generic Google blue badge.
  "google-vertex": { bg: "bg-cyan-50", text: "text-cyan-700", darkBg: "dark:bg-cyan-950/40", darkText: "dark:text-cyan-400", initial: "GV" },
  google:          { bg: "bg-blue-50", text: "text-blue-700", darkBg: "dark:bg-blue-950/40", darkText: "dark:text-blue-400", initial: "GG" },
  dashscope:       { bg: "bg-orange-50", text: "text-orange-700", darkBg: "dark:bg-orange-950/40", darkText: "dark:text-orange-400", initial: "DS" },
};

function getProviderBrand(id: string) {
  const key = id.toLowerCase();
  for (const [k, v] of Object.entries(PROVIDER_BRANDS)) {
    if (key.includes(k)) return v;
  }
  return { bg: "bg-slate-50", text: "text-slate-700", darkBg: "dark:bg-slate-800", darkText: "dark:text-slate-400", initial: id.slice(0, 2).toUpperCase() };
}

interface ProviderCardProps {
  provider: Provider;
  onEdit?: () => void;
  onDelete?: () => void;
  onSyncModels?: () => void;
  syncingModels?: boolean;
  testResetRevision?: number;
}

export function ProviderCard({
  provider,
  onEdit,
  onDelete,
  onSyncModels,
  syncingModels = false,
  testResetRevision = 0,
}: ProviderCardProps) {
  const { t } = useTranslation();
  // Test probes saved configuration only. updated_at also invalidates feedback
  // after credential-only edits without retaining credential values client-side.
  const testOwner = useMemo(() => ({
    active: true,
    pending: false,
    revision: [provider.tenant_id, provider.provider_id, provider.updated_at, testResetRevision],
  }), [
    provider.tenant_id, provider.provider_id, provider.updated_at, testResetRevision,
  ]);
  useLayoutEffect(() => {
    testOwner.active = true;
    return () => { testOwner.active = false; };
  }, [testOwner]);
  const [pendingOwner, setPendingOwner] = useState<typeof testOwner | null>(null);
  const testing = pendingOwner === testOwner;
  const runtimeReady = provider.has_api_key || provider.allow_environment_credentials;
  const [testFeedback, setTestFeedback] = useState<{
    owner: typeof testOwner;
    result: { success: boolean; message: string; latency_ms?: number };
  } | null>(null);
  const testResult = testFeedback?.owner === testOwner ? testFeedback.result : null;

  const handleTest = async () => {
    if (!testOwner.active || testOwner.pending) return;
    testOwner.pending = true;
    setPendingOwner(testOwner);
    setTestFeedback(null);
    try {
      const result = await testProviderConnection(provider.provider_id);
      if (testOwner.active) setTestFeedback({ owner: testOwner, result });
    } catch (error) {
      if (testOwner.active) setTestFeedback({
        owner: testOwner,
        result: {
          success: false,
          message: error instanceof Error ? error.message : "Connection test failed",
        },
      });
    } finally {
      testOwner.pending = false;
      setPendingOwner(current => current === testOwner ? null : current);
    }
  };

  return (
    <Card
      className={cn(
        "relative overflow-hidden group border bg-card transition-colors duration-200 hover:border-primary/30 hover:bg-muted/10",
        !provider.is_enabled && "opacity-75 grayscale-[0.5] bg-muted/30"
      )}
    >
      <CardContent className="p-4 relative">
        <div className="flex items-center gap-4">
          {/* Provider brand icon */}
          {(() => {
            const brand = getProviderBrand(provider.provider_id);
            return (
              <div className={cn(
                "shrink-0 w-10 h-10 rounded-lg flex items-center justify-center text-sm font-bold transition-colors duration-200",
                brand.bg, brand.text, brand.darkBg, brand.darkText
              )}>
                {brand.initial}
              </div>
            );
          })()}

          {/* Content */}
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2 mb-1">
              <h3 className="font-semibold text-foreground truncate">
                {provider.display_name}
              </h3>
              {/* Status Indicator */}
              <div className="shrink-0" title={provider.is_enabled ? t("common.enabled", "Enabled") : t("common.disabled", "Disabled")}>
                <span className={cn(
                  "inline-flex rounded-full h-2 w-2",
                  provider.is_enabled ? "bg-green-500" : "bg-gray-400"
                )}></span>
              </div>
            </div>
            <p className="font-mono text-xs text-muted-foreground truncate mb-1.5">
              {provider.provider_id}
            </p>
            <div className="flex flex-wrap items-center gap-1.5">
              <Badge variant="secondary" className="bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300 text-[10px] font-semibold border border-slate-200 dark:border-slate-700 px-1.5 py-0">
                {getApiTypeDisplayName(provider.api_type)}
              </Badge>
              <Badge
                variant="outline"
                className={cn(
                  "text-[10px] font-semibold border px-1.5 py-0",
                  runtimeReady
                    ? "bg-green-50 text-green-700 border-green-200 dark:bg-green-950/30 dark:text-green-300 dark:border-green-800"
                    : "bg-red-50 text-red-700 border-red-200 dark:bg-red-950/30 dark:text-red-300 dark:border-red-800"
                )}
              >
                <Key className="h-2.5 w-2.5 mr-1" />
                {runtimeReady ? t("providers.apiKey.configured", "Configured") : t("providers.apiKey.missing", "Missing Key")}
              </Badge>
            </div>
          </div>

          {/* Actions stay reachable on touch and become visible on keyboard focus. */}
          <div className="flex shrink-0 items-center gap-1 opacity-100 transition-opacity duration-150 md:opacity-0 md:group-focus-within:opacity-100 md:group-hover:opacity-100 [@media(hover:none)]:opacity-100">
            <Button
              variant="ghost"
              size="icon"
              onClick={onSyncModels}
              disabled={syncingModels}
              className="h-8 w-8 text-muted-foreground hover:text-foreground hover:bg-muted"
              title={t("providers.actions.syncModels", "Sync models")}
              aria-label={t("providers.actions.syncModels", "Sync models")}
            >
              <RefreshCw className={cn("h-4 w-4", syncingModels && "animate-spin")} />
            </Button>
            <Button
              variant="ghost"
              size="icon"
              onClick={handleTest}
              disabled={testing || !runtimeReady}
              className="h-8 w-8 text-muted-foreground hover:text-foreground hover:bg-muted"
              title={t("common.test", "Test")}
              aria-label={t("common.test", "Test")}
            >
              {testing ? (
                <Loader2 className="h-4 w-4 animate-spin" />
              ) : (
                <Zap className="h-4 w-4" />
              )}
            </Button>
            <Button
              variant="ghost"
              size="icon"
              className="h-8 w-8 text-muted-foreground hover:text-foreground hover:bg-muted"
              onClick={onEdit}
              title={t("common.configure", "Configure")}
              aria-label={t("common.configure", "Configure")}
            >
              <Settings className="h-4 w-4" />
            </Button>
            <Button
              variant="ghost"
              size="icon"
              onClick={onDelete}
              className="h-8 w-8 text-muted-foreground hover:text-destructive hover:bg-red-50 dark:hover:bg-red-950/20"
              title={t("common.delete", "Delete")}
              aria-label={t("common.delete", "Delete")}
            >
              <Trash2 className="h-4 w-4" />
            </Button>
          </div>
        </div>

        {/* Base URL - shown below if exists */}
        {provider.base_url && (
          <p className="text-xs text-muted-foreground mt-3 pt-3 border-t border-border/50 truncate" title={provider.base_url}>
            {provider.base_url}
          </p>
        )}

        {/* Test Result */}
        {testResult && (
          <div
            className={cn(
              "mt-3 p-2 rounded-lg text-xs font-medium flex items-center gap-2 animate-in fade-in slide-in-from-top-2",
              testResult.success
                ? "bg-green-50 text-green-700 dark:bg-green-950/30 dark:text-green-300 border border-green-200 dark:border-green-900"
                : "bg-red-50 text-red-700 dark:bg-red-950/30 dark:text-red-300 border border-red-200 dark:border-red-900"
            )}
          >
            {testResult.success ? (
              <CheckCircle2 className="h-3.5 w-3.5 shrink-0" />
            ) : (
              <XCircle className="h-3.5 w-3.5 shrink-0" />
            )}
            <span className="flex-1 truncate">{testResult.message}</span>
            {testResult.latency_ms && (
              <span className="opacity-70 tabular-nums shrink-0">{testResult.latency_ms}ms</span>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  );
}
