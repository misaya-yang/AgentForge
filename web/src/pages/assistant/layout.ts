export const ASSISTANT_COMPACT_MAX_WIDTH = 1199;
export const ASSISTANT_COMPACT_MEDIA_QUERY =
  `(max-width: ${ASSISTANT_COMPACT_MAX_WIDTH}px)`;

export function isAssistantCompactWidth(width: number): boolean {
  return width <= ASSISTANT_COMPACT_MAX_WIDTH;
}
