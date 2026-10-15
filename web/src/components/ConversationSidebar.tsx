/**
 * Conversation Sidebar — GPT-style session list with rename + folder grouping.
 *
 * Features:
 * - Date-based grouping (Today / Yesterday / Last 7 days / Older)
 * - Folder/Project grouping (sessions with metadata.folder)
 * - Inline rename (double-click title)
 * - Context menu (rename / move to folder / delete)
 * - Search across all sessions
 */

import { useCallback, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { motion } from "framer-motion";
import { cn } from "@/lib/utils";
import { CustomizeDialog } from "@/pages/assistant/components/CustomizeDialog";
import {
  FolderOpen,
  MessageSquare,
  Pencil,
  Plus,
  Search,
  Settings,
  Trash2,
  X,
  Loader2,
} from "lucide-react";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import type { SessionSummary } from "@/api/sessions";
import { updateSession } from "@/api/sessions";

// ── Folder Input Dialog ──────────────────────────────────────────────

function FolderDialog({
  isOpen, onClose, onConfirm, existingFolders,
}: {
  isOpen: boolean;
  onClose: () => void;
  onConfirm: (folder: string) => void;
  existingFolders: string[];
}) {
  const { t } = useTranslation();
  const [value, setValue] = useState("");

  function handleClose(): void {
    setValue("");
    onClose();
  }

  function handleConfirm(): void {
    const trimmed = value.trim();
    if (trimmed) {
      onConfirm(trimmed);
      handleClose();
    }
  }

  return (
    <Dialog open={isOpen} onOpenChange={(open) => { if (!open) handleClose(); }}>
      <DialogContent className="max-w-sm rounded-2xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2 text-base">
            <FolderOpen className="h-4 w-4 text-primary" />
            {t("assistant.moveToFolder", "Move to folder")}
          </DialogTitle>
          <DialogDescription>
            {t("assistant.folderHint", "Enter a folder name or select an existing one")}
          </DialogDescription>
        </DialogHeader>

        <div>
          <input
            value={value}
            onChange={(e) => setValue(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") handleConfirm();
              if (e.key === "Escape") handleClose();
            }}
            placeholder={t("assistant.folderPlaceholder", "e.g. Product Research")}
            className="w-full rounded-xl border border-border bg-background px-3.5 py-2.5 text-sm outline-hidden focus:border-primary focus:ring-2 focus:ring-primary/20 transition-[border-color,box-shadow] duration-150"
            autoFocus
          />
        </div>

        {existingFolders.length > 0 && (
          <div className="flex flex-wrap gap-1.5">
            {existingFolders.map((f) => (
              <button
                key={f}
                type="button"
                className={cn(
                  "px-2.5 py-1 rounded-lg text-xs transition-[background-color,color] duration-150",
                  value === f
                    ? "bg-primary text-primary-foreground"
                    : "bg-muted/50 text-muted-foreground hover:bg-muted hover:text-foreground"
                )}
                onClick={() => setValue(f)}
              >
                <FolderOpen className="h-3 w-3 inline mr-1" />{f}
              </button>
            ))}
          </div>
        )}

        <DialogFooter>
          <button
            type="button"
            onClick={handleClose}
            className="px-4 py-2 rounded-xl text-sm text-muted-foreground hover:bg-muted transition-colors"
          >
            {t("common.cancel", "Cancel")}
          </button>
          <button
            type="button"
            onClick={handleConfirm}
            disabled={!value.trim()}
            className="px-4 py-2 rounded-xl text-sm font-medium bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-40 transition-colors"
          >
            {t("common.confirm", "Move")}
          </button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// ── Types ────────────────────────────────────────────────────────────

interface ConversationSidebarProps {
  sessions: SessionSummary[];
  activeSessionId: string | null;
  isLoading?: boolean;
  onNewChat: () => void;
  onSelectSession: (sessionId: string) => void;
  onDeleteSession: (sessionId: string) => Promise<boolean>;
  onSessionsChange?: (sessions: SessionSummary[]) => void;
}

// ── Helpers ──────────────────────────────────────────────────────────

function getSessionTitle(session: SessionSummary, fallback: string): string {
  const meta = session.metadata as Record<string, unknown> | undefined;
  if (meta?.title && typeof meta.title === "string") return meta.title;
  return `${fallback} ${session.session_id.slice(0, 8)}...`;
}

function getSessionFolder(session: SessionSummary): string | null {
  const meta = session.metadata as Record<string, unknown> | undefined;
  return (meta?.folder as string) || null;
}

interface GroupedSessions {
  folders: Record<string, SessionSummary[]>;
  today: SessionSummary[];
  yesterday: SessionSummary[];
  lastWeek: SessionSummary[];
  older: SessionSummary[];
}

function groupSessions(sessions: SessionSummary[]): GroupedSessions {
  const groups: GroupedSessions = { folders: {}, today: [], yesterday: [], lastWeek: [], older: [] };
  if (!Array.isArray(sessions)) return groups;

  const now = new Date();
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const yesterday = new Date(today); yesterday.setDate(yesterday.getDate() - 1);
  const lastWeek = new Date(today); lastWeek.setDate(lastWeek.getDate() - 7);

  for (const session of sessions) {
    const folder = getSessionFolder(session);
    if (folder) {
      (groups.folders[folder] ??= []).push(session);
      continue;
    }
    const d = new Date(session.created_at || session.updated_at || 0);
    if (d >= today) groups.today.push(session);
    else if (d >= yesterday) groups.yesterday.push(session);
    else if (d >= lastWeek) groups.lastWeek.push(session);
    else groups.older.push(session);
  }

  const byDate = (a: SessionSummary, b: SessionSummary) =>
    new Date(b.created_at || b.updated_at || 0).getTime() - new Date(a.created_at || a.updated_at || 0).getTime();
  groups.today.sort(byDate);
  groups.yesterday.sort(byDate);
  groups.lastWeek.sort(byDate);
  groups.older.sort(byDate);
  Object.values(groups.folders).forEach((arr) => arr.sort(byDate));

  return groups;
}

// ── Session Item (with inline rename) ────────────────────────────────

function SessionItem({
  session, isActive, onSelect, onDelete, onRename, onMoveToFolder, onOpenFolderDialog, fallbackLabel,
}: {
  session: SessionSummary;
  isActive: boolean;
  onSelect: () => void;
  onDelete: () => void;
  onRename: (newTitle: string) => void;
  onMoveToFolder: (folder: string | null) => void;
  onOpenFolderDialog: () => void;
  fallbackLabel: string;
}) {
  const { t } = useTranslation();
  const [isEditing, setIsEditing] = useState(false);
  const [editValue, setEditValue] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);
  const title = getSessionTitle(session, fallbackLabel);

  function startEdit(): void {
    setEditValue(title);
    setIsEditing(true);
    setTimeout(() => inputRef.current?.select(), 50);
  }

  function commitEdit(): void {
    setIsEditing(false);
    const trimmed = editValue.trim();
    if (trimmed && trimmed !== title) onRename(trimmed);
  }

  const hasFolder = Boolean(getSessionFolder(session));

  return (
    <motion.div layout className="group relative">
      {isEditing ? (
        <div className="px-2 py-1">
          <input
            ref={inputRef}
            value={editValue}
            onChange={(e) => setEditValue(e.target.value)}
            onBlur={commitEdit}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitEdit();
              if (e.key === "Escape") setIsEditing(false);
            }}
            className="w-full rounded-md border border-primary/40 bg-background px-2 py-1.5 text-sm outline-hidden focus:border-primary"
            autoFocus
          />
        </div>
      ) : (
        <div className="flex items-center">
          <button
            type="button"
            className={cn(
              // GPT-style: generous corner radius (8px), modest padding,
              // a tighter color scale between rest / hover / active.
              "flex w-full items-center rounded-lg px-3 py-2 pr-14 text-left transition-colors duration-150",
              isActive
                // Active: soft surface + primary text, no border. The state
                // reads as "selected" without shouting.
                ? "bg-[hsl(var(--assistant-surface-soft))] text-[hsl(var(--assistant-text-primary))]"
                // Rest: secondary text on transparent. Hover lightens
                // surface and promotes text to primary in one step — no
                // intermediate "warming up" color shift.
                : "text-[hsl(var(--assistant-text-secondary))] hover:bg-[hsl(var(--assistant-surface-soft))]/70 hover:text-[hsl(var(--assistant-text-primary))]"
            )}
            onClick={onSelect}
            onDoubleClick={startEdit}
          >
            <span className="flex-1 truncate text-[13.5px] leading-5">{title}</span>
          </button>

          {/* Action buttons — visible on row hover. No background plate
              (GPT shows raw icons that get a subtle bg per icon on its
              own hover). The plate makes the row look busier than it
              is. */}
          <div className="absolute right-1.5 top-1/2 flex -translate-y-1/2 items-center gap-0.5 opacity-70 transition-opacity sm:opacity-0 sm:group-focus-within:opacity-100 sm:group-hover:opacity-100">
            <button type="button" onClick={(e) => { e.stopPropagation(); startEdit(); }} className="hidden rounded-md p-1.5 text-[hsl(var(--assistant-text-secondary))] hover:bg-[hsl(var(--assistant-surface-soft))] hover:text-[hsl(var(--assistant-text-primary))] sm:inline-flex" title={t("assistant.rename", "Rename")} aria-label={`${t("assistant.rename", "Rename")}: ${title}`}>
              <Pencil className="h-3.5 w-3.5" />
            </button>

            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <button type="button" className="rounded-md p-1.5 text-[hsl(var(--assistant-text-secondary))] hover:bg-[hsl(var(--assistant-surface-soft))] hover:text-[hsl(var(--assistant-text-primary))]" title={t("assistant.more", "More")} aria-label={`${t("assistant.more", "More")}: ${title}`}>
                  <span className="text-xs font-bold leading-none">···</span>
                </button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="w-48">
                <DropdownMenuItem onClick={startEdit}>
                  <Pencil className="h-3.5 w-3.5 mr-2" /> {t("assistant.rename", "Rename")}
                </DropdownMenuItem>
                <DropdownMenuItem onClick={onOpenFolderDialog}>
                  <FolderOpen className="h-3.5 w-3.5 mr-2" /> {t("assistant.moveToFolder", "Move to folder")}
                </DropdownMenuItem>
                {hasFolder && (
                  <DropdownMenuItem onClick={() => onMoveToFolder(null)}>
                    <X className="h-3.5 w-3.5 mr-2" /> {t("assistant.removeFromFolder", "Remove from folder")}
                  </DropdownMenuItem>
                )}
                <DropdownMenuSeparator />
                <DropdownMenuItem className="text-destructive focus:text-destructive focus:bg-destructive/10" onClick={onDelete}>
                  <Trash2 className="h-3.5 w-3.5 mr-2" /> {t("assistant.delete", "Delete")}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>

            <button type="button" onClick={(e) => { e.stopPropagation(); onDelete(); }} className="hidden rounded-md p-1.5 text-[hsl(var(--assistant-text-secondary))] hover:bg-destructive/10 hover:text-destructive sm:inline-flex" title={t("assistant.delete", "Delete")} aria-label={`${t("assistant.delete", "Delete")}: ${title}`}>
              <Trash2 className="h-3.5 w-3.5" />
            </button>
          </div>
        </div>
      )}
    </motion.div>
  );
}

// ── Section Group ────────────────────────────────────────────────────

function SectionGroup({
  label, icon, sessions, activeSessionId, onSelectSession, onDeleteSession, onRename, onMoveToFolder, onOpenFolderDialog, fallbackLabel, collapsible = false,
}: {
  label: string;
  icon?: React.ReactNode;
  sessions: SessionSummary[];
  activeSessionId: string | null;
  onSelectSession: (id: string) => void;
  onDeleteSession: (id: string) => void;
  onRename: (id: string, title: string) => void;
  onMoveToFolder: (id: string, folder: string | null) => void;
  onOpenFolderDialog: (id: string) => void;
  fallbackLabel: string;
  collapsible?: boolean;
}) {
  // Folders (collapsible groups) default to CLOSED so the projects
  // row stays compact at a glance — users click the folder to open
  // its sessions. Date groups (non-collapsible) stay open.
  const [collapsed, setCollapsed] = useState(collapsible === true);
  if (sessions.length === 0) return null;

  return (
    // GPT cadence: generous breathing room ABOVE each section heading
    // (mt-4) creates a clear visual break between date groups; tight
    // gap below (the heading sits ~6px from the first item) keeps the
    // group reading as one chunk.
    <div className="mt-4 first:mt-2">
      <button
        type="button"
        className="flex w-full items-center gap-1.5 px-3 pb-1.5 text-[11px] font-medium text-[hsl(var(--assistant-text-tertiary))] hover:text-[hsl(var(--assistant-text-secondary))]"
        onClick={() => collapsible && setCollapsed(!collapsed)}
      >
        {icon}
        <span className="flex-1 text-left">{label}</span>
        {collapsible && <span className="text-[10px]">{collapsed ? "▸" : "▾"}</span>}
      </button>
      {!collapsed && (
        // Items butt up against each other with no visible gap — GPT's
        // sidebar reads as a single tight cluster per date group.
        <div>
          {sessions.map((s) => (
            <SessionItem
              key={s.session_id}
              session={s}
              isActive={s.session_id === activeSessionId}
              onSelect={() => onSelectSession(s.session_id)}
              onDelete={() => onDeleteSession(s.session_id)}
              onRename={(title) => onRename(s.session_id, title)}
              onMoveToFolder={(folder) => onMoveToFolder(s.session_id, folder)}
              onOpenFolderDialog={() => onOpenFolderDialog(s.session_id)}
              fallbackLabel={fallbackLabel}
            />
          ))}
        </div>
      )}
    </div>
  );
}

// ── Main Component ───────────────────────────────────────────────────

export function ConversationSidebar({
  sessions, activeSessionId, isLoading = false,
  onNewChat, onSelectSession, onDeleteSession, onSessionsChange,
}: ConversationSidebarProps) {
  const { t } = useTranslation();
  const [searchQuery, setSearchQuery] = useState("");
  const [showCustomize, setShowCustomize] = useState(false);
  const fallback = t("assistant.chatFallback", "Chat");

  const filtered = useMemo(() => {
    if (!Array.isArray(sessions)) return [];
    if (!searchQuery.trim()) return sessions;
    const q = searchQuery.toLowerCase();
    return sessions.filter((s) => {
      const title = getSessionTitle(s, fallback).toLowerCase();
      const folder = getSessionFolder(s)?.toLowerCase() || "";
      return title.includes(q) || folder.includes(q);
    });
  }, [sessions, searchQuery, fallback]);

  const grouped = useMemo(() => groupSessions(filtered), [filtered]);
  const folderNames = Object.keys(grouped.folders).sort();

  // Collect folders from ALL sessions (not just filtered) for the dialog
  const allFolderNames = useMemo(() => {
    const folders = new Set<string>();
    for (const s of sessions) {
      const f = getSessionFolder(s);
      if (f) folders.add(f);
    }
    return Array.from(folders).sort();
  }, [sessions]);

  const hasNoSessions = !isLoading && filtered.length === 0;

  // Folder dialog state
  const [folderDialogOpen, setFolderDialogOpen] = useState(false);
  const [folderDialogTarget, setFolderDialogTarget] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<SessionSummary | null>(null);
  const [deleteError, setDeleteError] = useState("");
  const [deleting, setDeleting] = useState(false);

  function openFolderDialog(sessionId: string): void {
    setFolderDialogTarget(sessionId);
    setFolderDialogOpen(true);
  }

  const handleRename = useCallback(async (sessionId: string, newTitle: string) => {
    try {
      await updateSession(sessionId, { metadata: { title: newTitle } });
      onSessionsChange?.(sessions.map((s) =>
        s.session_id === sessionId
          ? { ...s, metadata: { ...(s.metadata as Record<string, unknown> || {}), title: newTitle } }
          : s
      ));
    } catch (e) { console.error("Rename failed:", e); }
  }, [sessions, onSessionsChange]);

  const handleMoveToFolder = useCallback(async (sessionId: string, folder: string | null) => {
    try {
      const meta = folder ? { folder } : { folder: null };
      await updateSession(sessionId, { metadata: meta });
      onSessionsChange?.(sessions.map((s) =>
        s.session_id === sessionId
          ? { ...s, metadata: { ...(s.metadata as Record<string, unknown> || {}), folder: folder ?? undefined } }
          : s
      ));
    } catch (e) { console.error("Move failed:", e); }
  }, [sessions, onSessionsChange]);

  const requestDelete = (sessionId: string) => {
    const session = sessions.find((item) => item.session_id === sessionId);
    if (!session) return;
    setDeleteError("");
    setDeleteTarget(session);
  };

  const closeDelete = () => {
    if (deleting) return;
    setDeleteTarget(null);
    setDeleteError("");
  };

  const confirmDelete = async () => {
    if (!deleteTarget || deleting) return;
    setDeleting(true);
    setDeleteError("");
    try {
      if (await onDeleteSession(deleteTarget.session_id)) {
        setDeleteTarget(null);
      } else {
        setDeleteError(t("assistant.deleteSessionFailed", "Could not delete this conversation. Try again."));
      }
    } finally {
      setDeleting(false);
    }
  };

  const commonProps = {
    activeSessionId, onSelectSession, onDeleteSession: requestDelete,
    onRename: handleRename, onMoveToFolder: handleMoveToFolder,
    onOpenFolderDialog: openFolderDialog, fallbackLabel: fallback,
  };

  return (
    <div className="flex flex-col h-full">
      {/* New Chat — restrained: ghost-style row with icon + label.
          Not a heavy filled pill; at rest reads as a nav item, hover
          reveals a soft surface fill. */}
      <div className="px-2 pt-3 pb-1">
        <button
          type="button"
          onClick={onNewChat}
          className="w-full flex items-center gap-2.5 rounded-lg px-3 py-2 text-[13.5px] font-medium text-[hsl(var(--assistant-text-primary))] hover:bg-[hsl(var(--assistant-surface-soft))] transition-colors duration-150"
        >
          <Plus className="h-[18px] w-[18px] text-[hsl(var(--assistant-text-secondary))]" />
          <span>{t("assistant.newChat")}</span>
        </button>
      </div>

      {/* Search */}
      <div className="p-3">
        <div className="relative">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
          <Input
            placeholder={t("assistant.searchConversations")}
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="pl-8 pr-8 h-9 text-sm"
          />
          {searchQuery && (
            <button type="button" className="absolute right-2 top-1/2 -translate-y-1/2 rounded p-1 hover:bg-muted" onClick={() => setSearchQuery("")} aria-label={`${t("common.clear")} ${t("common.search")}`}>
              <X className="h-3.5 w-3.5 text-muted-foreground" />
            </button>
          )}
        </div>
      </div>

      {/* Session List */}
      <div className="flex-1 overflow-y-auto px-2">
        {isLoading ? (
          <div className="flex items-center justify-center py-8">
            <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
          </div>
        ) : hasNoSessions ? (
          <div className="flex flex-col items-center justify-center py-12 px-4 text-center">
            <MessageSquare className="h-10 w-10 text-muted-foreground/40 mb-3" />
            <p className="text-sm text-muted-foreground">{t("assistant.noHistory")}</p>
          </div>
        ) : (
          <>
            {/* Folders (Projects) — zero margin; the eyebrow + folders
                + date groups all chain directly, no visible gaps at
                the section boundaries. */}
            {folderNames.length > 0 && (
              <div className="mt-2">
                <div className="px-3 pb-1 text-[11px] font-medium text-[hsl(var(--assistant-text-tertiary))]">
                  {t("assistant.projects", "Projects")}
                </div>
                {folderNames.map((folder) => (
                  <SectionGroup
                    key={`folder-${folder}`}
                    label={folder}
                    icon={<FolderOpen className="h-3 w-3" />}
                    sessions={grouped.folders[folder]}
                    collapsible
                    {...commonProps}
                  />
                ))}
              </div>
            )}

            {/* Date groups */}
            <SectionGroup label={t("assistant.today")} sessions={grouped.today} {...commonProps} />
            <SectionGroup label={t("assistant.yesterday")} sessions={grouped.yesterday} {...commonProps} />
            <SectionGroup label={t("assistant.lastWeek")} sessions={grouped.lastWeek} {...commonProps} />
            <SectionGroup label={t("assistant.older")} sessions={grouped.older} {...commonProps} />
          </>
        )}
      </div>

      {/* Skills & MCP Config */}
      <div className="px-2 py-2 border-t border-[hsl(var(--assistant-border-soft))] shrink-0">
        <button
          type="button"
          onClick={() => setShowCustomize(true)}
          className="flex w-full items-center gap-2.5 rounded-lg px-3 py-2 text-[13px] text-[hsl(var(--assistant-text-secondary))] hover:bg-[hsl(var(--assistant-surface-soft))] hover:text-[hsl(var(--assistant-text-primary))] transition-colors duration-150"
        >
          <Settings className="h-[15px] w-[15px]" />
          {t("assistant.customize", "Skills & MCP")}
        </button>
      </div>
      <CustomizeDialog open={showCustomize} onClose={() => setShowCustomize(false)} />

      {/* Folder move dialog */}
      <FolderDialog
        isOpen={folderDialogOpen}
        onClose={() => { setFolderDialogOpen(false); setFolderDialogTarget(null); }}
        onConfirm={(folder) => {
          if (folderDialogTarget) handleMoveToFolder(folderDialogTarget, folder);
        }}
        existingFolders={allFolderNames}
      />
      <Dialog open={deleteTarget !== null} onOpenChange={(open) => { if (!open) closeDelete(); }}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("assistant.deleteSessionTitle", "Delete this conversation?")}</DialogTitle>
            <DialogDescription>
              {t(
                "assistant.deleteSessionImpact",
                "This conversation will leave your history and cannot be reopened here. Existing share links remain active until you revoke them separately; downloaded copies are unaffected.",
              )}
            </DialogDescription>
          </DialogHeader>
          <p className="break-words text-sm font-medium">{deleteTarget && getSessionTitle(deleteTarget, fallback)}</p>
          {deleteError && <p role="alert" className="text-sm text-red-600">{deleteError}</p>}
          <DialogFooter>
            <button type="button" disabled={deleting} onClick={closeDelete} className="rounded-md border px-4 py-2 text-sm">
              {t("common.cancel", "Cancel")}
            </button>
            <button type="button" disabled={deleting} onClick={() => void confirmDelete()} className="rounded-md bg-destructive px-4 py-2 text-sm text-destructive-foreground disabled:opacity-50">
              {t("assistant.delete", "Delete")}
            </button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
