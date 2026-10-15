import dayjs from "dayjs";
import { useTranslation } from "react-i18next";

export function DashboardRefreshStatus({ requestedAt, mode }: { requestedAt: Date; mode: "manual" | "automatic" | "paused" }) {
  const { t } = useTranslation();
  return <span className="flex flex-wrap items-center gap-x-2 gap-y-1" title={t("dashboard.refreshLifecycle.notice")}>
    <span>{t(`dashboard.refreshLifecycle.${mode}`)}</span>
    <span>{t("dashboard.refreshLifecycle.requestedAt")}: <time dateTime={requestedAt.toISOString()}>{dayjs(requestedAt).format("YYYY-MM-DD HH:mm:ss")}</time></span>
  </span>;
}
