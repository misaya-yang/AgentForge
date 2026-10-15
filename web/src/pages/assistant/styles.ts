/**
 * Assistant Style Presets
 *
 * Optional response styles sent as lower-priority user guidance. They shape
 * presentation without overriding the current request or runtime policy.
 */
import i18n from "@/i18n";

export interface AssistantStyle {
  id: string;
  nameKey: string;
  descriptionKey: string;
  systemPromptKey?: string;
}

export const ASSISTANT_STYLES: AssistantStyle[] = [
  {
    id: "custom",
    nameKey: "assistant.styles.custom.name",
    descriptionKey: "assistant.styles.custom.desc",
  },
  {
    id: "concise",
    nameKey: "assistant.styles.concise.name",
    descriptionKey: "assistant.styles.concise.desc",
    systemPromptKey: "assistant.styles.concise.prompt",
  },
  {
    id: "formal",
    nameKey: "assistant.styles.formal.name",
    descriptionKey: "assistant.styles.formal.desc",
    systemPromptKey: "assistant.styles.formal.prompt",
  },
  {
    id: "socratic",
    nameKey: "assistant.styles.socratic.name",
    descriptionKey: "assistant.styles.socratic.desc",
    systemPromptKey: "assistant.styles.socratic.prompt",
  },
  {
    id: "comprehensive",
    nameKey: "assistant.styles.comprehensive.name",
    descriptionKey: "assistant.styles.comprehensive.desc",
    systemPromptKey: "assistant.styles.comprehensive.prompt",
  },
];

export const DEFAULT_STYLE_ID = "custom";

/**
 * Get style by ID
 */
export function getStyleById(id: string): AssistantStyle | undefined {
  return ASSISTANT_STYLES.find((style) => style.id === id);
}

/**
 * Get the system prompt for a style
 */
export function getStyleSystemPrompt(styleId: string): string | null {
  const style = getStyleById(styleId);
  if (!style?.systemPromptKey) return null;
  return i18n.t(style.systemPromptKey);
}
