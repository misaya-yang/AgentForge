import { create } from "zustand";
import { persist } from "zustand/middleware";

export type ThemeMode = "light" | "dark" | "system";
export type ResolvedTheme = "light" | "dark";

function getSystemTheme(): ResolvedTheme {
  if (typeof window === "undefined") return "light";
  return window.matchMedia("(prefers-color-scheme: dark)").matches
    ? "dark"
    : "light";
}

type AppState = {
  // === Playground (智能对话) 状态 ===
  selectedServiceId?: string;
  setSelectedServiceId: (id?: string) => void;
  activeSessionId?: string;  // Playground 的活动会话
  setActiveSessionId: (id?: string) => void;
  playgroundSidebarOpen: boolean;
  setPlaygroundSidebarOpen: (open: boolean) => void;
  localTitles: Record<string, string>;  // Playground 的会话标题缓存
  setLocalTitles: (titles: Record<string, string> | ((prev: Record<string, string>) => Record<string, string>)) => void;
  
  // === Assistant (AI助手) 独立状态 ===
  assistantActiveSessionId?: string;  // AI助手 的活动会话（与 Playground 完全分离）
  setAssistantActiveSessionId: (id?: string) => void;
  assistantSidebarOpen: boolean;
  setAssistantSidebarOpen: (open: boolean) => void;
  assistantLocalTitles: Record<string, string>;  // AI助手 的会话标题缓存
  setAssistantLocalTitles: (titles: Record<string, string> | ((prev: Record<string, string>) => Record<string, string>)) => void;
  
  // === 全局状态 ===
  themeMode: ThemeMode;
  resolvedTheme: ResolvedTheme;
  darkMode: boolean;
  setThemeMode: (mode: ThemeMode) => void;
  setResolvedTheme: (theme: ResolvedTheme) => void;
  toggleDarkMode: () => void;
};

export const useAppStore = create<AppState>()(
  persist(
    (set) => ({
      // Playground (智能对话)
      selectedServiceId: undefined,
      setSelectedServiceId: (id) => set({ selectedServiceId: id }),
      activeSessionId: undefined,
      setActiveSessionId: (id) => set({ activeSessionId: id }),
      playgroundSidebarOpen: false,
      setPlaygroundSidebarOpen: (open) => set({ playgroundSidebarOpen: open }),
      localTitles: {},
      setLocalTitles: (updater) => 
        set((state) => ({
          localTitles: typeof updater === 'function' ? updater(state.localTitles) : updater
        })),
      
      // Assistant (AI助手) - 完全独立的状态
      assistantActiveSessionId: undefined,
      setAssistantActiveSessionId: (id) => set({ assistantActiveSessionId: id }),
      assistantSidebarOpen: false,
      setAssistantSidebarOpen: (open) => set({ assistantSidebarOpen: open }),
      assistantLocalTitles: {},
      setAssistantLocalTitles: (updater) =>
        set((state) => ({
          assistantLocalTitles: typeof updater === 'function' ? updater(state.assistantLocalTitles) : updater
        })),
      
      // 全局
      themeMode: "system",
      resolvedTheme: getSystemTheme(),
      darkMode: getSystemTheme() === "dark",
      setThemeMode: (mode) =>
        set((state) => {
          if (mode === "system") {
            const systemTheme = getSystemTheme();
            return {
              themeMode: mode,
              resolvedTheme: systemTheme,
              darkMode: systemTheme === "dark",
            };
          }
          const resolvedTheme = mode;
          return {
            ...state,
            themeMode: mode,
            resolvedTheme,
            darkMode: resolvedTheme === "dark",
          };
        }),
      setResolvedTheme: (theme) =>
        set((state) => ({
          ...state,
          resolvedTheme: theme,
          darkMode: theme === "dark",
        })),
      toggleDarkMode: () =>
        set((s) => ({
          themeMode: s.resolvedTheme === "dark" ? "light" : "dark",
          resolvedTheme: s.resolvedTheme === "dark" ? "light" : "dark",
          darkMode: s.resolvedTheme !== "dark",
        })),
    }),
    {
      name: "agent-gateway-storage",
      version: 3,
      migrate: (persistedState: unknown, version: number) => {
        const state = (persistedState || {}) as Partial<AppState>;
        if (version < 2 && typeof state.darkMode === "boolean" && !state.themeMode) {
          const nextTheme = state.darkMode ? "dark" : "light";
          return {
            ...state,
            themeMode: nextTheme,
            resolvedTheme: nextTheme,
          };
        }
        if (version < 3) {
          return {
            ...state,
            playgroundSidebarOpen:
              typeof state.playgroundSidebarOpen === "boolean"
                ? state.playgroundSidebarOpen
                : false,
            assistantSidebarOpen:
              typeof state.assistantSidebarOpen === "boolean"
                ? state.assistantSidebarOpen
                : false,
          } as AppState;
        }
        return state as AppState;
      },
    }
  )
);
