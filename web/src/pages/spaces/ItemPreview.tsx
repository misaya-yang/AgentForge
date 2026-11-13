import {
  diffRevisions,
  exportItem,
  getContent,
  getItem,
  listBacklinks,
  listRevisions,
  listTree,
  patchItem,
  putContent,
  restoreRevision,
  type SpaceItem,
} from "@/api/spaces";
import { HttpError } from "@/lib/fetchClient";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useParams } from "react-router-dom";
import { positionBetween } from "./format";
import { KindIcon } from "./ui";

const BlockEditor = lazy(() => import("./editor/BlockEditor").then((module) => ({ default: module.BlockEditor })));

function crumbs(items: SpaceItem[], itemId: string) {
  const byId = new Map(items.map((item) => [item.item_id, item]));
  const chain: SpaceItem[] = [];
  let current = byId.get(itemId);
  const seen = new Set<string>();
  while (current && !seen.has(current.item_id)) {
    seen.add(current.item_id);
    chain.unshift(current);
    current = current.parent_id ? byId.get(current.parent_id) : undefined;
  }
  return chain;
}

function conflictStatus(error: unknown) {
  return error instanceof HttpError && (error.response?.status === 412 || error.response?.status === 428);
}

export function ItemPreview() {
  const { t } = useTranslation();
  const { itemId = "" } = useParams();
  const client = useQueryClient();
  const item = useQuery({ queryKey: ["spaces", "item", itemId], queryFn: () => getItem(itemId), enabled: itemId.length > 0 });
  const content = useQuery({
    queryKey: ["spaces", "content", itemId],
    queryFn: () => getContent(itemId),
    enabled: item.isSuccess && item.data.kind === "page",
  });
  const tree = useQuery({
    queryKey: ["spaces", "tree", item.data?.space_id],
    queryFn: () => listTree(item.data?.space_id ?? ""),
    enabled: Boolean(item.data?.space_id),
  });
  const revisions = useQuery({
    queryKey: ["spaces", "revisions", itemId],
    queryFn: () => listRevisions(itemId),
    enabled: item.data?.kind === "page",
  });
  const backlinks = useQuery({
    queryKey: ["spaces", "backlinks", itemId],
    queryFn: () => listBacklinks(itemId),
    enabled: item.data?.kind === "page",
  });
  const [draft, setDraft] = useState<{ blocks: unknown; markdown: string } | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [conflict, setConflict] = useState(false);
  const [moved, setMoved] = useState("");
  const conflictRef = useRef<HTMLDialogElement>(null);
  const [status, setStatus] = useState("");
  const [from, setFrom] = useState<number | null>(null);
  const [to, setTo] = useState<number | null>(null);
  const save = useMutation({
    mutationFn: (next: { blocks: unknown; markdown: string; revision: number }) => putContent(itemId, next.revision, {
      content_format: "blocknote_json",
      blocks: next.blocks,
      markdown: next.markdown,
    }),
    onSuccess: async (saved) => {
      setConflict(false);
      setStatus(t("spaces.editor.saved"));
      setDraft(null);
      await client.invalidateQueries({ queryKey: ["spaces", "content", itemId] });
      await client.invalidateQueries({ queryKey: ["spaces", "revisions", itemId] });
      client.setQueryData(["spaces", "item", itemId], (current: SpaceItem | undefined) => current ? { ...current, current_revision: saved.revision } : current);
    },
    onError: (error) => {
      if (conflictStatus(error)) setConflict(true);
      else setAttempt((current) => (current < 8 ? current + 1 : current));
      setStatus(t("spaces.editor.saveFailed"));
    },
  });
  const diff = useQuery({
    queryKey: ["spaces", "diff", itemId, from, to],
    queryFn: () => diffRevisions(itemId, from ?? 0, to ?? 0),
    enabled: from !== null && to !== null && from !== to,
  });

  const pending = save.isPending;
  const mutate = save.mutate;
  useEffect(() => {
    if (!draft || !content.data || pending) return;
    const revision = content.data.revision;
    const timer = window.setTimeout(() => {
      mutate({ blocks: draft.blocks, markdown: draft.markdown, revision });
    }, 1500);
    return () => window.clearTimeout(timer);
  }, [draft, content.data, pending, mutate, attempt]);

  useEffect(() => {
    const dialog = conflictRef.current;
    if (!dialog) return;
    if (conflict && !dialog.open) dialog.showModal();
    if (!conflict && dialog.open) dialog.close();
  }, [conflict, item.data]);

  const trail = crumbs(tree.data ?? [], itemId);
  const siblings = (parentId: string | null) => (tree.data ?? [])
    .filter((entry) => entry.parent_id === parentId)
    .sort((left, right) => left.position.localeCompare(right.position));

  async function move(entry: SpaceItem, parentId: string | null, index: number) {
    const ordered = siblings(parentId).filter((candidate) => candidate.item_id !== entry.item_id);
    const before = ordered[index - 1]?.position ?? null;
    const after = ordered[index]?.position ?? null;
    await patchItem(entry.item_id, {
      parent_id: parentId ?? undefined,
      clear_parent: parentId === null,
      position: positionBetween(before, after),
    });
    await client.invalidateQueries({ queryKey: ["spaces", "tree", entry.space_id] });
  }

  return (
    <article className="spaces-page" aria-labelledby="space-item-title">
      <p><Link to="/spaces">{t("spaces.preview.back")}</Link></p>
      {item.isPending ? <p className="spaces-state">{t("spaces.preparing")}</p> : null}
      {item.isError ? (
        <div className="spaces-state" role="alert">
          <p>{t("spaces.preview.missing")}</p>
          <button type="button" onClick={() => item.refetch()}>{t("spaces.retry")}</button>
        </div>
      ) : null}
      {item.data ? (
        <div className="spaces-editor-shell">
          <nav className="spaces-tree" aria-label={t("spaces.editor.tree")}>
            <p className="spaces-reorder-status" aria-live="polite">{moved}</p>
            <ul>
              {(tree.data ?? []).filter((entry) => !entry.parent_id).map((entry) => (
                <TreeRow key={entry.item_id} item={entry} items={tree.data ?? []} active={itemId} onMove={move} onMoved={setMoved} />
              ))}
            </ul>
          </nav>
          <div className="spaces-editor-main">
            <nav aria-label={t("spaces.editor.breadcrumb")}>
              <ol className="spaces-crumbs">
                {trail.map((entry) => (
                  <li key={entry.item_id}>
                    <Link to={`/spaces/item/${entry.item_id}`}>{entry.title}</Link>
                  </li>
                ))}
              </ol>
            </nav>
            <h2 id="space-item-title" className="spaces-heading">
              <span className="spaces-kind"><KindIcon kind={item.data.kind} /></span>
              {item.data.title}
            </h2>
            <dialog ref={conflictRef} className="spaces-conflict" aria-labelledby="space-conflict-title">
              <h2 id="space-conflict-title">{t("spaces.editor.conflict")}</h2>
              <button type="button" onClick={() => { setConflict(false); setDraft(null); void content.refetch(); }}>{t("spaces.editor.reload")}</button>
            </dialog>
            {item.data.kind === "page" && content.data ? (
              <Suspense fallback={<p className="spaces-state">{t("spaces.editor.loading")}</p>}>
                <BlockEditor
                  key={`${itemId}:${content.data.revision}:${conflict ? "conflict" : "live"}`}
                  itemId={itemId}
                  blocks={content.data.blocks}
                  markdown={content.data.markdown}
                  onChange={(blocks, markdown) => {
                    setStatus(t("spaces.editor.saving"));
                    setDraft({ blocks, markdown });
                  }}
                />
              </Suspense>
            ) : (
              <div className="spaces-preview">{content.data?.markdown || t("spaces.preview.empty")}</div>
            )}
            <p className="spaces-save-status" aria-live="polite">{status}</p>
            {item.data.kind === "page" ? (
              <div className="spaces-export">
                <button type="button" onClick={() => void download(itemId, "markdown")}>{t("spaces.editor.exportMd")}</button>
                <button type="button" onClick={() => void download(itemId, "html")}>{t("spaces.editor.exportHtml")}</button>
              </div>
            ) : null}
          </div>
          <aside className="spaces-sidepanel" aria-label={t("spaces.editor.panel")}>
            <section>
              <h3>{t("spaces.editor.versions")}</h3>
              <ul className="spaces-versions">
                {(revisions.data ?? []).map((revision) => (
                  <li key={revision.revision}>
                    <button type="button" onClick={() => { setFrom((current) => current ?? revision.revision); setTo(revision.revision); }}>
                      {t("spaces.editor.revision", { revision: revision.revision })}
                    </button>
                    <button type="button" onClick={() => void restoreRevision(itemId, revision.revision, content.data?.revision ?? 0).then(() => client.invalidateQueries({ queryKey: ["spaces"] }))}>
                      {t("spaces.editor.restore")}
                    </button>
                  </li>
                ))}
              </ul>
              {diff.data ? <pre className="spaces-diff">{diff.data.diff}</pre> : null}
            </section>
            <section>
              <h3>{t("spaces.editor.backlinks")}</h3>
              <ul>
                {(backlinks.data ?? []).map((link) => (
                  <li key={link.item_id}><Link to={`/spaces/item/${link.item_id}`}>{link.title}</Link></li>
                ))}
              </ul>
              {(backlinks.data ?? []).length === 0 ? <p>{t("spaces.editor.noBacklinks")}</p> : null}
            </section>
          </aside>
        </div>
      ) : null}
    </article>
  );
}

function TreeRow({
  item,
  items,
  active,
  onMove,
  onMoved,
}: {
  item: SpaceItem;
  items: SpaceItem[];
  active: string;
  onMove: (item: SpaceItem, parentId: string | null, index: number) => Promise<void>;
  onMoved: (message: string) => void;
}) {
  const { t } = useTranslation();
  const children = items.filter((entry) => entry.parent_id === item.item_id).sort((left, right) => left.position.localeCompare(right.position));
  function keyboardMove(direction: -1 | 1) {
    const ordered = items.filter((entry) => entry.parent_id === item.parent_id).sort((left, right) => left.position.localeCompare(right.position));
    const index = ordered.findIndex((entry) => entry.item_id === item.item_id);
    const next = index + direction;
    if (index < 0 || next < 0 || next >= ordered.length) return;
    void onMove(item, item.parent_id, direction < 0 ? index - 1 : index + 1).then(() => {
      onMoved(t("spaces.editor.moved", { title: item.title, position: next + 1 }));
    });
  }
  return (
    <li
      onDragOver={(event) => event.preventDefault()}
      onDrop={(event) => {
        event.preventDefault();
        event.stopPropagation();
        const id = event.dataTransfer.getData("text/plain");
        const moving = items.find((entry) => entry.item_id === id);
        if (!moving || moving.item_id === item.item_id) return;
        if (event.altKey) {
          void onMove(moving, item.item_id, children.length);
          return;
        }
        const ordered = items.filter((entry) => entry.parent_id === item.parent_id).sort((left, right) => left.position.localeCompare(right.position));
        const index = ordered.findIndex((entry) => entry.item_id === item.item_id);
        void onMove(moving, item.parent_id, index < 0 ? 0 : index);
      }}
    >
      <span className="spaces-tree-row">
        <button
          type="button"
          className="spaces-drag"
          draggable
          aria-label={t("spaces.editor.reorder", { title: item.title })}
          onDragStart={(event) => event.dataTransfer.setData("text/plain", item.item_id)}
          onKeyDown={(event) => {
            if (!event.altKey || (event.key !== "ArrowUp" && event.key !== "ArrowDown")) return;
            event.preventDefault();
            keyboardMove(event.key === "ArrowUp" ? -1 : 1);
          }}
        >
          {t("spaces.editor.drag")}
        </button>
        <Link to={`/spaces/item/${item.item_id}`} aria-current={item.item_id === active ? "page" : undefined}>{item.title}</Link>
      </span>
      {children.length > 0 ? (
        <ul>
          {children.map((child) => <TreeRow key={child.item_id} item={child} items={items} active={active} onMove={onMove} onMoved={onMoved} />)}
        </ul>
      ) : null}
    </li>
  );
}

async function download(itemId: string, format: "markdown" | "html") {
  const file = await exportItem(itemId, format);
  const blob = new Blob([file.body], { type: format === "html" ? "text/html" : "text/markdown" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = file.filename;
  anchor.click();
  URL.revokeObjectURL(url);
}
