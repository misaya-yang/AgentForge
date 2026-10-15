export function liveThinkingLabel(
  content: string | undefined,
  fallback: string,
  maxCharacters = 72,
): string {
  const normalized = (content || "").replace(/\s+/g, " ").trim();
  if (!normalized) return fallback;

  const characters = Array.from(normalized);
  const preview = characters.length <= maxCharacters
    ? normalized
    : `…${characters.slice(-maxCharacters).join("")}`;
  return `${fallback}: ${preview}`;
}
