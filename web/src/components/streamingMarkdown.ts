/**
 * Split only at blank-line boundaries that are outside fenced code and
 * display-math blocks. Completed blocks keep stable React identities while
 * the final in-progress block changes on every streamed token.
 */
export function splitStreamingMarkdownBlocks(text: string): string[] {
  // Reference definitions and footnotes can resolve syntax in any earlier
  // paragraph, so parsing them as independent ReactMarkdown trees is unsafe.
  // HTML containers likewise have their own blank-line rules. Keep those less
  // common responses as one correctness-first block.
  if (
    /\[[^\]\n]+\]\[[^\]\n]*\]|\[\^[^\]\n]+\]/.test(text) ||
    /^[ \t]{0,3}\[(?:\^)?[^\]]+\]:/m.test(text) ||
    /^[ \t]{0,3}(?:<!--|<\/?(?:address|article|aside|base|basefont|blockquote|body|caption|center|col|colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|form|frame|frameset|h[1-6]|head|header|hr|html|iframe|legend|li|link|main|menu|menuitem|nav|noframes|ol|optgroup|option|p|param|search|section|summary|table|tbody|td|tfoot|th|thead|title|tr|track|ul)\b)/im.test(
      text
    )
  ) {
    return text.trim() ? [text] : [];
  }

  const lines = text.split(/(?<=\n)/);
  const blocks: string[] = [];
  let start = 0;
  let fence: "```" | "~~~" | null = null;
  let displayMath = false;
  let blockContainsList = false;
  let blockContainsIndentedCode = false;

  const isListItem = (line: string) =>
    /^[ \t]{0,3}(?:[-+*]|\d{1,9}[.)])[ \t]+/.test(line);
  const isIndentedContinuation = (line: string) => /^(?:\t| {2,})\S/.test(line);

  const nextNonBlankLine = (from: number): string => {
    for (let index = from; index < lines.length; index += 1) {
      if (lines[index].trim()) return lines[index];
    }
    return "";
  };

  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    const trimmed = lines[index].trim();
    if (!displayMath && (trimmed.startsWith("```") || trimmed.startsWith("~~~"))) {
      const marker = trimmed.slice(0, 3) as "```" | "~~~";
      fence = fence === marker ? null : fence ?? marker;
      continue;
    }
    if (!fence && trimmed === "$$") {
      displayMath = !displayMath;
      continue;
    }
    if (!fence && !displayMath && trimmed) {
      blockContainsList ||= isListItem(line);
      blockContainsIndentedCode ||= /^(?:\t| {4,})\S/.test(line);
    }
    if (!fence && !displayMath && trimmed === "" && index >= start) {
      const nextLine = nextNonBlankLine(index + 1);
      const continuesList =
        blockContainsList &&
        (isListItem(nextLine) || isIndentedContinuation(nextLine));
      const continuesIndentedCode =
        blockContainsIndentedCode && isIndentedContinuation(nextLine);
      if (continuesList || continuesIndentedCode) continue;

      const block = lines.slice(start, index + 1).join("");
      if (block.trim()) blocks.push(block);
      start = index + 1;
      blockContainsList = false;
      blockContainsIndentedCode = false;
    }
  }

  const remainder = lines.slice(start).join("");
  if (remainder.trim()) blocks.push(remainder);
  return blocks;
}
