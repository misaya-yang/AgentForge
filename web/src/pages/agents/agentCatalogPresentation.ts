export function capabilityDisplayName(rawName: string, title?: string | null): string {
  const suppliedTitle = title?.trim();
  if (suppliedTitle) return suppliedTitle;

  const leaf = rawName.split("__").at(-1) || rawName;
  return leaf
    .replace(/[_./:-]+/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .replace(/^\p{Ll}/u, (first) => first.toUpperCase());
}
