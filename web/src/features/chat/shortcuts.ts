import { useEffect } from "react";
import { trackChatShortcut } from "./telemetry";

type ChatSurface = "assistant" | "playground";

interface UseChatShortcutsOptions {
  surface: ChatSurface;
  composerId: string;
  enabled?: boolean;
  onNewChat?: () => void;
  onStop?: () => void;
}

function isEditableElement(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName.toLowerCase();
  return tag === "input" || tag === "textarea" || tag === "select";
}

function focusComposer(composerId: string): boolean {
  const element = document.getElementById(composerId);
  if (!element) return false;
  if (element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement) {
    element.focus();
    return true;
  }
  if (element instanceof HTMLElement) {
    element.focus();
    return true;
  }
  return false;
}

export function useChatShortcuts({
  surface,
  composerId,
  enabled = true,
  onNewChat,
  onStop,
}: UseChatShortcutsOptions) {
  useEffect(() => {
    if (!enabled) return;

    const onKeyDown = (event: KeyboardEvent) => {
      const key = event.key.toLowerCase();
      const mod = event.metaKey || event.ctrlKey;
      const editable = isEditableElement(event.target);

      if (mod && key === "k") {
        event.preventDefault();
        if (focusComposer(composerId)) {
          trackChatShortcut(surface, "mod+k", "focus_composer");
        }
        return;
      }

      if (mod && event.shiftKey && key === "n" && onNewChat) {
        event.preventDefault();
        onNewChat();
        trackChatShortcut(surface, "mod+shift+n", "new_chat");
        return;
      }

      if (event.key === "Escape" && onStop) {
        // Escape should work even while typing, but avoid hijacking non-chat widgets.
        if (!editable || document.activeElement?.id === composerId) {
          onStop();
          trackChatShortcut(surface, "escape", "stop_stream");
        }
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [composerId, enabled, onNewChat, onStop, surface]);
}
