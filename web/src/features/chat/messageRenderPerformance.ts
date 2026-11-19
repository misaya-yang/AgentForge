import type { CSSProperties } from "react";

const OFFSCREEN_MESSAGE_STYLE: CSSProperties = {
  contentVisibility: "auto",
  containIntrinsicSize: "auto 180px",
};

export function messageContainmentStyle(
  isStreaming: boolean,
): CSSProperties | undefined {
  return isStreaming ? undefined : OFFSCREEN_MESSAGE_STYLE;
}

export function updateMessageById<T extends { id?: string }>(
  messages: T[],
  messageId: string,
  updater: (message: T) => T,
): T[] {
  const index = messages.findIndex((message) => message.id === messageId);
  if (index < 0) return messages;
  const next = [...messages];
  next[index] = updater(messages[index]);
  return next;
}
