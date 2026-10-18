import { useState, useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { NavLink, Outlet, useLocation, useNavigate } from "react-router-dom";
import { Layout, Typography, Dropdown } from "antd";
import type { MenuProps } from "antd";
import {
  PanelLeftClose,
  PanelLeft,
  Sun,
  Moon,
  User,
  HelpCircle,
  LogOut,
  Globe,
  Monitor,
  Menu,
  Lock,
  type LucideIcon,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { useAppStore } from "@/store/useAppStore";
import { useAuthStore } from "@/store/useAuthStore";
import { logout } from "@/api/auth";
import { HelpModal } from "@/components/HelpModal";
import { PasswordChangeModal } from "@/components/PasswordChangeModal";
import { ProfileModal } from "@/components/ProfileModal";
import { SetupBanner } from "@/components/SetupBanner";
import { Logo } from "@/components/Logo";
import { changeAppLanguage, languages } from "@/i18n";
import { GROUP_ORDER, useConsoleNavigation, type NavGroup, type NavItem } from "@/config/consoleNavigation";
import { ConsoleQuickNav } from "@/components/ConsoleQuickNav";
import "@/styles/enterprise-console.css";
import { preloadNavigationIntent } from "@/lib/routePreload";

const { Sider, Content, Header } = Layout;
const { Text } = Typography;

// Icons and labels are visually paired — keep size/stroke in sync.
// Thinner 1.5 stroke reads as more editorial than the default 2, and
// 18px keeps the collapsed rail comfortable without cramping.
const NAV_ICON_SIZE = 18;
const NAV_ICON_STROKE = 1.5;
const HEADER_HEIGHT = 56;

function NavItemIcon({ icon: Icon, size = NAV_ICON_SIZE }: { icon: LucideIcon; size?: number }) {
  return <Icon size={size} strokeWidth={NAV_ICON_STROKE} />;
}

function getPageChrome(pathname: string) {
  const segment = pathname.split("/")[1] || "dashboard";
  const map: Record<string, { titleKey: string; titleFallback: string; subtitleKey?: string; subtitleFallback?: string }> = {
    workbench: { titleKey: "nav.workbench", titleFallback: "Workbench", subtitleKey: "workbench.subtitle", subtitleFallback: "Your AI workspace, connected." },
    dashboard: {
      titleKey: "metrics.title",
      titleFallback: "Monitoring Dashboard",
      subtitleKey: "dashboard.command.subtitle",
      subtitleFallback: "AI Gateway operations, reliability, governance and request trace observability",
    },
    services: {
      titleKey: "services.page.title",
      titleFallback: "Service Management",
      subtitleKey: "services.page.subtitle",
      subtitleFallback: "Manage all your AI services, providers, and models in one place.",
    },
    knowledge: {
      titleKey: "knowledge.datasets.title",
      titleFallback: "Knowledge Base Management",
      subtitleKey: "knowledge.datasets.subtitle",
      subtitleFallback: "Manage and search your AI knowledge assets",
    },
    playground: { titleKey: "nav.playground", titleFallback: "Playground" },
    assistant: { titleKey: "nav.assistant", titleFallback: "AI Assistant" },
    agents: {
      titleKey: "nav.agents",
      titleFallback: "Agents",
      subtitleKey: "agents.list.subtitle",
      subtitleFallback: "Create, configure, test, and publish reusable agents.",
    },
    eval: {
      titleKey: "eval.title",
      titleFallback: "Eval Console",
      subtitleKey: "eval.description",
      subtitleFallback: "Review assistant, LangGraph proxy, and RAG traces with bounded previews and human scoring.",
    },
    tasks: { titleKey: "nav.tasks", titleFallback: "Tasks" },
    users: { titleKey: "nav.users", titleFallback: "Users" },
    settings: { titleKey: "nav.settings", titleFallback: "Settings" },
  };
  return map[segment] || map.dashboard;
}

function getInitials(name?: string): string {
  if (!name) return "U";
  const parts = name.trim().split(/\s+/);
  if (parts.length >= 2) return (parts[0][0] + parts[parts.length - 1][0]).toUpperCase();
  return name.slice(0, 2).toUpperCase();
}

export function AppLayout() {
  const { t } = useTranslation();
  const [collapsed, setCollapsed] = useState(false);
  const [isMobile, setIsMobile] = useState(false);
  const [helpModalOpen, setHelpModalOpen] = useState(false);
  const [showPasswordChange, setShowPasswordChange] = useState(false);
  const { resolvedTheme, darkMode, setThemeMode, toggleDarkMode } = useAppStore();
  const { user, clearAuth, forcePasswordChange, setForcePasswordChange } = useAuthStore();
  const location = useLocation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [showProfileModal, setShowProfileModal] = useState(false);

  // Re-check setup state on every route change so the banner disappears as
  // soon as a provider gets configured (e.g. right after the Services page
  // saves a key). The 5-minute staleTime keeps repeat navigations cheap.
  useEffect(() => {
    queryClient.invalidateQueries({ queryKey: ["setup-state"] });
  }, [location.pathname, queryClient]);

  const handleLanguageChange = (langCode: string) => {
    void changeAppLanguage(langCode);
  };

  useEffect(() => {
    const mq = window.matchMedia("(max-width: 1024px)");
    const sync = () => { const m = mq.matches; setIsMobile(m); if (m) setCollapsed(true); };
    sync();
    mq.addEventListener("change", sync);
    return () => mq.removeEventListener("change", sync);
  }, []);

  const languageMenuItems: MenuProps['items'] = languages.map(lang => ({
    key: lang.code,
    label: `${lang.flag} ${t(lang.nameKey, lang.nativeName)}`,
    onClick: () => handleLanguageChange(lang.code),
  }));

  const themeMenuItems: MenuProps["items"] = [
    { key: "light", icon: <Sun size={14} />, label: t("theme.mode.light", "Light"), onClick: () => setThemeMode("light") },
    { key: "dark", icon: <Moon size={14} />, label: t("theme.mode.dark", "Dark"), onClick: () => setThemeMode("dark") },
    { key: "system", icon: <Monitor size={14} />, label: t("theme.mode.system", "System"), onClick: () => setThemeMode("system") },
  ];
  const themeToggleLabel = darkMode
    ? t("theme.switchToLight", "Switch to light")
    : t("theme.switchToDark", "Switch to dark");

  const userMenuItems: MenuProps['items'] = [
    { key: 'profile', label: t('user.profile'), icon: <User size={14} /> },
    { key: 'change-password', label: t('user.changePassword'), icon: <Lock size={14} /> },
    { key: "theme", label: t("theme.title", "Theme"), icon: <Monitor size={14} />, children: themeMenuItems },
    { key: 'language', label: t('user.language'), icon: <Globe size={14} />, children: languageMenuItems },
    { key: 'help', label: t('help.title'), icon: <HelpCircle size={14} /> },
    { type: 'divider' },
    { key: 'logout', label: t('user.logout'), icon: <LogOut size={14} />, danger: true },
  ];

  const handleUserMenuClick = async ({ key }: { key: string }) => {
    if (key === 'logout') {
      try {
        await logout();
      } catch {
        // Still clear the local session if the server-side logout call fails.
      }
      clearAuth();
      navigate('/login');
    }
    else if (key === 'profile') setShowProfileModal(true);
    else if (key === 'change-password') setShowPasswordChange(true);
    else if (key === 'help') setHelpModalOpen(true);
  };

  useEffect(() => {
    if (!forcePasswordChange) return;
    const timer = window.setTimeout(() => setShowPasswordChange(true), 0);
    return () => window.clearTimeout(timer);
  }, [forcePasswordChange]);

  const { items: finalNavItems, isModelTesterOnly } = useConsoleNavigation();
  // Group headings only when the rail is expanded; the model_tester path and
  // the collapsed rail render items flat (icons + tooltips only).
  const showGroupHeadings = !isModelTesterOnly && !collapsed;
  const ungroupedItems = finalNavItems.filter(item => !item.group);
  const groupSections = GROUP_ORDER.map((group) => ({
    group,
    items: finalNavItems.filter(item => item.group === group),
  })).filter(section => section.items.length > 0);
  const navRows: Array<
    | { kind: "item"; item: NavItem }
    | { kind: "group-heading"; group: NavGroup }
  > = showGroupHeadings
    ? [
        ...ungroupedItems.map(item => ({ kind: "item" as const, item })),
        ...groupSections.flatMap(section => [
          { kind: "group-heading" as const, group: section.group },
          ...section.items.map(item => ({ kind: "item" as const, item })),
        ]),
      ]
    : finalNavItems.map(item => ({ kind: "item" as const, item }));
  // 187 = 220 × 0.85 (narrowed 15% per 2026-04-24 feedback). Keeps
  // "Knowledge Base" on one line with the 14.5px/400 label and a 12px
  // icon-gap, verified at 1440×900.
  const SIDER_WIDTH = 187;
  const siderOffset = isMobile ? (collapsed ? -SIDER_WIDTH : 0) : 0;
  const contentMarginLeft = isMobile ? 0 : collapsed ? 64 : SIDER_WIDTH;
  const floatingSidebarOpen = isMobile && !collapsed;
  const userInitials = getInitials(user?.display_name || user?.user_id);
  const pageChrome = getPageChrome(location.pathname);
  const pageTitle = t(pageChrome.titleKey, pageChrome.titleFallback);
  const pageSubtitle = pageChrome.subtitleKey
    ? t(pageChrome.subtitleKey, pageChrome.subtitleFallback || "")
    : "";

  useEffect(() => {
    if (!floatingSidebarOpen) return;
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setCollapsed(true);
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [floatingSidebarOpen]);

  return (
    <Layout className="enterprise-console" onPointerOver={preloadNavigationIntent} onFocusCapture={preloadNavigationIntent} style={{ minHeight: '100dvh' }}>
      <a className="console-skip-link" href="#console-main">{t("workbench.skipContent")}</a>
      {floatingSidebarOpen && (
        <button
          type="button"
          aria-label={t("nav.closeSidebar", "Close sidebar")}
          className="app-sidebar-hit-area"
          style={{ left: SIDER_WIDTH }}
          onClick={() => setCollapsed(true)}
        />
      )}
      <Sider
        collapsible collapsed={collapsed} onCollapse={setCollapsed} trigger={null}
        width={SIDER_WIDTH} collapsedWidth={isMobile ? 0 : 64}
        style={{
          position: 'fixed', left: isMobile ? siderOffset : 0, top: 0, bottom: 0, zIndex: 40,
          borderRight: '1px solid hsl(var(--border))',
          background: 'hsl(var(--sidebar-bg))',
          transition: 'left 0.3s cubic-bezier(0.16, 1, 0.3, 1), width 0.3s cubic-bezier(0.16, 1, 0.3, 1), min-width 0.3s cubic-bezier(0.16, 1, 0.3, 1), max-width 0.3s cubic-bezier(0.16, 1, 0.3, 1)',
        }}
        theme={resolvedTheme}
      >
        <div className="flex flex-col h-full">
          {/* Brand — slightly taller (64px) to breathe at the top rail. */}
          <div
            className={`flex items-center gap-2.5 border-b border-border/60 ${collapsed ? 'justify-center px-0' : 'px-[18px]'}`}
            style={{ height: 64, flexShrink: 0 }}
          >
            <Logo collapsed={collapsed} />
          </div>

          {/* Navigation — extra top/bottom room (py-4) so items aren't
              squeezed against the brand and footer rails. Gap-[3px]
              gives items a small breath without floating apart. */}
          <nav aria-label={t("workbench.primaryNav")} className="flex-1 overflow-y-auto px-2.5 scrollbar-hide py-4">
            <div className="flex flex-col gap-[3px]">
              {navRows.map((row) => {
                if (row.kind === "group-heading") {
                  return (
                    <div
                      key={row.group}
                      className="app-nav-group-label mt-2.5 px-[10px] text-[11px] font-medium uppercase tracking-wide text-muted-foreground/60 first:mt-0"
                    >
                      {t(`nav.group.${row.group}`)}
                    </div>
                  );
                }
                const item = row.item;
                return (
                <NavLink
                  key={item.key}
                  to={item.key}
                  aria-label={t(item.labelKey)}
                  onClick={() => { if (isMobile) setCollapsed(true); }}
                  className={({ isActive }) =>
                    `app-nav-link relative group flex items-center rounded-lg transition-colors duration-140 ease-out ${
                      collapsed ? 'justify-center py-[11px]' : 'gap-[10px] px-[10px] py-[11px]'
                    } ${isActive ? 'app-nav-item-active' : ''}`
                  }
                >
                  {({ isActive }) => (
                    <>
                      {isActive && !collapsed && (
                        <span
                          aria-hidden
                          className="absolute left-0 top-2 bottom-2 w-[3px]"
                          style={{
                            background: 'hsl(var(--primary))',
                            borderRadius: '0 2px 2px 0',
                          }}
                        />
                      )}
                      <span className="app-nav-icon shrink-0">
                        <NavItemIcon icon={item.icon} />
                      </span>
                      {!collapsed && (
                        <span className="app-nav-label truncate">
                          {t(item.labelKey)}
                        </span>
                      )}
                      {collapsed && (
                        <div className="absolute left-full ml-2 px-2 py-1 bg-popover text-popover-foreground text-xs rounded-md shadow-lg opacity-0 scale-95 group-hover:opacity-100 group-hover:scale-100 group-focus-visible:opacity-100 group-focus-visible:scale-100 transition-[opacity,transform] duration-100 pointer-events-none whitespace-nowrap z-50 border border-border/50">
                          {t(item.labelKey)}
                        </div>
                      )}
                    </>
                  )}
                </NavLink>
                );
              })}
            </div>
          </nav>

          {/* Footer — theme + collapse. Same vertical rhythm as the
              nav items above (py-[11px] + gap-[3px]) so the rail feels
              continuous rather than split. */}
          <div className="px-2.5 py-3 border-t border-border/60 flex flex-col gap-[3px]">
            <button
              type="button"
              onClick={toggleDarkMode}
              aria-label={themeToggleLabel}
              className={`app-nav-link flex items-center rounded-lg transition-colors duration-140 ${
                collapsed ? 'justify-center py-[10px]' : 'gap-[10px] px-[10px] py-[10px]'
              }`}
            >
              <span className="app-nav-icon shrink-0">
                {darkMode ? <Sun size={NAV_ICON_SIZE} strokeWidth={NAV_ICON_STROKE} /> : <Moon size={NAV_ICON_SIZE} strokeWidth={NAV_ICON_STROKE} />}
              </span>
              {!collapsed && (
                <span className="app-nav-label flex-1 text-left truncate">
                  {themeToggleLabel}
                </span>
              )}
            </button>
            <button
              type="button"
              onClick={() => setCollapsed(!collapsed)}
              aria-label={collapsed ? t("nav.expandSidebar", "Expand") : t("nav.collapseSidebar")}
              className={`app-nav-link flex items-center rounded-lg transition-colors duration-140 ${
                collapsed ? 'justify-center py-[10px]' : 'gap-[10px] px-[10px] py-[10px]'
              }`}
            >
              <span className="app-nav-icon shrink-0" style={{ transform: collapsed ? 'rotate(180deg)' : 'none', transition: 'transform .18s ease' }}>
                {collapsed ? <PanelLeft size={NAV_ICON_SIZE} strokeWidth={NAV_ICON_STROKE} /> : <PanelLeftClose size={NAV_ICON_SIZE} strokeWidth={NAV_ICON_STROKE} />}
              </span>
              {!collapsed && (
                <span className="app-nav-label truncate">{t('nav.collapseSidebar', 'Collapse sidebar')}</span>
              )}
            </button>
          </div>
        </div>
      </Sider>

      <Layout style={{ marginLeft: contentMarginLeft, transition: 'margin-left 0.3s cubic-bezier(0.16, 1, 0.3, 1)', background: 'transparent', minHeight: '100dvh' }}>
        {/* Header */}
        <Header style={{
          padding: isMobile ? '0 10px' : '0 16px',
          background: 'hsl(var(--background) / 0.9)',
          backdropFilter: 'blur(12px)',
          borderBottom: '1px solid hsl(var(--border))',
          display: 'flex', alignItems: 'center', justifyContent: 'space-between',
          gap: 12, position: 'relative', zIndex: 30, height: HEADER_HEIGHT,
          lineHeight: 'normal',
        }}>
          <div className="flex min-w-0 flex-1 items-center gap-3">
            {isMobile && (
              <button type="button" onClick={() => setCollapsed(p => !p)} className="h-8 w-8 shrink-0 inline-flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground" aria-label={t("nav.toggleSidebar", "Toggle sidebar")}>
                <Menu size={18} />
              </button>
            )}
            <div className="app-header-title min-w-0">
              <h1 className="app-header-title-text truncate">{pageTitle}</h1>
              {pageSubtitle && (
                <p className="app-header-subtitle truncate">{pageSubtitle}</p>
              )}
            </div>
          </div>
          <ConsoleQuickNav items={finalNavItems} />
          <Dropdown menu={{ items: userMenuItems, onClick: handleUserMenuClick }} trigger={['click']}>
            <button
              type="button"
              aria-label={t("user.openMenu", "Open user menu")}
              aria-haspopup="menu"
              className="flex shrink-0 items-center gap-2 rounded-md border-0 bg-transparent px-1.5 py-1 text-foreground transition-colors hover:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/30"
            >
              <div className="w-6 h-6 rounded-md flex items-center justify-center text-[10px] font-semibold text-primary bg-primary/10 border border-primary/15">
                {userInitials}
              </div>
              <Text className="hidden sm:inline" style={{ fontSize: 13, fontWeight: 500 }}>
                {user?.display_name || user?.user_id || t('common.user')}
              </Text>
            </button>
          </Dropdown>
        </Header>

        <Content id="console-main" tabIndex={-1} style={{ padding: isMobile ? '8px' : '12px 18px 18px', minHeight: `calc(100dvh - ${HEADER_HEIGHT}px)`, overflow: 'auto' }}>
          <SetupBanner />
          <div className="page-transition"><Outlet /></div>
        </Content>
      </Layout>

      <HelpModal open={helpModalOpen} onClose={() => setHelpModalOpen(false)} />
      <PasswordChangeModal
        open={showPasswordChange} allowClose={!forcePasswordChange}
        onClose={() => setShowPasswordChange(false)}
        onComplete={() => { setShowPasswordChange(false); setForcePasswordChange(false); navigate("/login", { replace: true }); }}
      />
      <ProfileModal open={showProfileModal} onClose={() => setShowProfileModal(false)} />

      <style>{`
        .ant-layout-sider-children { box-shadow: none !important; }
        .app-sidebar-hit-area {
          position: fixed;
          top: 0;
          right: 0;
          bottom: 0;
          z-index: 35;
          padding: 0;
          border: 0;
          background: hsl(228 12% 5% / 0.34);
          backdrop-filter: blur(2px);
          cursor: default;
        }
        .scrollbar-hide::-webkit-scrollbar { display: none; }
        .scrollbar-hide { -ms-overflow-style: none; scrollbar-width: none; }

        /* Base nav link — editorial tone: slightly larger but thinner
           text. 14.5px / weight 400 reads as the
           kind of sans you see in tool chrome from Linear / Raycast
           rather than a SaaS admin dashboard. */
        .app-nav-link {
          color: hsl(var(--muted-foreground));
          cursor: pointer;
          font-size: 14.5px;
        }
        .app-nav-link:hover { background: hsl(var(--muted) / 0.5); color: hsl(var(--foreground)); }
        .app-nav-link:hover .app-nav-icon { color: hsl(var(--foreground)); }
        .app-nav-icon {
          color: inherit;
          display: inline-flex;
          align-items: center;
        }
        .app-nav-label {
          font-family: "Geist Sans", "Geist", "Noto Sans SC", "PingFang SC", "Microsoft YaHei", system-ui, sans-serif;
          font-size: 14.5px;
          font-weight: 400;
          letter-spacing: 0;
          line-height: 1.25;
        }

        /* Active — keep the primary tint but drop from 600→520-ish
           weight so the shift from inactive→active is a tone change,
           not a weight jump. Prevents the "stand up and shout" look. */
        .app-nav-item-active {
          background: hsl(var(--primary) / 0.09);
          color: hsl(var(--primary)) !important;
        }
        .app-nav-item-active:hover {
          background: hsl(var(--primary) / 0.12);
        }
        .app-nav-item-active .app-nav-icon,
        .app-nav-item-active:hover .app-nav-icon {
          color: hsl(var(--primary));
        }
        .app-nav-item-active .app-nav-label {
          color: hsl(var(--primary));
          font-weight: 500;
        }
        .dark .app-nav-item-active { background: hsl(var(--primary) / 0.18); }
        .dark .app-nav-item-active:hover { background: hsl(var(--primary) / 0.24); }

        .app-header-title {
          display: flex;
          flex-direction: column;
          justify-content: center;
          gap: 2px;
        }
        .app-header-title-text {
          color: hsl(var(--foreground));
          font-size: 14.5px;
          font-weight: 650;
          line-height: 1.15;
          letter-spacing: 0;
        }
        .app-header-subtitle {
          color: hsl(var(--muted-foreground));
          font-size: 12px;
          font-weight: 400;
          line-height: 1.2;
          letter-spacing: 0;
        }
        @media (max-width: 720px) {
          .app-header-subtitle {
            display: none;
          }
        }
      `}</style>
    </Layout>
  );
}
