import { LayoutDashboard, Server, BookOpen, Zap, Bot, ChartLine, ListTodo, Users, Settings, Boxes, PanelsTopLeft, type LucideIcon } from "lucide-react";
import { useAuthStore } from "@/store/useAuthStore";
import { isAgentStudioEnabled } from "@/config/runtime";

export type NavGroup = "use" | "build" | "govern";

export type NavItem = {
  key: string;
  labelKey: string;
  icon: LucideIcon;
  permission: string | null;
  group?: NavGroup;
};

// Dashboard stays ungrouped on top; the rest splits into 使用 (assistant +
// agents), 构建 (knowledge, playground, eval), and 治理 (services, users,
// tasks, settings). Empty groups collapse away during filtering below.
export const GROUP_ORDER: NavGroup[] = ["use", "build", "govern"];

export const navItems: NavItem[] = [
  { key: "/workbench", labelKey: "nav.workbench", icon: PanelsTopLeft, permission: null },
  { key: "/dashboard", labelKey: "nav.dashboard", icon: LayoutDashboard, permission: "console:dashboard:view" },
  { key: "/assistant", labelKey: "nav.assistant", icon: Bot, permission: "conversation:playground:access", group: "use" },
  { key: "/agents", labelKey: "nav.agents", icon: Boxes, permission: null, group: "use" },
  { key: "/knowledge", labelKey: "nav.knowledge", icon: BookOpen, permission: "knowledge:dataset:view", group: "build" },
  { key: "/playground", labelKey: "nav.playground", icon: Zap, permission: "conversation:playground:access", group: "build" },
  { key: "/eval", labelKey: "nav.eval", icon: ChartLine, permission: "console:eval:view", group: "build" },
  { key: "/services", labelKey: "nav.services", icon: Server, permission: "console:services:view", group: "govern" },
  { key: "/users", labelKey: "nav.users", icon: Users, permission: "user:list", group: "govern" },
  { key: "/tasks", labelKey: "nav.tasks", icon: ListTodo, permission: "console:dashboard:view", group: "govern" },
  { key: "/settings", labelKey: "nav.settings", icon: Settings, permission: "console:settings:view", group: "govern" },
];

/** Single permission policy shared by rail, quick navigation and workbench. */
export function useConsoleNavigation() {
  const { user, hasPermission } = useAuthStore();
  const roles = user?.roles || [];
  const isModelTesterOnly = roles.length === 1 && roles[0] === "model_tester";
  const items = navItems.filter((item) =>
    (item.key !== "/agents" || isAgentStudioEnabled()) &&
    (item.permission === null || hasPermission(item.permission)) &&
    (!isModelTesterOnly || item.key === "/playground")
  );
  return { items, isModelTesterOnly };
}
