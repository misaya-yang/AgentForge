import { useCallback, useRef, type MouseEvent } from "react";
import { flushSync } from "react-dom";
import { useNavigate } from "react-router-dom";
import { preloadRoute } from "@/lib/routePreload";

export function useRouteTransition() {
  const navigate = useNavigate();
  const generation = useRef(0);
  const transition = useRef<ViewTransition | null>(null);
  return useCallback((event: MouseEvent<HTMLAnchorElement>) => {
    if (event.defaultPrevented || event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey
      || event.currentTarget.target || event.currentTarget.hasAttribute("download")) return;
    const url = new URL(event.currentTarget.href);
    if (url.origin !== location.origin || !document.startViewTransition || matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    event.preventDefault();
    const attempt = ++generation.current;
    const departure = location.href;
    void (async () => {
      await preloadRoute(url.pathname);
      if (attempt !== generation.current || location.href !== departure) return;
      transition.current?.skipTransition();
      transition.current = document.startViewTransition(() => {
        flushSync(() => navigate(`${url.pathname}${url.search}${url.hash}`));
      });
      try { await transition.current.finished; } catch { /* Skipped transitions still navigate. */ }
      if (attempt !== generation.current || location.href !== url.href) return;
      const heading = document.querySelector<HTMLElement>("main h1, main h2");
      if (heading) { heading.tabIndex = -1; heading.focus({ preventScroll: true }); }
    })();
  }, [navigate]);
}
