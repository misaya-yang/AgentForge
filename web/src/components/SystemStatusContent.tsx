import type { ReactNode } from "react";
import { SearchX, ShieldAlert } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Logo } from "@/components/Logo";

type SystemStatusKind = "forbidden" | "notFound";

interface SystemStatusPageProps {
  kind: SystemStatusKind;
  children: ReactNode;
}

const statusConfig = {
  forbidden: {
    code: "403",
    icon: ShieldAlert,
    titleKey: "systemPages.forbidden.title",
    descriptionKey: "systemPages.forbidden.description",
  },
  notFound: {
    code: "404",
    icon: SearchX,
    titleKey: "systemPages.notFound.title",
    descriptionKey: "systemPages.notFound.description",
  },
} satisfies Record<
  SystemStatusKind,
  {
    code: string;
    icon: typeof ShieldAlert;
    titleKey: string;
    descriptionKey: string;
  }
>;

export default function SystemStatusContent({ kind, children }: SystemStatusPageProps) {
  const { t } = useTranslation();
  const config = statusConfig[kind];
  const Icon = config.icon;

  return (
    <main className="relative flex min-h-dvh items-center justify-center overflow-hidden bg-background px-6 py-16">
      <div className="absolute left-6 top-6 sm:left-10 sm:top-8">
        <Logo collapsed={false} />
      </div>

      <section className="w-full max-w-lg text-center" aria-labelledby={`${kind}-title`}>
        <div className="mx-auto mb-6 flex h-12 w-12 items-center justify-center rounded-xl border border-border bg-muted/40 text-muted-foreground">
          <Icon aria-hidden="true" size={22} strokeWidth={1.6} />
        </div>
        <p className="mb-3 text-xs font-medium uppercase tracking-[0.18em] text-muted-foreground">
          {t("systemPages.eyebrow")}
        </p>
        <h1 className="mb-2 font-mono text-sm font-normal text-primary">{config.code}</h1>
        <h2 id={`${kind}-title`} className="text-3xl font-semibold tracking-tight text-foreground sm:text-4xl">
          {t(config.titleKey)}
        </h2>
        <p className="mx-auto mt-4 max-w-md text-sm leading-6 text-muted-foreground sm:text-base">
          {t(config.descriptionKey)}
        </p>
        {children}
      </section>
    </main>
  );
}
