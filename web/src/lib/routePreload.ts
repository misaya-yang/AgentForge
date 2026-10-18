type Loader = () => Promise<unknown>;
export function createRoutePreloader(loaders: Record<string, Loader>) {
  const loaded = new Map<string, Promise<unknown>>();
  return (path: string): Promise<unknown> | undefined => {
    const key = Object.keys(loaders).sort((a, b) => b.length - a.length)
      .find(root => path === root || path.startsWith(`${root}/`));
    if (!key) return;
    let pending = loaded.get(key);
    if (!pending) {
      pending = loaders[key]().catch(() => { loaded.delete(key); });
      loaded.set(key, pending);
    }
    return pending;
  };
}
// Vite converts these imports to modulepreload links for their static closure.
const preload = createRoutePreloader({
  "/workbench": () => import("@/pages/workbench"),
  "/dashboard": () => import("@/pages/dashboard/index"),
  "/services": () => import("@/pages/Services"),
  "/knowledge/create": () => import("@/pages/knowledge/DatasetCreate"),
  "/knowledge": () => import("@/pages/knowledge/Datasets"),
  "/playground": () => import("@/pages/playground"),
  "/assistant": () => import("@/pages/assistant"),
  "/agents/new": () => import("@/pages/agents/AgentCreatePage"),
  "/agents": () => import("@/pages/agents/AgentListPage"),
  "/eval": () => import("@/pages/eval"),
  "/tasks": () => import("@/pages/tasks"),
  "/settings": () => import("@/pages/Settings"),
  "/settings/routing": () => import("@/pages/settings/RoutingPolicySettings"),
  "/settings/identity": () => import("@/pages/settings/EnterpriseIdentitySettings"),
  "/settings/mcp-registry": () => import("@/pages/settings/McpRegistrySettings"),
  "/connections": () => import("@/pages/Connections"),
  "/users": () => import("@/pages/UserManagement"),
});
export function preloadNavigationIntent(event: { target: EventTarget | null }) {
  const link = event.target instanceof Element ? event.target.closest("a[href]") : null;
  if (!link || link.hasAttribute("download")) return;
  const connection = (navigator as Navigator & { connection?: { saveData?: boolean; effectiveType?: string } }).connection;
  if (connection?.saveData || connection?.effectiveType === "2g") return;
  const url = new URL(link.getAttribute("href")!, window.location.href);
  if (url.origin === window.location.origin) void preload(url.pathname);
}
