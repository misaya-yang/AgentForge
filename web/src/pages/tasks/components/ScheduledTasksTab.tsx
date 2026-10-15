import { Link } from "react-router-dom";
import { useMemo, useRef, useState } from "react";
import { ScheduledHistoryBrowser } from "./ScheduledHistoryBrowser";
import { ScheduledTaskCreation } from "./ScheduledTaskCreation";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { generateUUID } from "@/lib/utils";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { ScheduledTaskActions } from "./ScheduledTaskActions";

function ScheduleEditor({ token, scope }: { token: string | null; scope: string }) {
  const { t } = useTranslation();
  const [revision, setRevision] = useState(0);
  const [creationVisit, setCreationVisit] = useState(0);
  const history = useRef<HTMLDivElement>(null);
  return <div className="space-y-4">
    <p className="text-sm text-muted-foreground">{t("tasks.scheduled.resultNotice", "Execution status and output are available here. Open Updates for durable in-app outcome receipts and read state. No email, push or external notification is sent.")}</p>
    <Button asChild size="sm" variant="link"><Link to="/tasks?tab=notifications">{t("tasks.notifications.title", "Scheduled task updates")}</Link></Button>
    {token && <ScheduledTaskCreation key={creationVisit} token={token} scope={scope}
      onReview={() => { setRevision(value => value + 1); history.current?.focus(); }}
      onNew={() => setCreationVisit(value => value + 1)} />}
    <div ref={history} tabIndex={-1}><ScheduledHistoryBrowser revision={revision} renderActions={(taskId, task, available) => <ScheduledTaskActions key={taskId} taskId={taskId} task={task} available={available} onObserved={() => setRevision(value => value + 1)} />} /></div>
  </div>;
}

export function ScheduledTasksTab() {
  const token = useAuthStore(state => state.token);
  const scope = useAuthStore(state => sdkAuthorityScope(state.user));
  const identity = useMemo(() => ({ id: generateUUID(), token, scope }), [token, scope]);
  return <ScheduleEditor key={identity.id} token={token} scope={scope} />;
}
