export const OUTPUT_PAGE_SIZE = 20;
export const OUTPUT_PREVIEW_LIMIT = 16000;

/** Bounded display serialization, not a complete export or response-size guarantee. */
export function taskOutputPreview(value: unknown): { text: string; truncated: boolean } {
  let text = ""; let truncated = false; let nodes = 0;
  const append = (part: string) => { const left = OUTPUT_PREVIEW_LIMIT - text.length; text += part.slice(0, left); if (part.length > left) truncated = true; };
  function visit(item: unknown, depth: number) {
    if (text.length >= OUTPUT_PREVIEW_LIMIT || ++nodes > 512 || depth > 8) { truncated = true; return; }
    if (typeof item === "string") { const excerpt = item.slice(0, OUTPUT_PREVIEW_LIMIT); if (excerpt.length < item.length) truncated = true; append(JSON.stringify(excerpt)); }
    else if (item === null || typeof item === "number" || typeof item === "boolean") append(String(item));
    else if (Array.isArray(item)) {
      append("["); for (let i = 0; i < item.length; i++) { if (i >= 50 || text.length >= OUTPUT_PREVIEW_LIMIT || nodes >= 512) { truncated = true; break; } if (i) append(", "); visit(item[i], depth + 1); } append("]");
    } else if (item && typeof item === "object") {
      append("{"); let count = 0;
      for (const key in item) { if (!Object.prototype.hasOwnProperty.call(item, key)) continue; if (count >= 50 || text.length >= OUTPUT_PREVIEW_LIMIT || nodes >= 512) { truncated = true; break; } if (count++) append(", "); append(JSON.stringify(key.slice(0, 256))); if (key.length > 256) truncated = true; append(": "); visit((item as Record<string, unknown>)[key], depth + 1); }
      append("}");
    } else append("null");
  }
  if (typeof value === "string") { text = value.slice(0, OUTPUT_PREVIEW_LIMIT); truncated = text.length < value.length; }
  else visit(value, 0);
  return { text, truncated };
}

export function taskOutputItems(result: unknown, taskId: string, requestId: string): unknown[] | null {
  if (!result || typeof result !== "object") return null;
  const value = result as Record<string, unknown>;
  if (!requestId || value.request_id !== requestId || (value.task_id != null && value.task_id !== taskId) || !Array.isArray(value.outputs)) return null;
  return value.outputs;
}
