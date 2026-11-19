import { useEffect } from "react";
import { useAppStore } from "@/store/useAppStore";

type MediaQueryEventLike = MediaQueryListEvent | MediaQueryList;

function resolveSystemTheme(eventLike?: MediaQueryEventLike): "light" | "dark" {
  const isDark = eventLike
    ? eventLike.matches
    : window.matchMedia("(prefers-color-scheme: dark)").matches;
  return isDark ? "dark" : "light";
}

function applyThemeToDom(theme: "light" | "dark"): void {
  const root = document.documentElement;
  root.classList.toggle("dark", theme === "dark");
  root.style.colorScheme = theme;
  root.dataset.theme = theme;
}

export function useThemeSync(): "light" | "dark" {
  const themeMode = useAppStore((s) => s.themeMode);
  const resolvedTheme = useAppStore((s) => s.resolvedTheme);
  const setResolvedTheme = useAppStore((s) => s.setResolvedTheme);

  useEffect(() => {
    if (typeof window === "undefined") return;

    if (themeMode !== "system") {
      setResolvedTheme(themeMode);
      applyThemeToDom(themeMode);
      return;
    }

    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const syncFromSystem = (eventLike?: MediaQueryEventLike) => {
      const nextTheme = resolveSystemTheme(eventLike);
      setResolvedTheme(nextTheme);
      applyThemeToDom(nextTheme);
    };

    syncFromSystem(media);
    media.addEventListener("change", syncFromSystem);
    return () => media.removeEventListener("change", syncFromSystem);
  }, [setResolvedTheme, themeMode]);

  useEffect(() => {
    applyThemeToDom(resolvedTheme);
  }, [resolvedTheme]);

  return resolvedTheme;
}

