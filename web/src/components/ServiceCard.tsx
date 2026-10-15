import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";
import type { HealthStatus, ServiceDefinition, ServiceType } from "@/types/gateway";
import { Card, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ServiceConfigDialog } from "@/components/ServiceConfigDialog";
import { cn } from "@/lib/utils";
import { useAppStore } from "@/store/useAppStore";
import {
  MessageSquare,
  Sparkles,
  Cog,
  BarChart3,
  Tag,
  GitBranch,
  Wrench,
  Package,
  Settings,
  Bot,
  Globe,
} from "lucide-react";

// Simple icon components for service types
function ServiceIcon({ serviceType }: { serviceType: ServiceType }) {
  const iconProps = { className: "h-6 w-6" };

  switch (serviceType) {
    case "assistant":
      return <Bot {...iconProps} />;
    case "conversational":
      return <MessageSquare {...iconProps} />;
    case "generative":
      return <Sparkles {...iconProps} />;
    case "processing":
      return <Cog {...iconProps} />;
    case "embedding":
      return <BarChart3 {...iconProps} />;
    case "classification":
      return <Tag {...iconProps} />;
    case "langgraph":
      return <GitBranch {...iconProps} />;
    case "proxy":
      return <Globe {...iconProps} />;
    case "custom":
      return <Wrench {...iconProps} />;
    default:
      return <Package {...iconProps} />;
  }
}

export function ServiceCard({
  service,
  health,
  onSelect,
  selected,
}: {
  service: ServiceDefinition;
  health?: HealthStatus;
  selected?: boolean;
  onSelect?: () => void;
}) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const setSelectedServiceId = useAppStore((state) => state.setSelectedServiceId);
  const [configOpen, setConfigOpen] = useState(false);
  const isHealthy = health?.status === "healthy";
  const isVirtual = service.metadata?.is_virtual === true;
  const serviceTypeLabels: Record<string, string> = {
    assistant: t("services.types.assistant"),
    langgraph: t("services.types.langgraph"),
    proxy: t("services.types.proxy"),
    conversational: t("services.types.conversational"),
    generative: t("services.types.generative"),
    processing: t("services.types.processing"),
    embedding: t("services.types.embedding"),
    classification: t("services.types.classification"),
    custom: t("services.types.custom"),
  };
  const serviceTypeLabel = serviceTypeLabels[service.service_type] || service.service_type;

  return (
    <>
      <Card
        className={cn(
          "relative overflow-hidden cursor-pointer group transition-[border-color,box-shadow,background-color] duration-150 border bg-card hover:shadow-sm hover:border-primary/30",
          selected && "ring-1 ring-primary/50 border-primary/40 shadow-xs bg-primary/2 dark:bg-primary/4"
        )}
        onClick={onSelect}
      >
        <CardContent className="p-4 relative">
          <div className="flex items-center gap-4">
            {/* Icon */}
            <div className="shrink-0 p-2.5 rounded-lg bg-primary/10 text-primary group-hover:bg-primary group-hover:text-primary-foreground transition-[color,background-color] duration-200 border border-primary/20">
              <ServiceIcon serviceType={service.service_type} />
            </div>

            {/* Content */}
            <div className="flex-1 min-w-0">
              <div className="flex items-center gap-2 mb-1">
                <h3 className="font-semibold text-foreground truncate">
                  {service.name}
                </h3>
                {/* Status Indicator */}
                <div className="shrink-0" title={isHealthy ? t("services.status.active", "Active") : t("services.status.inactive", "Inactive")}>
                  <span className={cn(
                    "inline-flex rounded-full h-2 w-2",
                    isHealthy ? "bg-green-500" : "bg-red-500"
                  )}></span>
                </div>
              </div>
              <p className="font-mono text-xs text-muted-foreground truncate mb-1.5">
                {service.service_id}
              </p>
              <div className="flex flex-wrap items-center gap-1.5">
                <Badge variant="secondary" className="bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300 text-[10px] font-semibold border border-slate-200 dark:border-slate-700 px-1.5 py-0">
                  {serviceTypeLabel}
                </Badge>
                {isVirtual && (
                  <Badge variant="outline" className="text-[10px] font-semibold border-blue-200 text-blue-700 bg-blue-50 dark:bg-blue-900/30 dark:text-blue-300 dark:border-blue-800 px-1.5 py-0">
                    {t("services.badge.core", "Core")}
                  </Badge>
                )}
                {service.supported_modes?.slice(0, 2).map((m) => (
                  <Badge key={m} variant="outline" className="text-[10px] text-muted-foreground font-medium border-border bg-background px-1.5 py-0">
                    {m}
                  </Badge>
                ))}
              </div>
            </div>

            {/* Actions */}
            <div className="shrink-0 flex items-center gap-1.5">
              <Button
                variant="outline"
                size="sm"
                className="h-7 px-2 text-xs text-muted-foreground hover:text-primary transition-[color,background-color] duration-150"
                onClick={(e) => {
                  e.stopPropagation();
                  if (service.service_type === "assistant") {
                    navigate("/assistant");
                    return;
                  }
                  setSelectedServiceId(service.service_id);
                  navigate("/playground");
                }}
                title={service.service_type === "assistant"
                  ? t("services.actions.openAssistant", "Open Assistant")
                  : t("services.actions.debugInPlayground", "Debug in Playground")}
              >
                <MessageSquare className="h-3.5 w-3.5 mr-1" />
                <span>{t("common.debug", "调试")}</span>
              </Button>
              {!isVirtual && (
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-7 w-7 text-muted-foreground hover:text-foreground hover:bg-muted transition-[color,background-color] duration-150"
                  onClick={(e) => {
                    e.stopPropagation();
                    setConfigOpen(true);
                  }}
                  title={t("common.configure")}
                >
                  <Settings className="h-3.5 w-3.5" />
                </Button>
              )}
            </div>
          </div>

          {/* Description - optional, shown if exists */}
          {service.description && (
            <p className="text-xs text-muted-foreground mt-3 pt-3 border-t border-border/50 line-clamp-1">
              {service.description}
            </p>
          )}
        </CardContent>
      </Card>

      {!isVirtual && (
        <ServiceConfigDialog
          serviceId={service.service_id}
          serviceName={service.name}
          open={configOpen}
          onOpenChange={setConfigOpen}
        />
      )}
    </>
  );
}
