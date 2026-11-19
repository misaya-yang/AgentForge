import { AppWindow, FolderLock, Globe2, Plus, X } from "lucide-react";
import type { ReactNode } from "react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader } from "@/components/ui/card";

import type { LocalOSGrants } from "./types";

export interface GrantScopePanelProps {
  grants: LocalOSGrants;
  disabled?: boolean;
  onAddWorkspaceGrant?: () => void;
  onAddAppGrant?: () => void;
  onAddDomainGrant?: () => void;
  onRevokeWorkspaceGrant?: (grantId: string) => void;
  onRevokeAppGrant?: (grantId: string) => void;
  onRevokeDomainGrant?: (grantId: string) => void;
}

interface GrantSectionProps {
  title: string;
  description: string;
  icon: ReactNode;
  count: number;
  onAdd?: () => void;
  disabled?: boolean;
  children: ReactNode;
}

function GrantSection({
  title,
  description,
  icon,
  count,
  onAdd,
  disabled,
  children,
}: GrantSectionProps) {
  return (
    <section className="rounded-md border border-border/70" aria-label={title}>
      <div className="flex items-start justify-between gap-3 border-b border-border/60 px-3 py-2.5">
        <div className="flex min-w-0 items-start gap-2">
          <span className="mt-0.5 text-muted-foreground">{icon}</span>
          <div>
            <h3 className="text-sm font-medium">
              {title} <span className="text-muted-foreground">({count})</span>
            </h3>
            <p className="text-xs text-muted-foreground">{description}</p>
          </div>
        </div>
        {onAdd ? (
          <Button size="sm" variant="ghost" disabled={disabled} onClick={onAdd}>
            <Plus aria-hidden="true" className="mr-1 size-3.5" />
            Add
          </Button>
        ) : null}
      </div>
      <div className="divide-y divide-border/50">{children}</div>
    </section>
  );
}

function EmptyGrant({ children }: { children: ReactNode }) {
  return <p className="px-3 py-3 text-xs text-muted-foreground">{children}</p>;
}

export function GrantScopePanel({
  grants,
  disabled = false,
  onAddWorkspaceGrant,
  onAddAppGrant,
  onAddDomainGrant,
  onRevokeWorkspaceGrant,
  onRevokeAppGrant,
  onRevokeDomainGrant,
}: GrantScopePanelProps) {
  return (
    <Card aria-labelledby="local-os-grants-heading">
      <CardHeader className="p-4">
        <h2 id="local-os-grants-heading" className="text-sm font-semibold">
          Session grants
        </h2>
        <p className="text-xs text-muted-foreground">
          Reading local data and sending it outside the device remain separate permissions.
        </p>
      </CardHeader>
      <CardContent className="space-y-3 p-4 pt-0">
        <GrantSection
          title="Authorized folders"
          description="List, grep, read, hash, or watch only inside folders selected on this device."
          icon={<FolderLock aria-hidden="true" className="size-4" />}
          count={grants.workspaces.length}
          onAdd={onAddWorkspaceGrant}
          disabled={disabled}
        >
          {grants.workspaces.length === 0 ? (
            <EmptyGrant>No local folder is authorized.</EmptyGrant>
          ) : (
            grants.workspaces.map((grant) => (
              <div key={grant.id} className="flex items-start gap-3 px-3 py-3">
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <p className="truncate text-sm font-medium">{grant.displayName}</p>
                    <Badge variant={grant.status === "active" ? "secondary" : "outline"}>
                      {grant.status}
                    </Badge>
                  </div>
                  <p className="mt-0.5 truncate font-mono text-xs text-muted-foreground">
                    {grant.displayPath}
                  </p>
                  <div className="mt-2 flex flex-wrap gap-1">
                    {grant.capabilities.map((capability) => (
                      <Badge key={capability} variant="outline" className="font-normal">
                        file.{capability}
                      </Badge>
                    ))}
                  </div>
                </div>
                {onRevokeWorkspaceGrant ? (
                  <Button
                    size="icon"
                    variant="ghost"
                    disabled={disabled || grant.status !== "active"}
                    onClick={() => onRevokeWorkspaceGrant(grant.id)}
                    aria-label={`Revoke workspace grant for ${grant.displayName}`}
                  >
                    <X aria-hidden="true" className="size-4" />
                  </Button>
                ) : null}
              </div>
            ))
          )}
        </GrantSection>

        <GrantSection
          title="Applications"
          description="Observation, control, and submit are independent capabilities."
          icon={<AppWindow aria-hidden="true" className="size-4" />}
          count={grants.apps.length}
          onAdd={onAddAppGrant}
          disabled={disabled}
        >
          {grants.apps.length === 0 ? (
            <EmptyGrant>No desktop application is authorized.</EmptyGrant>
          ) : (
            grants.apps.map((grant) => (
              <div key={grant.id} className="flex items-start gap-3 px-3 py-3">
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <p className="truncate text-sm font-medium">{grant.displayName}</p>
                    <Badge variant={grant.status === "active" ? "secondary" : "outline"}>
                      {grant.status}
                    </Badge>
                  </div>
                  {grant.bundleIdentifier ? (
                    <p className="mt-0.5 truncate font-mono text-xs text-muted-foreground">
                      {grant.bundleIdentifier}
                    </p>
                  ) : null}
                  <div className="mt-2 flex flex-wrap gap-1">
                    {grant.capabilities.map((capability) => (
                      <Badge key={capability} variant="outline" className="font-normal">
                        app.{capability}
                      </Badge>
                    ))}
                  </div>
                </div>
                {onRevokeAppGrant ? (
                  <Button
                    size="icon"
                    variant="ghost"
                    disabled={disabled || grant.status !== "active"}
                    onClick={() => onRevokeAppGrant(grant.id)}
                    aria-label={`Revoke application grant for ${grant.displayName}`}
                  >
                    <X aria-hidden="true" className="size-4" />
                  </Button>
                ) : null}
              </div>
            ))
          )}
        </GrantSection>

        <GrantSection
          title="Domains"
          description="Uploads and form submission require their own grant."
          icon={<Globe2 aria-hidden="true" className="size-4" />}
          count={grants.domains.length}
          onAdd={onAddDomainGrant}
          disabled={disabled}
        >
          {grants.domains.length === 0 ? (
            <EmptyGrant>No browser origin is authorized.</EmptyGrant>
          ) : (
            grants.domains.map((grant) => (
              <div key={grant.id} className="flex items-start gap-3 px-3 py-3">
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <p className="truncate font-mono text-sm font-medium">{grant.origin}</p>
                    <Badge variant={grant.status === "active" ? "secondary" : "outline"}>
                      {grant.status}
                    </Badge>
                  </div>
                  <div className="mt-2 flex flex-wrap gap-1">
                    {grant.capabilities.map((capability) => (
                      <Badge key={capability} variant="outline" className="font-normal">
                        network.{capability}
                      </Badge>
                    ))}
                  </div>
                </div>
                {onRevokeDomainGrant ? (
                  <Button
                    size="icon"
                    variant="ghost"
                    disabled={disabled || grant.status !== "active"}
                    onClick={() => onRevokeDomainGrant(grant.id)}
                    aria-label={`Revoke domain grant for ${grant.origin}`}
                  >
                    <X aria-hidden="true" className="size-4" />
                  </Button>
                ) : null}
              </div>
            ))
          )}
        </GrantSection>
      </CardContent>
    </Card>
  );
}
