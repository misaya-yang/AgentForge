import type { SpaceItem } from "@/api/spaces";

const UNITS = [
  ["minute", 60],
  ["hour", 24],
  ["day", 7],
  ["week", 4],
  ["month", 12],
  ["year", Number.POSITIVE_INFINITY],
] as const;

export function relativeTime(iso: string, now: number, locale: string): string {
  const target = Date.parse(iso);
  if (Number.isNaN(target)) return "";
  let value = Math.round((target - now) / 60000);
  let unit: Intl.RelativeTimeFormatUnit = "minute";
  for (const [next, size] of UNITS) {
    unit = next;
    if (Math.abs(value) < size) break;
    value = Math.round(value / size);
  }
  return new Intl.RelativeTimeFormat(locale, { numeric: "always" }).format(value, unit);
}

export function sourceLabel(item: Pick<SpaceItem, "source" | "shared" | "created_by" | "space_name">, viewerId: string): string {
  if (item.shared && item.created_by !== viewerId && item.space_name.trim()) return item.space_name;
  if (item.source.startsWith("connector:")) return item.source.slice("connector:".length);
  return "—";
}

export function itemKindKey(kind: string): string {
  return ["page", "folder", "file", "link", "slide", "site", "image", "external"].includes(kind)
    ? kind
    : "file";
}
