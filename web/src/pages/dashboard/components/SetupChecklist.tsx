import { Link } from "react-router-dom";
import { CheckCircle2, Circle } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useSetupState } from "@/api/setup";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

interface ChecklistStep {
  key: "provider" | "knowledge" | "chat";
  href: string;
  done: boolean;
}

export function SetupChecklist() {
  const { t } = useTranslation();
  const { data } = useSetupState();

  // The checklist replaces the dashboard first screen until a provider is
  // configured — then the dashboard content takes over.
  if (!data || data.configured) {
    return null;
  }

  // This branch only renders while `configured` is false, so the provider
  // step is never done here; it flips the whole checklist off instead.
  const steps: ChecklistStep[] = [
    { key: "provider", href: "/services", done: false },
    { key: "knowledge", href: "/knowledge", done: false },
    { key: "chat", href: "/assistant", done: false },
  ];

  return (
    <Card className="mb-4 max-w-2xl">
      <CardHeader>
        <CardTitle className="text-base">
          {t("setup.checklist.title", "Get started in three steps")}
        </CardTitle>
        <p className="text-sm text-muted-foreground">
          {t(
            "setup.checklist.subtitle",
            "Connect a model provider, create a knowledge base, then start your first conversation."
          )}
        </p>
      </CardHeader>
      <CardContent className="flex flex-col gap-2">
        {steps.map((step) => (
          <div
            key={step.key}
            className="flex items-center gap-3 rounded-lg border border-border/60 px-3 py-2.5"
          >
            {step.done ? (
              <CheckCircle2 size={18} className="shrink-0 text-primary" />
            ) : (
              <Circle size={18} className="shrink-0 text-muted-foreground/60" />
            )}
            <span className="min-w-0 flex-1 truncate text-sm">
              {t(`setup.checklist.step.${step.key}`)}
            </span>
            {!step.done && (
              <Button asChild size="sm" variant="outline">
                <Link to={step.href}>{t("setup.checklist.go", "Go")}</Link>
              </Button>
            )}
          </div>
        ))}
      </CardContent>
    </Card>
  );
}
