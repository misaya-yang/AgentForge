import {
  Laptop,
  Link2,
  RefreshCw,
  ShieldX,
  Wifi,
  WifiOff,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader } from "@/components/ui/card";
import { cn } from "@/lib/utils";

import { connectionClasses, connectionLabels, formatTimestamp } from "./presentation";
import type { LocalOSDevice } from "./types";

export interface DevicePickerStatusProps {
  devices: LocalOSDevice[];
  selectedDeviceId?: string;
  onSelectDevice: (deviceId: string) => void;
  onPairDevice?: () => void;
  onRefreshDevice?: (deviceId: string) => void;
  onRevokeDevice?: (deviceId: string) => void;
}

export function DevicePickerStatus({
  devices,
  selectedDeviceId,
  onSelectDevice,
  onPairDevice,
  onRefreshDevice,
  onRevokeDevice,
}: DevicePickerStatusProps) {
  const selectedDevice = devices.find((device) => device.id === selectedDeviceId);
  const online = selectedDevice?.connectionStatus === "online";

  return (
    <Card aria-labelledby="local-os-device-heading">
      <CardHeader className="flex-row items-start justify-between gap-4 space-y-0 p-4">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <Laptop aria-hidden="true" className="size-4 text-muted-foreground" />
            <h2 id="local-os-device-heading" className="text-sm font-semibold">
              Local device
            </h2>
          </div>
          <p className="mt-1 text-xs text-muted-foreground">
            The Local Node executes only the capabilities granted on that device.
          </p>
        </div>
        {onPairDevice ? (
          <Button size="sm" variant="outline" onClick={onPairDevice}>
            <Link2 aria-hidden="true" className="mr-1.5 size-3.5" />
            Pair device
          </Button>
        ) : null}
      </CardHeader>

      <CardContent className="space-y-4 p-4 pt-0">
        {devices.length > 0 ? (
          <div className="space-y-1.5">
            <label htmlFor="local-os-device-select" className="text-xs font-medium">
              Active device
            </label>
            <select
              id="local-os-device-select"
              value={selectedDeviceId ?? ""}
              onChange={(event) => onSelectDevice(event.target.value)}
              className="h-9 w-full rounded-md border border-input bg-background px-3 text-sm text-foreground outline-hidden focus-visible:ring-2 focus-visible:ring-ring"
            >
              <option value="" disabled>
                Select a paired device
              </option>
              {devices.map((device) => (
                <option key={device.id} value={device.id}>
                  {device.label} · {connectionLabels[device.connectionStatus]}
                </option>
              ))}
            </select>
          </div>
        ) : (
          <div className="rounded-md border border-dashed border-border p-4 text-sm text-muted-foreground">
            No paired Local Node. Cloud chat remains available, but local files and apps are unavailable.
          </div>
        )}

        {selectedDevice ? (
          <div className="rounded-md border border-border/70 bg-muted/25 p-3" aria-live="polite">
            <div className="flex flex-wrap items-start justify-between gap-2">
              <div className="flex min-w-0 items-center gap-2">
                {online ? (
                  <Wifi aria-hidden="true" className="size-4 text-emerald-600" />
                ) : (
                  <WifiOff aria-hidden="true" className="size-4 text-muted-foreground" />
                )}
                <div className="min-w-0">
                  <p className="truncate text-sm font-medium">{selectedDevice.label}</p>
                  <p className="text-xs text-muted-foreground">
                    {selectedDevice.os} {selectedDevice.osVersion ?? ""}
                    {selectedDevice.nodeVersion ? ` · Node ${selectedDevice.nodeVersion}` : ""}
                  </p>
                </div>
              </div>
              <Badge
                variant="outline"
                className={cn("shrink-0", connectionClasses[selectedDevice.connectionStatus])}
              >
                {connectionLabels[selectedDevice.connectionStatus]}
              </Badge>
            </div>

            <dl className="mt-3 grid gap-2 text-xs sm:grid-cols-2">
              <div>
                <dt className="text-muted-foreground">Last heartbeat</dt>
                <dd className="mt-0.5 font-medium">
                  {formatTimestamp(selectedDevice.lastHeartbeatAt)}
                </dd>
              </div>
              <div>
                <dt className="text-muted-foreground">Reported capabilities</dt>
                <dd className="mt-0.5 font-medium">
                  {selectedDevice.capabilities.length > 0
                    ? `${selectedDevice.capabilities.length} available`
                    : "None"}
                </dd>
              </div>
            </dl>

            {selectedDevice.capabilities.length > 0 ? (
              <div className="mt-3 flex flex-wrap gap-1.5" aria-label="Reported capabilities">
                {selectedDevice.capabilities.map((capability) => (
                  <Badge key={capability} variant="secondary" className="font-normal">
                    {capability}
                  </Badge>
                ))}
              </div>
            ) : null}

            <div className="mt-3 flex flex-wrap gap-2">
              {onRefreshDevice ? (
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => onRefreshDevice(selectedDevice.id)}
                >
                  <RefreshCw aria-hidden="true" className="mr-1.5 size-3.5" />
                  Refresh status
                </Button>
              ) : null}
              {onRevokeDevice ? (
                <Button
                  size="sm"
                  variant="destructive"
                  onClick={() => onRevokeDevice(selectedDevice.id)}
                >
                  <ShieldX aria-hidden="true" className="mr-1.5 size-3.5" />
                  Revoke pairing
                </Button>
              ) : null}
            </div>
          </div>
        ) : null}
      </CardContent>
    </Card>
  );
}

