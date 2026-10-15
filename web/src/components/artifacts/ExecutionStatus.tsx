import * as React from "react";
import { Loader2, CheckCircle2, XCircle, Clock } from "lucide-react";
import { cn } from "@/lib/utils";

export type ExecutionStatusType = "idle" | "running" | "success" | "error" | "timeout";

interface ExecutionStatusProps {
  status: ExecutionStatusType;
  executionTimeMs?: number;
  className?: string;
}

const statusConfig: Record<
  ExecutionStatusType,
  {
    icon: React.ElementType;
    text: string | ((ms?: number) => string);
    iconClassName: string;
    textClassName: string;
  }
> = {
  idle: {
    icon: Clock,
    text: "Ready",
    iconClassName: "text-muted-foreground",
    textClassName: "text-muted-foreground",
  },
  running: {
    icon: Loader2,
    text: "Executing...",
    iconClassName: "text-blue-500 animate-spin",
    textClassName: "text-blue-500",
  },
  success: {
    icon: CheckCircle2,
    text: (ms) => (ms !== undefined ? `Completed in ${ms}ms` : "Completed"),
    iconClassName: "text-green-500",
    textClassName: "text-green-500",
  },
  error: {
    icon: XCircle,
    text: "Execution failed",
    iconClassName: "text-destructive",
    textClassName: "text-destructive",
  },
  timeout: {
    icon: Clock,
    text: "Execution timed out",
    iconClassName: "text-amber-500",
    textClassName: "text-amber-500",
  },
};

export function ExecutionStatus({
  status,
  executionTimeMs,
  className,
}: ExecutionStatusProps) {
  const config = statusConfig[status];
  const Icon = config.icon;
  const text =
    typeof config.text === "function"
      ? config.text(executionTimeMs)
      : config.text;

  return (
    <div className={cn("flex items-center gap-2", className)}>
      <Icon className={cn("h-4 w-4", config.iconClassName)} />
      <span className={cn("text-sm font-medium", config.textClassName)}>
        {text}
      </span>
    </div>
  );
}
