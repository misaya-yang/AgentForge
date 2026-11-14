import { useEffect, useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { api } from "@/lib/api";
import { bindShell, installOverlayFallback } from "@/pages/spaces/overlay";
import { useAuthStore } from "@/store/useAuthStore";

export const CONNECT_DIALOG_ID = "connect-apps";

const CATEGORIES = ["all", "local", "office", "communication", "design", "development", "data", "password", "more"] as const;

export interface CatalogEntry {
  id: string;
  kind: string;
  name: string;
  description: string;
  category: string;
  icon: string;
  auth_kind: string;
}

export interface CatalogConnection {
  connection_id: string;
  entry_id: string;
  owner_kind: string;
  owner_id: string;
  status: string;
}

function asEntries(value: unknown): CatalogEntry[] {
  const entries = value && typeof value === "object" && "entries" in value ? (value as { entries?: unknown }).entries : [];
  return Array.isArray(entries) ? entries.filter((entry): entry is CatalogEntry => Boolean(entry && typeof entry === "object" && "id" in entry)) : [];
}

function asConnections(value: unknown): CatalogConnection[] {
  const rows = value && typeof value === "object" && "connections" in value ? (value as { connections?: unknown }).connections : [];
  return Array.isArray(rows) ? rows.filter((row): row is CatalogConnection => Boolean(row && typeof row === "object" && "connection_id" in row)) : [];
}

function localOnline(value: unknown): boolean {
  const nodes = Array.isArray(value)
    ? value
    : value && typeof value === "object"
      ? (value as { nodes?: unknown; items?: unknown }).nodes ?? (value as { items?: unknown }).items
      : [];
  return Array.isArray(nodes) && nodes.some((node) => node && typeof node === "object" && (node as { status?: string }).status === "online");
}

const EMPTY_CONNECTIONS: CatalogConnection[] = [];

function useCatalogData() {
  const catalog = useQuery({
    queryKey: ["catalog", "entries"],
    queryFn: async () => asEntries((await api.get("/api/v1/catalog")).data),
  });
  const connections = useQuery({
    queryKey: ["catalog", "connections"],
    queryFn: async () => asConnections((await api.get("/api/v1/connections")).data),
  });
  const local = useQuery({
    queryKey: ["catalog", "local-nodes"],
    queryFn: async () => localOnline((await api.get("/api/v1/local-nodes")).data),
  });
  return { catalog, connections, localOnline: local.data === true };
}

function prepareOverlay(node: HTMLElement | null) {
  bindShell(node);
  if (node) installOverlayFallback(node);
}

export function ConnectAppsDialog() {
  const { t } = useTranslation();
  useEffect(() => {
    prepareOverlay(document.body);
  }, []);
  return (
    <dialog id={CONNECT_DIALOG_ID} data-light="" aria-labelledby="connect-apps-title" className="connect-apps-dialog">
      <header className="flex items-center justify-between gap-3">
        <h2 id="connect-apps-title" className="text-base font-semibold">{t("connect.title")}</h2>
        <button type="button" data-command={`close:${CONNECT_DIALOG_ID}`}>{t("connect.close")}</button>
      </header>
      <CatalogBoard />
      <style>{`
        .connect-apps-dialog:modal { overscroll-behavior: contain; width: min(920px, calc(100vw - 2rem)); max-height: min(820px, calc(100dvh - 2rem)); }
        .connect-apps-dialog::backdrop { overflow: hidden; overscroll-behavior: contain; background: hsl(0 0% 0% / 0.4); }
        .connect-card-deferred { content-visibility: auto; contain-intrinsic-size: auto none auto 96px; }
        .connect-spinner { width: 0.8rem; height: 0.8rem; border: 2px solid currentColor; border-right-color: transparent; border-radius: 999px; animation: connect-spin 0.7s linear infinite; display: inline-block; }
        @keyframes connect-spin { to { transform: rotate(360deg); } }
      `}</style>
    </dialog>
  );
}

export function CatalogBoard({ manage = false }: { manage?: boolean }) {
  const { t } = useTranslation();
  const { catalog, connections, localOnline } = useCatalogData();
  const [query, setQuery] = useState("");
  const [category, setCategory] = useState<(typeof CATEGORIES)[number]>("all");
  const [kind, setKind] = useState("");
  const entries = catalog.data ?? [];
  const rows = connections.data ?? EMPTY_CONNECTIONS;
  const connectedIds = useMemo(() => new Set(rows.filter((row) => row.status === "connected").map((row) => row.entry_id)), [rows]);
  const needle = query.trim().toLowerCase();
  const matches = entries.filter((entry) => {
    if (kind && entry.kind !== kind) return false;
    if (category !== "all" && entry.category !== category) return false;
    if (!needle) return true;
    return `${entry.name} ${entry.description} ${entry.kind}`.toLowerCase().includes(needle);
  });
  const recommended = category === "all" && !kind ? matches.filter((entry) => entry.category !== "local") : matches;
  const localEntries = entries.filter((entry) => entry.category === "local" && (!needle || `${entry.name} ${entry.description}`.toLowerCase().includes(needle)));
  const connected = entries.filter((entry) => connectedIds.has(entry.id));
  return (
    <div className="flex flex-col gap-4">
      <label className="flex flex-col gap-1 text-sm">
        <span className="sr-only">{t("connect.searchLabel")}</span>
        <input
          type="search"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder={t("connect.searchPlaceholder")}
          className="w-full rounded-md border border-border bg-background px-3 py-2"
        />
      </label>
      <div className="flex flex-wrap items-center gap-2">
        {CATEGORIES.map((item) => (
          <button key={item} type="button" aria-pressed={category === item && !kind} onClick={() => { setCategory(item); setKind(""); }}>
            {t(`connect.category.${item}`)}
          </button>
        ))}
      </div>
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <span>{t("connect.connectedCount", { count: connected.length })}</span>
        {connected.map((entry) => (
          <img key={entry.id} src={entry.icon} alt="" width={20} height={20} />
        ))}
        <a href="/settings/connectors?kind=bot_template" onClick={(event) => { event.preventDefault(); setKind("bot_template"); setCategory("all"); }}>{t("connect.botTemplates")}</a>
      </div>
      <section>
        <h3 className="mb-2 text-sm font-medium">{t("connect.recommended")}</h3>
        <EntryGrid entries={recommended} connections={rows} connectedIds={connectedIds} />
      </section>
      {localOnline && category === "all" && !kind && (
        <section>
          <h3 className="mb-2 text-sm font-medium">{t("connect.thisComputer")}</h3>
          <EntryGrid entries={localEntries} connections={rows} connectedIds={connectedIds} />
        </section>
      )}
      {manage && <SharedConnections entries={entries} connections={rows} />}
    </div>
  );
}

function EntryGrid({ entries, connections, connectedIds }: { entries: CatalogEntry[]; connections: CatalogConnection[]; connectedIds: Set<string> }) {
  const { t } = useTranslation();
  if (entries.length === 0) return <p>{t("connect.empty")}</p>;
  return (
    <ul className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
      {entries.map((entry, index) => (
        <li key={entry.id} className={index > 5 ? "connect-card-deferred rounded-lg border border-border p-3" : "rounded-lg border border-border p-3"}>
          <EntryCard entry={entry} connection={connections.find((row) => row.entry_id === entry.id && row.status !== "revoked")} connected={connectedIds.has(entry.id)} />
        </li>
      ))}
    </ul>
  );
}

function EntryCard({ entry, connection, connected }: { entry: CatalogEntry; connection?: CatalogConnection; connected: boolean }) {
  const { t } = useTranslation();
  const client = useQueryClient();
  const [pending, setPending] = useState(false);
  const [secret, setSecret] = useState("");
  const [askSecret, setAskSecret] = useState(false);
  const [error, setError] = useState("");
  async function finish(request: Promise<unknown>) {
    setPending(true);
    setError("");
    try {
      await request;
      await client.invalidateQueries({ queryKey: ["catalog", "connections"] });
      setAskSecret(false);
      setSecret("");
    } catch {
      setError(t("connect.failed"));
    } finally {
      setPending(false);
    }
  }
  function connect() {
    if (entry.auth_kind === "oauth") {
      void finish((async () => {
        const { data } = await api.post<{ authorization_url?: string }>("/api/v1/connections/" + entry.id + "/oauth/start", {});
        if (data.authorization_url) window.location.assign(data.authorization_url);
      })());
      return;
    }
    if (entry.auth_kind === "api_key" || entry.auth_kind === "pat") {
      setAskSecret(true);
      return;
    }
    void finish(api.post("/api/v1/connections", { entry_id: entry.id, owner_kind: "user" }));
  }
  return (
    <article className="flex h-full flex-col gap-2">
      <div className="flex items-center gap-2">
        {entry.icon ? <img src={entry.icon} alt="" width={28} height={28} /> : null}
        <strong>{entry.name}</strong>
      </div>
      <p className="text-sm text-muted-foreground">{entry.description}</p>
      {connected ? <span>{t("connect.connectedState")}</span> : (
        <button type="button" onClick={connect} disabled={pending} aria-busy={pending}>
          {pending ? <span className="connect-spinner" aria-hidden="true" /> : null}
          {pending ? t("connect.connecting") : t("connect.connect")}
        </button>
      )}
      {askSecret && !connected && (
        <form className="flex flex-col gap-2" onSubmit={(event) => {
          event.preventDefault();
          void finish(api.post("/api/v1/connections", { entry_id: entry.id, owner_kind: "user", secret }));
        }}>
          <label className="text-sm">
            {t("connect.secretLabel")}
            <input className="mt-1 w-full rounded-md border border-border px-2 py-1" type="password" autoComplete="off" name="connector-secret" value={secret} onChange={(event) => setSecret(event.target.value)} required />
          </label>
          <button type="submit" disabled={pending}>{t("connect.saveSecret")}</button>
        </form>
      )}
      {connection && connection.status !== "connected" ? <span>{connection.status}</span> : null}
      {error ? <p role="alert">{error}</p> : null}
    </article>
  );
}

function SharedConnections({ entries, connections }: { entries: CatalogEntry[]; connections: CatalogConnection[] }) {
  const { t } = useTranslation();
  const client = useQueryClient();
  const canAdmin = useAuthStore((state) => state.hasPermission("connector:admin") || state.hasPermission("admin:*"));
  const shared = connections.filter((row) => row.owner_kind === "tenant" || row.owner_kind === "space");
  async function update(connectionId: string, path: "revoke" | "policy") {
    if (path === "policy") await api.post(`/api/v1/connections/${connectionId}/policy`, { status: "disabled_by_policy" });
    else await api.post(`/api/v1/connections/${connectionId}/revoke`);
    await client.invalidateQueries({ queryKey: ["catalog", "connections"] });
  }
  return (
    <section>
      <h3 className="mb-2 text-sm font-medium">{t("connect.shared")}</h3>
      {shared.length === 0 ? <p>{t("connect.noShared")}</p> : (
        <ul className="flex flex-col gap-2">
          {shared.map((row) => {
            const entry = entries.find((item) => item.id === row.entry_id);
            return (
              <li key={row.connection_id} className="flex flex-wrap items-center gap-2 text-sm">
                <span>{entry?.name ?? row.entry_id}</span>
                <span>{row.owner_kind}</span>
                <span>{row.status}</span>
                <button type="button" onClick={() => void update(row.connection_id, "revoke")}>{t("connect.revoke")}</button>
                {canAdmin && row.status !== "disabled_by_policy" ? (
                  <button type="button" onClick={() => void update(row.connection_id, "policy")}>{t("connect.policy")}</button>
                ) : null}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
