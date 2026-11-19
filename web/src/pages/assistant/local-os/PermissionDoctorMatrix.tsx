import { AlertTriangle, CheckCircle2, CircleX, Settings2 } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader } from "@/components/ui/card";
import { cn } from "@/lib/utils";

import { permissionClasses, permissionLabels } from "./presentation";
import type { PermissionHealthItem, PermissionKind } from "./types";

export interface PermissionDoctorMatrixProps {
  deviceId?: string;
  permissions: PermissionHealthItem[];
  onOpenPermissionSettings?: (deviceId: string, permission: PermissionKind) => void;
}

function PermissionIcon({ status }: { status: PermissionHealthItem["status"] }) {
  if (status === "ready") {
    return <CheckCircle2 aria-hidden="true" className="size-4 text-emerald-600" />;
  }
  if (status === "denied") {
    return <CircleX aria-hidden="true" className="size-4 text-destructive" />;
  }
  return <AlertTriangle aria-hidden="true" className="size-4 text-amber-600" />;
}

export function PermissionDoctorMatrix({
  deviceId,
  permissions,
  onOpenPermissionSettings,
}: PermissionDoctorMatrixProps) {
  const readyCount = permissions.filter((permission) => permission.status === "ready").length;

  return (
    <Card aria-labelledby="permission-doctor-heading">
      <CardHeader className="flex-row items-start justify-between gap-3 space-y-0 p-4">
        <div>
          <div className="flex items-center gap-2">
            <Settings2 aria-hidden="true" className="size-4 text-muted-foreground" />
            <h2 id="permission-doctor-heading" className="text-sm font-semibold">
              Permission doctor
            </h2>
          </div>
          <p className="mt-1 text-xs text-muted-foreground">
            Capability truth from the selected device and its OS permissions.
          </p>
        </div>
        <Badge variant="outline">
          {readyCount}/{permissions.length} ready
        </Badge>
      </CardHeader>
      <CardContent className="p-0">
        {permissions.length === 0 ? (
          <p className="px-4 pb-4 text-sm text-muted-foreground">
            Select an online device to inspect permission health.
          </p>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-left text-sm">
              <caption className="sr-only">Local Node permission health matrix</caption>
              <thead className="border-y border-border/70 bg-muted/30 text-xs text-muted-foreground">
                <tr>
                  <th scope="col" className="px-4 py-2 font-medium">
                    Capability
                  </th>
                  <th scope="col" className="px-4 py-2 font-medium">
                    Status
                  </th>
                  <th scope="col" className="px-4 py-2 text-right font-medium">
                    Action
                  </th>
                </tr>
              </thead>
              <tbody className="divide-y divide-border/60">
                {permissions.map((permission) => {
                  const actionable =
                    Boolean(deviceId) &&
                    Boolean(onOpenPermissionSettings) &&
                    (permission.status === "denied" || permission.status === "needs_action");

                  return (
                    <tr key={permission.id}>
                      <th scope="row" className="px-4 py-3 font-normal">
                        <div className="flex items-start gap-2">
                          <PermissionIcon status={permission.status} />
                          <div>
                            <p className="font-medium text-foreground">{permission.label}</p>
                            <p className="mt-0.5 max-w-md text-xs text-muted-foreground">
                              {permission.detail ?? permission.description}
                            </p>
                          </div>
                        </div>
                      </th>
                      <td className="px-4 py-3">
                        <Badge
                          variant="outline"
                          className={cn(permissionClasses[permission.status])}
                        >
                          {permissionLabels[permission.status]}
                        </Badge>
                      </td>
                      <td className="px-4 py-3 text-right">
                        {actionable && deviceId && onOpenPermissionSettings ? (
                          <Button
                            size="sm"
                            variant="outline"
                            onClick={() => onOpenPermissionSettings(deviceId, permission.id)}
                          >
                            {permission.actionLabel ?? "Open settings"}
                          </Button>
                        ) : (
                          <span className="text-xs text-muted-foreground">—</span>
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

