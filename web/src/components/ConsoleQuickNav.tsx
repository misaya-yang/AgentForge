import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { Search, ArrowUpRight } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Dialog, DialogContent, DialogTitle, DialogDescription, DialogTrigger } from "@/components/ui/dialog";
import type { NavItem } from "@/config/consoleNavigation";

export function ConsoleQuickNav({ items }: { items: NavItem[] }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);
  const matches = items.filter(item => `${t(item.labelKey)} ${item.key}`.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase()));
  const activeIndex = Math.min(selected, Math.max(0, matches.length - 1));

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setOpen(value => !value);
        setQuery("");
        setSelected(0);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  useEffect(() => {
    if (open) listRef.current?.querySelector(`[data-index="${activeIndex}"]`)?.scrollIntoView({ block: "nearest" });
  }, [activeIndex, open]);

  const choose = (item: NavItem) => { setOpen(false); navigate(item.key); };
  return (
    <Dialog open={open} onOpenChange={(value) => { setOpen(value); setQuery(""); setSelected(0); }}>
      <DialogTrigger asChild>
        <button className="console-search-trigger" type="button" aria-label={t("workbench.search")}>
          <Search size={16} aria-hidden="true" />
          <span>{t("workbench.search")}</span><kbd>Ctrl / ⌘ K</kbd>
        </button>
      </DialogTrigger>
      <DialogContent className="console-command-dialog">
        <DialogTitle>{t("workbench.search")}</DialogTitle>
        <DialogDescription>{t("workbench.searchHint")}</DialogDescription>
        <div className="console-command-input">
          <Search size={18} aria-hidden="true" />
          <input autoFocus value={query} onChange={(event) => { setQuery(event.target.value); setSelected(0); }}
            placeholder={t("workbench.searchPlaceholder")} aria-label={t("workbench.search")}
            role="combobox" aria-autocomplete="list" aria-expanded="true" aria-controls="console-command-results"
            aria-activedescendant={matches.length ? `console-command-${activeIndex}` : undefined}
            onKeyDown={(event) => {
              if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                event.preventDefault();
                if (matches.length) setSelected((activeIndex + (event.key === "ArrowDown" ? 1 : -1) + matches.length) % matches.length);
              }
              if (event.key === "Enter" && matches[activeIndex]) { event.preventDefault(); choose(matches[activeIndex]); }
            }} />
        </div>
        <div ref={listRef} id="console-command-results" role="listbox" aria-label={t("workbench.destinations")} className="console-command-results">
          {matches.map((item, index) => <div key={item.key} id={`console-command-${index}`} data-index={index}
            role="option" aria-selected={index === activeIndex} className="console-command-option"
            onMouseMove={() => setSelected(index)} onClick={() => choose(item)}>
            <item.icon size={18} aria-hidden="true" /><span>{t(item.labelKey)}</span><ArrowUpRight size={15} aria-hidden="true" />
          </div>)}
          {!matches.length && <p role="status" className="console-empty">{t("workbench.noResults")}</p>}
        </div>
      </DialogContent>
    </Dialog>
  );
}
