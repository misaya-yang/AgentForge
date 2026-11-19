import type { ReactNode } from "react";
import { Modal } from "@/components/ui/native-modal";
import { toast } from "@/hooks/use-toast";

type Notice = ReactNode | { content: ReactNode; key?: string | number };
const notices = new Map<string, () => void>();
function show(content: Notice, destructive = false) {
  const structured = content !== null && typeof content === "object" && "content" in content;
  const id = structured && content.key !== undefined ? String(content.key) : undefined;
  if (id) notices.get(id)?.();
  const handle = toast({ description: structured ? content.content : content, variant: destructive ? "destructive" : "default" });
  if (id) notices.set(id, handle.dismiss);
  return handle.dismiss;
}
const message = { success: show, info: show, warning: show, error: (content: Notice) => show(content, true), loading: show,
  destroy: (key?: string | number) => { if (key !== undefined) { notices.get(String(key))?.(); notices.delete(String(key)); } else { for (const dismiss of notices.values()) dismiss(); notices.clear(); } } };
/** All console routes share the existing toast store and native confirmation UI. */
export function useProductFeedback() { return { message, modal: Modal }; }
