import { useCallback, useEffect, useRef, useState } from "react";

/**
 * Manages auto-scroll-to-bottom behaviour for a chat container.
 *
 * Returns a ref to attach to the scrollable element, a boolean for
 * showing a "scroll to bottom" button, and helpers to trigger scrolls.
 */
export function useScrollToBottom(messages: { role: string; isStreaming?: boolean }[]) {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const stickToBottomRef = useRef(true);
  const scrollRafRef = useRef<number | null>(null);
  const [showScrollToBottom, setShowScrollToBottom] = useState(false);

  const scheduleScrollToBottom = useCallback((behavior: ScrollBehavior) => {
    const el = scrollRef.current;
    if (!el) return;
    if (scrollRafRef.current !== null) return;
    scrollRafRef.current = requestAnimationFrame(() => {
      scrollRafRef.current = null;
      if (behavior === "auto") {
        el.scrollTop = el.scrollHeight;
      } else {
        el.scrollTo({ top: el.scrollHeight, behavior });
      }
    });
  }, []);

  // Attach scroll listener
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const handleScroll = () => {
      const offset = el.scrollHeight - el.scrollTop - el.clientHeight;
      stickToBottomRef.current = offset < 120;
      setShowScrollToBottom(offset > 200);
    };
    handleScroll();
    el.addEventListener("scroll", handleScroll, { passive: true });
    return () => {
      el.removeEventListener("scroll", handleScroll);
    };
  }, []);

  // Auto-scroll when messages change
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    if (!stickToBottomRef.current) return;
    const last = messages[messages.length - 1];
    const isStreaming = last?.role === "assistant" && last?.isStreaming;
    scheduleScrollToBottom(isStreaming ? "auto" : "smooth");
  }, [messages, scheduleScrollToBottom]);

  // Cleanup RAF on unmount
  useEffect(() => {
    return () => {
      if (scrollRafRef.current !== null) {
        cancelAnimationFrame(scrollRafRef.current);
        scrollRafRef.current = null;
      }
    };
  }, []);

  const scrollToBottom = useCallback(() => {
    stickToBottomRef.current = true;
    scheduleScrollToBottom("smooth");
  }, [scheduleScrollToBottom]);

  return {
    scrollRef,
    stickToBottomRef,
    showScrollToBottom,
    scheduleScrollToBottom,
    scrollToBottom,
  } as const;
}
