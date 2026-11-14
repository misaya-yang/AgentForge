import {
  createItem,
  createSpace,
  deleteItem,
  importPages,
  listItems,
  patchItem,
  putContent,
  replaceShares,
  starItem,
  type ItemKind,
  type ItemView,
  type SpaceItem,
  type SpaceRecord,
} from "@/api/spaces";
import { useAuthStore } from "@/store/useAuthStore";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronDown, LayoutGrid, List, Search } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { Link, useOutletContext, useParams, useSearchParams } from "react-router-dom";
import { relativeTime, sourceLabel } from "./format";
import { bindShell, closeDialog, dialogForm, installOverlayFallback, openDialog } from "./overlay";
import { KindIcon, SpaceDialog } from "./ui";

const VIEWS: ItemView[] = ["all", "starred", "mine", "shared"];
const KINDS = new Set(["page", "slide", "site", "image", "file", "folder", "link", "external"]);

export function SpacesHome() {
  const { t, i18n } = useTranslation();
  const { spaceId } = useParams();
  const { spaces } = useOutletContext<{ spaces: SpaceRecord[] }>();
  const [search, setSearch] = useSearchParams();
  const [grid, setGrid] = useState(false);
  const [active, setActive] = useState<SpaceItem | null>(null);
  const [notice, setNotice] = useState("");
  const viewerId = useAuthStore((state) => state.user?.user_id ?? "");
  const client = useQueryClient();
  const view = VIEWS.includes(search.get("view") as ItemView) ? search.get("view") as ItemView : "all";
  const kind = KINDS.has(search.get("kind") ?? "") ? search.get("kind") as ItemKind : undefined;
  const q = search.get("q") ?? "";
  const items = useQuery({
    queryKey: ["spaces", "items", { view, kind, q, spaceId }],
    queryFn: () => listItems({ view, kind, q: q || undefined, space_id: spaceId, limit: 50 }),
  });
  const refresh = () => client.invalidateQueries({ queryKey: ["spaces"] });
  const fail = () => setNotice(t("spaces.error"));
  const createSpaceMutation = useMutation({ mutationFn: createSpace, onSuccess: refresh, onError: fail });
  const createItemMutation = useMutation({
    mutationFn: (input: { spaceId: string; title: string; kind: ItemKind; source: string; format: "markdown" | "binary"; markdown?: string }) =>
      createItem(input.spaceId, {
        title: input.title,
        kind: input.kind,
        source: input.source,
        content_format: input.format,
      }).then(async (created) => {
        if (input.markdown) {
          await putContent(created.item_id, created.current_revision, {
            content_format: "markdown",
            blocks: null,
            markdown: input.markdown,
          });
        }
        return created;
      }),
    onSuccess: refresh,
    onError: fail,
  });
  const patchMutation = useMutation({
    mutationFn: (input: { itemId: string; title?: string; space_id?: string }) => patchItem(input.itemId, {
      title: input.title,
      space_id: input.space_id,
    }),
    onSuccess: refresh,
    onError: fail,
  });
  const shareMutation = useMutation({
    mutationFn: (input: { itemId: string; user: string; role: "viewer" | "commenter" | "editor" | "owner" }) =>
      replaceShares(input.itemId, { shares: [{ principal_type: "user", principal_id: input.user, role: input.role }] }),
    onSuccess: refresh,
    onError: fail,
  });
  const starMutation = useMutation({ mutationFn: (item: SpaceItem) => starItem(item.item_id, !item.starred), onSuccess: refresh, onError: fail });
  const deleteMutation = useMutation({ mutationFn: (itemId: string) => deleteItem(itemId), onSuccess: refresh, onError: fail });

  const rows = items.data?.items ?? [];
  useEffect(() => {
    const shell = document.querySelector(".spaces-shell");
    if (shell instanceof HTMLElement) {
      bindShell(shell);
      installOverlayFallback(shell);
    }
  }, [items.data]);
  const sectionKind = kind === "page" || kind === "slide" || kind === "site" || kind === "image" ? kind : undefined;
  const heading = spaceId
    ? spaces.find((space) => space.space_id === spaceId)?.name ?? t("spaces.nav.all")
    : sectionKind ? t(`spaces.nav.${sectionKind}`) : t("spaces.nav.all");
  const targetSpace = spaceId || spaces[0]?.space_id || "";

  function setView(next: ItemView) {
    const params = new URLSearchParams(search);
    params.set("view", next);
    setSearch(params);
  }

  async function onCreateSpace(event: FormEvent<HTMLFormElement>) {
    const { dialog, save } = dialogForm(event);
    if (!save) return closeDialog(dialog);
    const data = new FormData(event.currentTarget);
    await createSpaceMutation.mutateAsync({
      name: String(data.get("name") ?? ""),
      visibility: String(data.get("visibility") ?? "private") as "private" | "members" | "tenant",
      kind: "personal",
      icon: "",
    });
    closeDialog(dialog);
  }

  async function onCreatePage(event: FormEvent<HTMLFormElement>) {
    const { dialog, save } = dialogForm(event);
    if (!save) return closeDialog(dialog);
    if (!targetSpace) {
      setNotice(t("spaces.dialog.needSpace"));
      return;
    }
    const data = new FormData(event.currentTarget);
    await createItemMutation.mutateAsync({
      spaceId: targetSpace,
      title: String(data.get("title") ?? ""),
      kind: "page",
      source: "native",
      format: "markdown",
    });
    closeDialog(dialog);
  }

  async function onRename(event: FormEvent<HTMLFormElement>) {
    const { dialog, save } = dialogForm(event);
    if (!save || !active) return closeDialog(dialog);
    const data = new FormData(event.currentTarget);
    await patchMutation.mutateAsync({ itemId: active.item_id, title: String(data.get("title") ?? "") });
    closeDialog(dialog);
  }

  async function onShare(event: FormEvent<HTMLFormElement>) {
    const { dialog, save } = dialogForm(event);
    if (!save || !active) return closeDialog(dialog);
    const data = new FormData(event.currentTarget);
    await shareMutation.mutateAsync({
      itemId: active.item_id,
      user: String(data.get("user") ?? ""),
      role: String(data.get("role") ?? "viewer") as "viewer",
    });
    closeDialog(dialog);
  }

  async function onMove(event: FormEvent<HTMLFormElement>) {
    const { dialog, save } = dialogForm(event);
    if (!save || !active) return closeDialog(dialog);
    const data = new FormData(event.currentTarget);
    await patchMutation.mutateAsync({ itemId: active.item_id, space_id: String(data.get("space") ?? "") });
    closeDialog(dialog);
  }

  async function onUpload(file: File | undefined, markdown: boolean) {
    if (!file) return;
    if (!targetSpace) {
      setNotice(t("spaces.dialog.needSpace"));
      openDialog("space-create-space");
      return;
    }
    const name = file.name.toLowerCase();
    const imported = markdown || name.endsWith(".html") || name.endsWith(".htm") || name.endsWith(".zip");
    if (imported) {
      const format = (name.endsWith(".zip") ? "zip" : name.endsWith(".html") || name.endsWith(".htm") ? "html" : "markdown") as "zip" | "html" | "markdown";
      const bytes = new Uint8Array(await file.arrayBuffer());
      let binary = "";
      for (let index = 0; index < bytes.length; index += 0x8000) {
        binary += String.fromCharCode(...bytes.subarray(index, index + 0x8000));
      }
      await importPages(targetSpace, { format, filename: file.name, content_base64: btoa(binary) });
      refresh();
      return;
    }
    await createItemMutation.mutateAsync({
      spaceId: targetSpace,
      title: file.name,
      kind: "file",
      source: "upload",
      format: "binary",
    });
  }

  return (
    <section aria-labelledby="spaces-heading">
      <h2 id="spaces-heading" className="spaces-heading">{heading}</h2>
      <div className="spaces-toolbar">
        <div className="spaces-tabs" role="tablist" aria-label={t("spaces.brand")}>
          {VIEWS.map((entry) => (
            <button key={entry} type="button" role="tab" aria-selected={view === entry} onClick={() => setView(entry)}>
              {t(`spaces.tabs.${entry}`)}
            </button>
          ))}
        </div>
        <form className="spaces-search" role="search" onSubmit={(event) => {
          event.preventDefault();
          const params = new URLSearchParams(search);
          const value = String(new FormData(event.currentTarget).get("q") ?? "");
          if (value) params.set("q", value);
          else params.delete("q");
          setSearch(params);
        }}>
          <Search size={14} aria-hidden="true" />
          <input name="q" type="search" defaultValue={q} aria-label={t("spaces.searchLabel")} placeholder={t("spaces.search")} />
        </form>
        <button type="button" className="spaces-create-pill" data-command="toggle-popover:space-create-menu" aria-haspopup="menu">
          {t("spaces.create")}
          <ChevronDown size={14} aria-hidden="true" />
        </button>
        <button type="button" className="spaces-icon-button" aria-pressed={!grid} aria-label={t("spaces.list")} onClick={() => setGrid(false)}>
          <List size={16} aria-hidden="true" />
        </button>
        <button type="button" className="spaces-icon-button" aria-pressed={grid} aria-label={t("spaces.grid")} onClick={() => setGrid(true)}>
          <LayoutGrid size={16} aria-hidden="true" />
        </button>
      </div>
      {notice ? <p role="alert">{notice}</p> : null}
      {items.isPending ? (
        <div aria-busy="true">
          <p className="sr-only">{t("spaces.preparing")}</p>
          <div className="spaces-skeleton" />
          <div className="spaces-skeleton" />
        </div>
      ) : null}
      {items.isError ? (
        <div className="spaces-state" role="alert">
          <p>{t("spaces.error")}</p>
          <button type="button" onClick={() => items.refetch()}>{t("spaces.retry")}</button>
        </div>
      ) : null}
      {!items.isPending && !items.isError && rows.length === 0 ? <p className="spaces-state">{t("spaces.empty")}</p> : null}
      {!items.isPending && !items.isError && rows.length > 0 && grid ? (
        <ul className="spaces-grid">
          {rows.map((item) => (
            <li key={item.item_id} className="spaces-card">
              <Link className="spaces-name" to={`/spaces/item/${item.item_id}`}>
                <KindIcon kind={item.kind} />
                <span className="spaces-title">{item.title}</span>
              </Link>
              <span className="spaces-muted">{sourceLabel(item, viewerId)}</span>
            </li>
          ))}
        </ul>
      ) : null}
      {!items.isPending && !items.isError && rows.length > 0 && !grid ? (
        <table className="spaces-table spaces-table--items">
          <caption className="sr-only">{t("spaces.itemCount", { count: rows.length })}</caption>
          <thead>
            <tr>
              <th scope="col">{t("spaces.columns.name")}</th>
              <th scope="col">{t("spaces.columns.source")}</th>
              <th scope="col">{t("spaces.columns.activity")}</th>
              <th scope="col"><span className="sr-only">{t("spaces.menu.actions", { title: "" })}</span></th>
            </tr>
          </thead>
          <tbody>
            {rows.map((item) => (
              <tr key={item.item_id} className="spaces-row">
                <td>
                  <Link className="spaces-name" to={`/spaces/item/${item.item_id}`}>
                    <span className="spaces-kind"><KindIcon kind={item.kind} /></span>
                    <span className="spaces-title">{item.title}</span>
                    <span className="spaces-badge">{item.space_name}</span>
                  </Link>
                </td>
                <td className="spaces-muted">{sourceLabel(item, viewerId)}</td>
                <td className="spaces-muted">
                  <time dateTime={item.last_activity_at}>{relativeTime(item.last_activity_at, Date.now(), i18n.language)}</time>
                </td>
                <td>
                  <button
                    type="button"
                    className="spaces-icon-button"
                    data-command={`toggle-popover:item-menu-${item.item_id}`}
                    aria-label={t("spaces.menu.actions", { title: item.title })}
                    aria-haspopup="menu"
                    onClick={() => setActive(item)}
                  >
                    ···
                  </button>
                  <div id={`item-menu-${item.item_id}`} popover="auto">
                    <div data-focusgroup="">
                      <button type="button" onClick={() => openDialog("space-rename")}>{t("spaces.actions.rename")}</button>
                      <button type="button" onClick={() => openDialog("space-share")}>{t("spaces.actions.share")}</button>
                      <button type="button" onClick={() => openDialog("space-move")}>{t("spaces.actions.move")}</button>
                      <button type="button" onClick={() => starMutation.mutate(item)}>{item.starred ? t("spaces.actions.unstar") : t("spaces.actions.star")}</button>
                      <button type="button" onClick={() => openDialog("space-delete")}>{t("spaces.actions.delete")}</button>
                    </div>
                  </div>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : null}

      <SpaceDialog id="space-create-space" titleId="space-create-space-title" title={t("spaces.dialog.createSpace")}>
        <form onSubmit={onCreateSpace}>
          <label>{t("spaces.field.name")}<input name="name" required maxLength={200} autoFocus /></label>
          <label>{t("spaces.field.visibility")}
            <select name="visibility" defaultValue="private">
              <option value="private">{t("spaces.visibility.private")}</option>
              <option value="members">{t("spaces.visibility.members")}</option>
              <option value="tenant">{t("spaces.visibility.tenant")}</option>
            </select>
          </label>
          <div className="spaces-dialog-actions">
            <button type="button" value="cancel" onClick={(event) => closeDialog(event.currentTarget.closest("dialog"))}>{t("spaces.cancel")}</button>
            <button type="submit" value="save">{t("spaces.save")}</button>
          </div>
        </form>
      </SpaceDialog>
      <SpaceDialog id="space-create-page" titleId="space-create-page-title" title={t("spaces.dialog.createPage")}>
        <form onSubmit={onCreatePage}>
          <label>{t("spaces.field.title")}<input name="title" required maxLength={500} autoFocus /></label>
          <div className="spaces-dialog-actions">
            <button type="button" value="cancel" onClick={(event) => closeDialog(event.currentTarget.closest("dialog"))}>{t("spaces.cancel")}</button>
            <button type="submit" value="save">{t("spaces.save")}</button>
          </div>
        </form>
      </SpaceDialog>
      <SpaceDialog id="space-rename" titleId="space-rename-title" title={t("spaces.dialog.rename")}>
        <form onSubmit={onRename}>
          <label>{t("spaces.field.title")}<input name="title" required maxLength={500} defaultValue={active?.title ?? ""} key={active?.item_id} autoFocus /></label>
          <div className="spaces-dialog-actions">
            <button type="button" value="cancel" onClick={(event) => closeDialog(event.currentTarget.closest("dialog"))}>{t("spaces.cancel")}</button>
            <button type="submit" value="save">{t("spaces.save")}</button>
          </div>
        </form>
      </SpaceDialog>
      <SpaceDialog id="space-share" titleId="space-share-title" title={t("spaces.dialog.share")}>
        <form onSubmit={onShare}>
          <label>{t("spaces.field.user")}<input name="user" required maxLength={255} autoFocus /></label>
          <label>{t("spaces.field.role")}
            <select name="role" defaultValue="viewer">
              <option value="viewer">viewer</option>
              <option value="commenter">commenter</option>
              <option value="editor">editor</option>
            </select>
          </label>
          <div className="spaces-dialog-actions">
            <button type="button" value="cancel" onClick={(event) => closeDialog(event.currentTarget.closest("dialog"))}>{t("spaces.cancel")}</button>
            <button type="submit" value="save">{t("spaces.save")}</button>
          </div>
        </form>
      </SpaceDialog>
      <SpaceDialog id="space-move" titleId="space-move-title" title={t("spaces.dialog.move")}>
        <form onSubmit={onMove}>
          <label>{t("spaces.field.space")}
            <select name="space" defaultValue={targetSpace}>
              {spaces.map((space) => <option key={space.space_id} value={space.space_id}>{space.name}</option>)}
            </select>
          </label>
          <div className="spaces-dialog-actions">
            <button type="button" value="cancel" onClick={(event) => closeDialog(event.currentTarget.closest("dialog"))}>{t("spaces.cancel")}</button>
            <button type="submit" value="save">{t("spaces.save")}</button>
          </div>
        </form>
      </SpaceDialog>
      <SpaceDialog id="space-delete" titleId="space-delete-title" title={t("spaces.dialog.delete")} light={false}>
        <p>{t("spaces.deleteConfirm", { title: active?.title ?? "" })}</p>
        <form method="dialog" className="spaces-dialog-actions">
          <button value="cancel" autoFocus>{t("spaces.cancel")}</button>
          <button type="button" onClick={() => { if (active) deleteMutation.mutate(active.item_id); closeDialog(document.getElementById("space-delete") as HTMLDialogElement | null); }}>{t("spaces.actions.delete")}</button>
        </form>
      </SpaceDialog>
      <dialog id="space-upload" aria-labelledby="space-upload-title" data-light="">
        <h2 id="space-upload-title">{t("spaces.menu.upload")}</h2>
        <input aria-label={t("spaces.menu.upload")} type="file" onChange={(event) => void onUpload(event.currentTarget.files?.[0], false)} />
        <form method="dialog" className="spaces-dialog-actions"><button value="cancel">{t("spaces.close")}</button></form>
      </dialog>
      <dialog id="space-import" aria-labelledby="space-import-title" data-light="">
        <h2 id="space-import-title">{t("spaces.menu.import")}</h2>
        <input aria-label={t("spaces.menu.import")} type="file" accept=".md,.markdown,.html,.htm,.zip,text/markdown,text/html,application/zip" onChange={(event) => void onUpload(event.currentTarget.files?.[0], true)} />
        <form method="dialog" className="spaces-dialog-actions"><button value="cancel">{t("spaces.close")}</button></form>
      </dialog>
    </section>
  );
}
