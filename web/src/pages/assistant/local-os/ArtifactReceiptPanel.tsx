import {
  FileDiff,
  FileText,
  Image,
  RotateCcw,
  ScrollText,
  ShieldCheck,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";

import { formatTimestamp, shortDigest } from "./presentation";
import type { LocalArtifactKind, LocalArtifactReceipt } from "./types";

export interface ArtifactReceiptPanelProps {
  artifacts: LocalArtifactReceipt[];
  onOpenArtifact?: (artifactId: string) => void;
  onRollbackArtifact?: (artifactId: string, rollbackRef: string) => void;
}

function ArtifactIcon({ kind }: { kind: LocalArtifactKind }) {
  const className = "size-4";
  switch (kind) {
    case "diff":
      return <FileDiff aria-hidden="true" className={className} />;
    case "screenshot":
      return <Image aria-hidden="true" className={className} />;
    case "process_output":
      return <ScrollText aria-hidden="true" className={className} />;
    case "rollback":
      return <RotateCcw aria-hidden="true" className={className} />;
    default:
      return <FileText aria-hidden="true" className={className} />;
  }
}

export function ArtifactReceiptPanel({
  artifacts,
  onOpenArtifact,
  onRollbackArtifact,
}: ArtifactReceiptPanelProps) {
  const orderedArtifacts = [...artifacts].sort((left, right) =>
    right.createdAt.localeCompare(left.createdAt),
  );

  return (
    <Card aria-labelledby="local-artifacts-heading">
      <CardHeader className="flex-row items-start justify-between gap-3 space-y-0 p-4">
        <div>
          <div className="flex items-center gap-2">
            <ShieldCheck aria-hidden="true" className="size-4 text-muted-foreground" />
            <h2 id="local-artifacts-heading" className="text-sm font-semibold">
              Artifacts & receipts
            </h2>
          </div>
          <p className="mt-1 text-xs text-muted-foreground">
            Environment evidence, not model claims. Rollback appears only when supported.
          </p>
        </div>
        <Badge variant="outline">{artifacts.length}</Badge>
      </CardHeader>
      <CardContent className="p-0">
        {orderedArtifacts.length === 0 ? (
          <p className="px-4 pb-4 text-sm text-muted-foreground">No local receipts recorded.</p>
        ) : (
          <ScrollArea className="max-h-[32rem] px-4 pb-4">
            <ul className="space-y-3">
              {orderedArtifacts.map((artifact) => (
                <li key={artifact.id} className="rounded-md border border-border/70 p-3">
                  <div className="flex flex-wrap items-start justify-between gap-2">
                    <div className="flex min-w-0 items-start gap-2">
                      <span className="mt-0.5 text-muted-foreground">
                        <ArtifactIcon kind={artifact.kind} />
                      </span>
                      <div className="min-w-0">
                        <p className="truncate text-sm font-medium">{artifact.title}</p>
                        <p className="mt-0.5 text-xs text-muted-foreground">
                          {formatTimestamp(artifact.createdAt)}
                        </p>
                      </div>
                    </div>
                    <Badge variant={artifact.status === "available" ? "secondary" : "outline"}>
                      {artifact.status}
                    </Badge>
                  </div>

                  {artifact.detail ? (
                    <p className="mt-2 text-xs leading-relaxed text-muted-foreground">
                      {artifact.detail}
                    </p>
                  ) : null}

                  {artifact.diffText ? (
                    <pre className="mt-3 max-h-44 overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted p-3 font-mono text-xs text-foreground">
                      {artifact.diffText}
                    </pre>
                  ) : null}

                  {artifact.beforeDigest || artifact.afterDigest ? (
                    <dl className="mt-3 grid gap-2 text-xs sm:grid-cols-2">
                      <div>
                        <dt className="text-muted-foreground">Before</dt>
                        <dd className="font-mono" title={artifact.beforeDigest}>
                          {shortDigest(artifact.beforeDigest)}
                        </dd>
                      </div>
                      <div>
                        <dt className="text-muted-foreground">After</dt>
                        <dd className="font-mono" title={artifact.afterDigest}>
                          {shortDigest(artifact.afterDigest)}
                        </dd>
                      </div>
                    </dl>
                  ) : null}

                  <div className="mt-3 flex flex-wrap gap-2">
                    {onOpenArtifact && artifact.status === "available" ? (
                      <Button
                        size="sm"
                        variant="outline"
                        onClick={() => onOpenArtifact(artifact.id)}
                      >
                        Open receipt
                      </Button>
                    ) : null}
                    {onRollbackArtifact &&
                    artifact.rollbackRef &&
                    artifact.rollbackStatus === "available" ? (
                      <Button
                        size="sm"
                        variant="destructive"
                        onClick={() => onRollbackArtifact(artifact.id, artifact.rollbackRef!)}
                      >
                        <RotateCcw aria-hidden="true" className="mr-1.5 size-3.5" />
                        Request rollback
                      </Button>
                    ) : null}
                    {artifact.rollbackStatus && artifact.rollbackStatus !== "available" ? (
                      <Badge variant="outline">Rollback: {artifact.rollbackStatus}</Badge>
                    ) : null}
                  </div>
                </li>
              ))}
            </ul>
          </ScrollArea>
        )}
      </CardContent>
    </Card>
  );
}
