import { useState } from "react";
import { Link } from "react-router-dom";
import { X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useSetupState } from "@/api/setup";
import { useAuthStore } from "@/store/useAuthStore";
import {
  readSetupBannerDismissed,
  writeSetupBannerDismissed,
} from "./setupBannerStorage";

export function SetupBanner() {
  const userId = useAuthStore((state) => state.user?.user_id);
  return <SetupBannerForUser key={userId ?? "anonymous"} userId={userId} />;
}

function SetupBannerForUser({ userId }: { userId?: string }) {
  const { t } = useTranslation();
  const { data, isLoading, error } = useSetupState();
  const [dismissed, setDismissed] = useState(() =>
    readSetupBannerDismissed(userId),
  );

  // Only show while the stack reports no configured provider. Loading,
  // failed, or already-configured states render nothing — the banner must
  // never block or disturb the rest of the console.
  if (isLoading || error || !data || data.configured || dismissed) {
    return null;
  }

  return (
    <div className="mb-3 flex flex-wrap items-center gap-x-3 gap-y-1 rounded-lg border border-primary/25 bg-primary/10 px-4 py-2.5 text-sm text-foreground">
      <span className="min-w-0 flex-1 truncate">
        {t("setup.banner.text", "Configure a model provider to start using the assistant.")}
      </span>
      <Link
        to="/services"
        className="shrink-0 rounded-md bg-primary px-2.5 py-1 text-xs font-medium text-primary-foreground transition-colors hover:bg-primary/90"
      >
        {t("setup.banner.action", "Configure model services")}
      </Link>
      <button
        type="button"
        aria-label={t("setup.banner.dismiss", "Dismiss")}
        className="shrink-0 rounded-md p-1 text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground"
        onClick={() => {
          if (writeSetupBannerDismissed(userId)) {
            setDismissed(true);
          }
        }}
      >
        <X size={14} />
      </button>
    </div>
  );
}
