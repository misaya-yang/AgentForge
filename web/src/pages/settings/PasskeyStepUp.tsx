import { useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { getPasskeyStepUpStatus, verifyConsolePasskey } from "@/api/auth";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";

export function PasskeyStepUp() {
  const { t } = useTranslation();
  const scope = useAuthenticatedReadScope();
  const pending = useRef(false);
  const [busy, setBusy] = useState(false), [failed, setFailed] = useState(false);
  const query = useQuery({ queryKey: ["passkey-step-up", scope.id], retry: false, gcTime: 0,
    queryFn: async () => {
      const status = await getPasskeyStepUpStatus();
      if (!scope.isCurrent()) throw new Error("Console session changed");
      return status;
    } });
  const verify = async () => {
    if (pending.current || !scope.isCurrent()) return;
    pending.current = true; setBusy(true); setFailed(false);
    try {
      await verifyConsolePasskey();
      if (scope.isCurrent()) await query.refetch();
    } catch { if (scope.isCurrent()) setFailed(true); }
    finally { pending.current = false; if (scope.isCurrent()) setBusy(false); }
  };
  return <Card aria-busy={busy || query.isFetching}>
    <CardHeader><CardTitle>{t("settings.auth.stepUp.title")}</CardTitle>
      <CardDescription>{t("settings.auth.stepUp.description")}</CardDescription></CardHeader>
    <CardContent className="space-y-3">
      {(query.isError || failed) && <p role="alert">{t("settings.auth.stepUp.failed")}</p>}
      {query.data && <p role="status">{t(!query.data.enabled ? "settings.auth.stepUp.disabled"
        : !query.data.passkey_enrolled ? "settings.auth.stepUp.enrollmentRequired"
          : query.data.verified ? "settings.auth.stepUp.verified" : "settings.auth.stepUp.required")}</p>}
      <Button disabled={busy || query.isFetching || !query.data?.enabled || !query.data.passkey_enrolled}
        onClick={() => void verify()}>{t("settings.auth.stepUp.verify")}</Button>
    </CardContent>
  </Card>;
}
