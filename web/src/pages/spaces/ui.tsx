import { File, FileText, Globe, Image, Link2, Presentation, Shapes } from "lucide-react";
import type { ReactNode } from "react";
import { itemKindKey } from "./format";

export function KindIcon({ kind }: { kind: string }) {
  const common = { size: 14, "aria-hidden": true as const };
  switch (itemKindKey(kind)) {
    case "page":
      return <FileText {...common} />;
    case "slide":
      return <Presentation {...common} />;
    case "site":
      return <Globe {...common} />;
    case "image":
      return <Image {...common} />;
    case "link":
    case "external":
      return <Link2 {...common} />;
    case "folder":
      return <Shapes {...common} />;
    default:
      return <File {...common} />;
  }
}

export function SpaceDialog({
  id,
  titleId,
  title,
  labelledBy,
  children,
  light = true,
}: {
  id: string;
  titleId: string;
  title: string;
  labelledBy?: string;
  children: ReactNode;
  light?: boolean;
}) {
  return (
    <dialog id={id} aria-labelledby={labelledBy ?? titleId} data-light={light ? "" : undefined}>
      <h2 id={titleId}>{title}</h2>
      {children}
    </dialog>
  );
}
