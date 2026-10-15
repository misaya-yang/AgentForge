/**
 * Permissions tab of the dataset detail page (visibility, permission info,
 * usage guide).
 *
 * Extracted verbatim from DatasetDetail.tsx (C2 split); no behavior change.
 * `dsQuery.data` → `dataset` prop and `dsQuery.refetch()` → `onDatasetRefetch`.
 * The shell keeps its own visibilityIcons for the header badge; this tab
 * keeps a local copy for the permission-info row, as in the original.
 */

import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Globe, Lock, Users, User, HelpCircle, Bot, Brain, Code } from "lucide-react";

import { updateDataset } from "@/api/knowledge";
import type { Dataset } from "@/types/knowledge";

import { Card } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { toast } from "@/hooks/use-toast";

interface PermissionsTabProps {
  datasetId?: string;
  dataset?: Dataset;
  onDatasetRefetch: () => void;
}

export function PermissionsTab({ datasetId, dataset, onDatasetRefetch }: PermissionsTabProps) {
  const { t } = useTranslation();

  const visibilityIcons: Record<string, ReactNode> = {
    private: <Lock className="h-4 w-4" />,
    tenant: <Users className="h-4 w-4" />,
    public: <Globe className="h-4 w-4" />,
  };

  return (
    <div className="max-w-3xl space-y-6">
      {/* 可见性设置 */}
      <Card className="p-5">
        <div className="flex items-center justify-between mb-4">
          <h3 className="font-semibold text-foreground flex items-center gap-2">
            <Globe className="h-5 w-5 text-blue-500" />
            {t("knowledge.detail.accessPermission")}
          </h3>
        </div>

        <div className="space-y-4">
          <p className="text-sm text-muted-foreground">
            {t("knowledge.detail.accessPermissionHint")}
          </p>

          <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
            {[
              { id: "private", name: t("knowledge.detail.permPrivate"), desc: t("knowledge.detail.permPrivateDesc"), icon: Lock },
              { id: "tenant", name: t("knowledge.detail.permTenant"), desc: t("knowledge.detail.permTenantDesc"), icon: Users },
              { id: "public", name: t("knowledge.detail.permPublic"), desc: t("knowledge.detail.permPublicDesc"), icon: Globe },
            ].map((opt) => {
              const Icon = opt.icon;
              const currentVisibility = dataset?.visibility || "private";
              const isSelected = currentVisibility === opt.id;
              return (
                <Card
                  key={opt.id}
                  className={`p-4 cursor-pointer transition-[border-color,background-color] duration-150 ${
                    isSelected
                      ? "border-2 border-primary bg-primary/5"
                      : "border hover:border-primary/30"
                  }`}
                  onClick={async () => {
                    if (dataset && opt.id !== currentVisibility) {
                      try {
                        await updateDataset(datasetId!, { visibility: opt.id as "private" | "tenant" | "public" });
                        onDatasetRefetch();
                        toast.success(t("knowledge.detail.permUpdated"));
                      } catch (e) {
                        toast.error(t("knowledge.detail.permUpdateFailed"), e instanceof Error ? e.message : String(e));
                      }
                    }
                  }}
                >
                  <div className="flex items-center gap-2">
                    <Icon className={`h-4 w-4 ${isSelected ? "text-primary" : "text-muted-foreground"}`} />
                    <span className="text-sm font-medium">{opt.name}</span>
                  </div>
                  <p className="text-xs text-muted-foreground mt-1">{opt.desc}</p>
                </Card>
              );
            })}
          </div>
        </div>
      </Card>

      {/* 当前权限信息 */}
      <Card className="p-5">
        <div className="flex items-center justify-between mb-4">
          <h3 className="font-semibold text-foreground flex items-center gap-2">
            <User className="h-5 w-5 text-emerald-500" />
            {t("knowledge.detail.permInfo")}
          </h3>
        </div>

        <div className="space-y-4">
          <div className="grid grid-cols-2 gap-4">
            <div className="p-3 bg-muted/40 rounded-lg">
              <Label className="text-xs text-muted-foreground">{t("knowledge.detail.creator")}</Label>
              <p className="text-sm font-medium mt-1">{dataset?.created_by || t("knowledge.detail.unknown")}</p>
            </div>
            <div className="p-3 bg-muted/40 rounded-lg">
              <Label className="text-xs text-muted-foreground">{t("knowledge.detail.currentVisibility")}</Label>
              <div className="flex items-center gap-2 mt-1">
                {visibilityIcons[dataset?.visibility as keyof typeof visibilityIcons] || <Lock className="h-4 w-4" />}
                <span className="text-sm font-medium">
                  {dataset?.visibility === "private" && t("knowledge.detail.permPrivate")}
                  {dataset?.visibility === "tenant" && t("knowledge.detail.permTenant")}
                  {dataset?.visibility === "public" && t("knowledge.detail.permPublic")}
                  {!dataset?.visibility && t("knowledge.detail.permPrivate")}
                </span>
              </div>
            </div>
          </div>

          <div className="p-4 bg-blue-50 dark:bg-blue-950/30 border border-blue-200 dark:border-blue-900/60 rounded-lg">
            <p className="text-sm text-blue-700 dark:text-blue-300">
              <span className="font-medium">{t("knowledge.detail.hint")}</span>
              {dataset?.visibility === "private" && t("knowledge.detail.permPrivateHint")}
              {dataset?.visibility === "tenant" && t("knowledge.detail.permTenantHint")}
              {dataset?.visibility === "public" && t("knowledge.detail.permPublicHint")}
              {!dataset?.visibility && t("knowledge.detail.permPrivateHint")}
            </p>
          </div>
        </div>
      </Card>

      {/* 使用说明 */}
      <Card className="p-5">
        <div className="flex items-center justify-between mb-4">
          <h3 className="font-semibold text-foreground flex items-center gap-2">
            <HelpCircle className="h-5 w-5 text-amber-500" />
            {t("knowledge.detail.usageGuide")}
          </h3>
        </div>

        <div className="space-y-3 text-sm text-muted-foreground">
          <div className="flex items-start gap-3">
            <div className="w-6 h-6 rounded-full bg-primary/10 flex items-center justify-center shrink-0 mt-0.5">
              <Bot className="h-3.5 w-3.5 text-primary" />
            </div>
            <div>
              <p className="font-medium text-foreground">{t("knowledge.detail.aiAssistant")}</p>
              <p className="mt-0.5">{t("knowledge.detail.aiAssistantHint")}</p>
            </div>
          </div>
          <div className="flex items-start gap-3">
            <div className="w-6 h-6 rounded-full bg-emerald-500/10 flex items-center justify-center shrink-0 mt-0.5">
              <Brain className="h-3.5 w-3.5 text-emerald-500" />
            </div>
            <div>
              <p className="font-medium text-foreground">{t("knowledge.detail.langGraphAgent")}</p>
              <p className="mt-0.5">{t("knowledge.detail.langGraphAgentHint")}</p>
            </div>
          </div>
          <div className="flex items-start gap-3">
            <div className="w-6 h-6 rounded-full bg-violet-500/10 flex items-center justify-center shrink-0 mt-0.5">
              <Code className="h-3.5 w-3.5 text-violet-500" />
            </div>
            <div>
              <p className="font-medium text-foreground">{t("knowledge.detail.apiCall")}</p>
              <p className="mt-0.5">{t("knowledge.detail.apiCallHint")} <code className="text-xs bg-muted px-1 py-0.5 rounded">/api/v1/knowledge/{dataset?.dataset_id || "{dataset_id}"}/retrieve</code></p>
            </div>
          </div>
        </div>
      </Card>
    </div>
  );
}
