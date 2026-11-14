import { listItems, listSpaces, type SpaceRecord } from "@/api/spaces";
import { useAuthStore } from "@/store/useAuthStore";
import { useQuery } from "@tanstack/react-query";
import { ChevronDown, FileText, Globe, Image, LayoutGrid, PanelLeft, Plus, Presentation } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { useMediaQuery } from "@/hooks/useMediaQuery";
import { useTranslation } from "react-i18next";
import { Link, NavLink, Outlet, useLocation, useSearchParams } from "react-router-dom";
import { bindShell, installOverlayFallback } from "./overlay";
import "./spaces.css";
import { SpaceDialog } from "./ui";

const FILTERS = [
  { kind: "page", icon: FileText },
  { kind: "slide", icon: Presentation },
  { kind: "site", icon: Globe },
  { kind: "image", icon: Image },
] as const;

export function SpacesLayout() {
  const { t, i18n } = useTranslation();
  const shell = useRef<HTMLDivElement>(null);
  const isMobile = useMediaQuery("(max-width: 1024px)");
  const [navigationOpen, setNavigationOpen] = useState(false);
  const user = useAuthStore((state) => state.user);
  const location = useLocation();
  const [search] = useSearchParams();
  const kind = search.get("kind");
  const spaces = useQuery({ queryKey: ["spaces", "list"], queryFn: listSpaces });
  const recent = useQuery({
    queryKey: ["spaces", "recent", search.get("recent") === "all" ? 20 : 5],
    queryFn: () => listItems({ view: "recent", limit: search.get("recent") === "all" ? 20 : 5 }),
  });
  useEffect(() => {
    bindShell(shell.current);
    installOverlayFallback(shell.current ?? document.body);
  }, [spaces.data, recent.data, location.pathname]);

  const initials = (user?.display_name || user?.user_id || "U").slice(0, 2).toUpperCase();
  return (
    <div className="spaces-shell" ref={shell}>
      <Collapsible className="spaces-navigation" open={!isMobile || navigationOpen} onOpenChange={setNavigationOpen}>
        <CollapsibleTrigger asChild>
          <Button variant="ghost" size="sm" className="spaces-navigation-trigger" aria-controls="spaces-navigation-content" aria-expanded={!isMobile || navigationOpen}>
            <PanelLeft data-icon="inline-start" aria-hidden="true" />
            {t("spaces.brand")}
            <ChevronDown data-icon="inline-end" aria-hidden="true" />
          </Button>
        </CollapsibleTrigger>
        <CollapsibleContent id="spaces-navigation-content" inert={isMobile && !navigationOpen}>
      <aside className="spaces-sidebar" aria-label={t("spaces.brand")} onClick={event => {
        if (event.target instanceof Element && event.target.closest("a")) setNavigationOpen(false);
      }}>
        <p className="spaces-brand">
          {t("spaces.brand")}
          <button type="button" className="spaces-create" data-command="toggle-popover:space-create-menu" aria-haspopup="menu">
            <Plus size={14} aria-hidden="true" />
            {t("spaces.create")}
          </button>
        </p>
        <div id="space-create-menu" popover="auto">
          <div data-focusgroup="" aria-labelledby="space-create-label">
            <span id="space-create-label" hidden>{t("spaces.create")}</span>
            <button type="button" data-command="show-modal:space-create-page">{t("spaces.menu.page")}</button>
            <button type="button" data-command="show-modal:space-create-space">{t("spaces.menu.space")}</button>
            <button type="button" data-command="show-modal:space-upload">{t("spaces.menu.upload")}</button>
            <button type="button" data-command="show-modal:space-import">{t("spaces.menu.import")}</button>
          </div>
        </div>
        <nav>
          <ul className="spaces-nav">
            <li>
              <Link className="spaces-nav-link" to="/spaces" aria-current={location.pathname === "/spaces" && !kind ? "page" : undefined}>
                <LayoutGrid size={15} aria-hidden="true" />
                {t("spaces.nav.all")}
              </Link>
            </li>
            {FILTERS.map((filter) => (
              <li key={filter.kind}>
                <Link className="spaces-nav-link" to={`/spaces?kind=${filter.kind}`} aria-current={location.pathname === "/spaces" && kind === filter.kind ? "page" : undefined}>
                  <filter.icon size={15} aria-hidden="true" />
                  {t(`spaces.nav.${filter.kind}`)}
                </Link>
              </li>
            ))}
            <li>
              <button type="button" className="spaces-nav-button" data-command="show-modal:space-drive">
                {t("spaces.nav.drive")}
              </button>
            </li>
          </ul>
        </nav>
        <section aria-labelledby="spaces-recent-label">
          <div className="spaces-side-heading">
            <span id="spaces-recent-label">{t("spaces.recent")}</span>
            <NavLink className="spaces-text-button" to="/spaces?recent=all">{t("spaces.expandRecent")}</NavLink>
          </div>
          <ul className="spaces-side-list">
            {(recent.data?.items ?? []).map((item) => (
              <li key={item.item_id}>
                <NavLink className="spaces-side-link" to={`/spaces/item/${item.item_id}`}>
                  <span>{item.title}</span>
                </NavLink>
              </li>
            ))}
          </ul>
        </section>
        <section aria-labelledby="spaces-list-label">
          <div className="spaces-side-heading" id="spaces-list-label">{t("spaces.spaces")}</div>
          <ul className="spaces-side-list">
            {(spaces.data ?? []).map((space) => (
              <li key={space.space_id}>
                <NavLink className="spaces-side-link" to={`/spaces/${space.space_id}`}>
                  <span>{space.icon ? `${space.icon} ` : ""}{space.name}</span>
                </NavLink>
              </li>
            ))}
          </ul>
        </section>
        <NavLink className="spaces-nav-link" to="/spaces/trash" end>{t("spaces.trash")}</NavLink>
        <p className="spaces-account">
          <span className="spaces-avatar" aria-hidden="true">{initials}</span>
          <span>{t("spaces.account", { name: user?.display_name || user?.user_id || initials })}</span>
        </p>
      </aside>
        </CollapsibleContent>
      </Collapsible>
      <div className="spaces-main">
        <Outlet context={{ spaces: spaces.data ?? [], locale: i18n.language } satisfies { spaces: SpaceRecord[]; locale: string }} />
      </div>
      <SpaceDialog id="space-drive" titleId="space-drive-title" title={t("spaces.dialog.drive")}>
        <p>{t("spaces.dialog.driveBody")}</p>
        <form method="dialog" className="spaces-dialog-actions">
          <button value="cancel" autoFocus>{t("spaces.close")}</button>
        </form>
      </SpaceDialog>
    </div>
  );
}
