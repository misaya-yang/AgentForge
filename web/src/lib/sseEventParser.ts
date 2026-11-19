export interface ParsedSSEEvent<T> {
  event: string;
  data: T;
  id?: string;
}

/** Parse one normalized SSE event block, retaining the resume cursor. */
export function parseSseEventPart<T>(
  part: string,
  options: { preserveControlFrames?: boolean } = {},
): ParsedSSEEvent<T> | null {
  const lines = part.split("\n");
  let event = "";
  let id: string | undefined;
  const dataLines: string[] = [];

  for (const line of lines) {
    if (line.startsWith("event:")) {
      event = line.slice(6).trim();
    } else if (line.startsWith("id:")) {
      const value = line.slice(3).replace(/^ /, "");
      if (!value.includes("\0")) id = value;
    } else if (line.startsWith("data:")) {
      dataLines.push(line.slice(5).trim());
    }
  }

  const dataStr = dataLines.join("\n");
  // Some protocols use a data-less named event or [DONE] as a run terminal.
  // Expose the raw control frame only to callers that explicitly interpret it;
  // other consumers retain their existing data-only behavior.
  if (!dataStr || dataStr === "[DONE]") {
    if (!options.preserveControlFrames || (!event && !dataStr)) return null;
    return { event, data: (dataStr || undefined) as T, ...(id !== undefined ? { id } : {}) };
  }

  try {
    const parsed = JSON.parse(dataStr) as T;
    return { event, data: parsed, ...(id !== undefined ? { id } : {}) };
  } catch {
    return { event, data: dataStr as unknown as T, ...(id !== undefined ? { id } : {}) };
  }
}
