const GFM_TABLE_DELIMITER = /(?:^|\n)\s*\|?(?:\s*:?-{3,}:?\s*\|)+\s*:?-{3,}:?\s*\|?\s*(?:\n|$)/;
const GFM_TASK_LIST = /(?:^|\n)\s*[-*+]\s+\[[ xX]\]\s+/;
const GFM_STRIKETHROUGH = /~~[^~\n]+~~/;
const GFM_AUTOLINK = /(?:https?:\/\/|www\.)\S+/gi;
const GFM_EMAIL_AUTOLINK = /\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b/gi;

function hasBareAutolink(text: string, pattern: RegExp): boolean {
  pattern.lastIndex = 0;
  for (const match of text.matchAll(pattern)) {
    const index = match.index ?? 0;
    const previous = index > 0 ? text[index - 1] : "";
    const previousTwo = index > 1 ? text.slice(index - 2, index) : "";
    if (previous !== "<" && previousTwo !== "](") return true;
  }
  return false;
}

export function needsGfmPlugin(text: string): boolean {
  return GFM_TABLE_DELIMITER.test(text)
    || GFM_TASK_LIST.test(text)
    || GFM_STRIKETHROUGH.test(text)
    || hasBareAutolink(text, GFM_AUTOLINK)
    || hasBareAutolink(text, GFM_EMAIL_AUTOLINK);
}
