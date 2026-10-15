import { LockKeyhole, MonitorCog, ShieldCheck } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";

import { ArtifactReceiptPanel } from "./ArtifactReceiptPanel";
import { DevicePickerStatus } from "./DevicePickerStatus";
import { ExactApprovalCard } from "./ExactApprovalCard";
import { GrantScopePanel } from "./GrantScopePanel";
import { LiveObservationCanvas } from "./LiveObservationCanvas";
import { LocalActionTimeline } from "./LocalActionTimeline";
import { OfflineDegradationNotice } from "./OfflineDegradationNotice";
import { PermissionDoctorMatrix } from "./PermissionDoctorMatrix";
import type {
  LocalOSControlSurfaceActions,
  LocalOSControlSurfaceModel,
} from "./types";

export interface LocalOSControlSurfaceProps {
  model: LocalOSControlSurfaceModel;
  actions: LocalOSControlSurfaceActions;
  className?: string;
}

/**
 * Props-driven control surface for Local Node state.
 *
 * The component intentionally contains no networking, persistence, optimistic
 * authorization, or mocked device behavior. Integrators must project durable
 * runtime state into `model` and forward user intent through `actions`.
 */
export function LocalOSControlSurface({
  model,
  actions,
  className,
}: LocalOSControlSurfaceProps) {
  const selectedDevice = model.devices.find(
    (device) => device.id === model.selectedDeviceId,
  );
  const localUnavailable = selectedDevice?.connectionStatus !== "online";

  return (
    <section
      className={className}
      aria-labelledby="local-os-control-surface-heading"
      data-testid="local-os-control-surface"
    >
      <header className="mb-4 flex flex-col gap-3 border-b border-border pb-4 sm:flex-row sm:items-start sm:justify-between">
        <div className="flex min-w-0 items-start gap-3">
          <span className="flex size-9 shrink-0 items-center justify-center rounded-lg border border-primary/15 bg-primary/8 text-primary">
            <MonitorCog aria-hidden="true" className="size-5" />
          </span>
          <div>
            <h1 id="local-os-control-surface-heading" className="text-base font-semibold">
              Local files
            </h1>
            <p className="mt-1 max-w-3xl text-sm text-muted-foreground">
              Analyze explicitly authorized folders with list, grep, read, hash, and change
              watching. The Local Node remains authoritative for every path and grant.
            </p>
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant="outline" className="gap-1.5">
            <ShieldCheck aria-hidden="true" className="size-3.5" />
            Deny wins
          </Badge>
          <Badge variant="outline" className="gap-1.5">
            <LockKeyhole aria-hidden="true" className="size-3.5" />
            Local confirmation
          </Badge>
        </div>
      </header>

      {model.degradation ? (
        <div className="mb-4">
          <OfflineDegradationNotice degradation={model.degradation} />
        </div>
      ) : null}

      <Tabs defaultValue="control" className="w-full">
        <TabsList className="grid h-auto w-full grid-cols-3 sm:w-fit">
          <TabsTrigger value="control">Control</TabsTrigger>
          <TabsTrigger value="permissions">Permissions</TabsTrigger>
          <TabsTrigger value="receipts">Receipts</TabsTrigger>
        </TabsList>

        <TabsContent value="control" className="mt-4 space-y-4">
          <div className="grid gap-4 xl:grid-cols-[minmax(0,1.45fr)_minmax(21rem,0.85fr)]">
            <LiveObservationCanvas
              observation={model.observation}
              canTakeOver={!localUnavailable}
              canStop={Boolean(actions.onStopComputerUse)}
              onRequestTakeover={actions.onRequestTakeover}
              onStop={actions.onStopComputerUse}
            />
            <LocalActionTimeline
              actions={model.actions}
              onVerifyUnknownAction={actions.onVerifyUnknownAction}
            />
          </div>

          {model.approval ? (
            <ExactApprovalCard
              approval={model.approval}
              trustedConfirmation={model.trustedConfirmation}
              onRequestTrustedConfirmation={actions.onRequestTrustedConfirmation}
              onReject={actions.onRejectApproval}
              onChangeScope={actions.onChangeApprovalScope}
            />
          ) : null}
        </TabsContent>

        <TabsContent value="permissions" className="mt-4">
          <div className="grid gap-4 xl:grid-cols-2">
            <div className="space-y-4">
              <DevicePickerStatus
                devices={model.devices}
                selectedDeviceId={model.selectedDeviceId}
                onSelectDevice={actions.onSelectDevice}
                onPairDevice={actions.onPairDevice}
                onRefreshDevice={actions.onRefreshDevice}
                onRevokeDevice={actions.onRevokeDevice}
              />
              <PermissionDoctorMatrix
                deviceId={selectedDevice?.id}
                permissions={selectedDevice?.permissions ?? []}
                onOpenPermissionSettings={actions.onOpenPermissionSettings}
              />
            </div>
            <GrantScopePanel
              grants={model.grants}
              disabled={localUnavailable}
              onAddWorkspaceGrant={actions.onAddWorkspaceGrant}
              onAddAppGrant={actions.onAddAppGrant}
              onAddDomainGrant={actions.onAddDomainGrant}
              onRevokeWorkspaceGrant={actions.onRevokeWorkspaceGrant}
              onRevokeAppGrant={actions.onRevokeAppGrant}
              onRevokeDomainGrant={actions.onRevokeDomainGrant}
            />
          </div>
        </TabsContent>

        <TabsContent value="receipts" className="mt-4">
          <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_minmax(22rem,0.8fr)]">
            <ArtifactReceiptPanel
              artifacts={model.artifacts}
              onOpenArtifact={actions.onOpenArtifact}
              onRollbackArtifact={actions.onRollbackArtifact}
            />
            <LocalActionTimeline
              actions={model.actions}
              maxHeightClassName="max-h-[40rem]"
              onVerifyUnknownAction={actions.onVerifyUnknownAction}
            />
          </div>
        </TabsContent>
      </Tabs>
    </section>
  );
}
