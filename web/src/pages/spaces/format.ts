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

const POSITION = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

export function positionBetween(before: string | null, after: string | null): string {
  const left = before ?? "";
  const right = after ?? "";
  if (!left && !right) return "a0";
  let prefix = "";
  let index = 0;
  while (index < 48) {
    const leftCode = index < left.length ? POSITION.indexOf(left[index] ?? "") : -1;
    const rightCode = index < right.length ? POSITION.indexOf(right[index] ?? "") : POSITION.length;
    const low = leftCode < 0 ? 0 : leftCode;
    const high = rightCode < 0 ? POSITION.length : rightCode;
    if (!right && left) return `${left}V`;
    if (high - low > 1) return prefix + POSITION[Math.floor((low + high) / 2)];
    prefix += POSITION[low];
    index += 1;
  }
  return `${left}V`.slice(0, 64);
}

export function itemKindKey(kind: string): string {
  return ["page", "folder", "file", "link", "slide", "site", "image", "external"].includes(kind)
    ? kind
    : "file";
}
