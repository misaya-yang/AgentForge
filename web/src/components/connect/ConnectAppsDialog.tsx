import { useEffect, useId, useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as ToggleGroup from "@radix-ui/react-toggle-group";
import { Check, CircleAlert, LoaderCircle, Plug, Search, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { api } from "@/lib/api";
import { bindShell, installOverlayFallback } from "@/pages/spaces/overlay";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import "./connect-apps.css";

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
  const authority = useAuthStore(state => sdkAuthorityScope(state.user));
  const catalog = useQuery({
    queryKey: ["catalog", "entries", authority],
    queryFn: async () => asEntries((await api.get("/api/v1/catalog")).data),
  });
  const connections = useQuery({
    queryKey: ["catalog", "connections", authority],
    queryFn: async () => asConnections((await api.get("/api/v1/connections")).data),
  });
  const local = useQuery({
    queryKey: ["catalog", "local-nodes", authority],
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
    <dialog id={CONNECT_DIALOG_ID} closedby="any" data-light="" aria-labelledby="connect-apps-title" aria-describedby="connect-apps-summary" className="connect-apps-dialog">
      <header className="connect-dialog-header">
        <div>
          <h2 id="connect-apps-title">{t("connect.title")}</h2>
          <p id="connect-apps-summary">{t("connect.manageSummary")}</p>
        </div>
        <button type="button" className="forge-icon-button" data-command={`close:${CONNECT_DIALOG_ID}`} aria-label={t("connect.close")}><X size={20} aria-hidden="true" /></button>
      </header>
      <CatalogBoard />
    </dialog>
  );
}

export function CatalogBoard({ manage = false }: { manage?: boolean }) {
  const { t } = useTranslation();
  const { catalog, connections, localOnline } = useCatalogData();
  const [query, setQuery] = useState("");
  const [category, setCategory] = useState<(typeof CATEGORIES)[number]>("all");
  const [kind, setKind] = useState("");
  const searchId = useId();
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
    <div className="connect-board">
      <label className="connect-search" htmlFor={searchId}>
        <Search size={20} aria-hidden="true" />
        <span className="sr-only">{t("connect.searchLabel")}</span>
        <input
          type="search"
          id={searchId}
          // Native showModal() only honors the HTML autofocus attribute; React's autoFocus is JS-only.
          ref={(node) => { if (node && !manage) node.setAttribute("autofocus", ""); }}
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder={t("connect.searchPlaceholder")}
        />
      </label>
      <ToggleGroup.Root type="single" className="connect-categories" aria-label={t("connect.categories")} value={kind ? "" : category} onValueChange={(value) => { if (value) { setCategory(value as typeof category); setKind(""); } }}>
        {CATEGORIES.map((item) => (
          <ToggleGroup.Item key={item} value={item} className="connect-category">
            {t(`connect.category.${item}`)}
          </ToggleGroup.Item>
        ))}
      </ToggleGroup.Root>
      <div className="connect-status-bar">
        <span className="connect-connected-count"><Check size={16} aria-hidden="true" />{t("connect.connectedCount", { count: connected.length })}</span>
        <span className="connect-connected-icons">{connected.slice(0, 8).map((entry) => (
          <img key={entry.id} src={entry.icon} alt={entry.name} width={24} height={24} />
        ))}</span>
        <a href="/settings/connectors?kind=bot_template" onClick={(event) => { event.preventDefault(); setKind("bot_template"); setCategory("all"); }}>{t("connect.botTemplates")}</a>
      </div>
      <section className="connect-section" aria-busy={catalog.isPending || connections.isPending}>
        <h3>{t("connect.recommended")}</h3>
        {catalog.isError || connections.isError ? (
          <div className="forge-state forge-error" role="alert"><CircleAlert aria-hidden="true" /><p>{t("connect.loadFailed")}</p><button type="button" className="forge-button" onClick={() => { void catalog.refetch(); void connections.refetch(); }}>{t("connect.retry")}</button></div>
        ) : catalog.isPending || connections.isPending ? (
          <div className="connect-grid" aria-label={t("connect.loading")} role="progressbar">
            {Array.from({ length: 6 }, (_, index) => <div key={index} className="connect-skeleton" aria-hidden="true"><span /><span /><span /></div>)}
          </div>
        ) : <EntryGrid entries={recommended} connections={rows} connectedIds={connectedIds} />}
      </section>
      {localOnline && category === "all" && !kind && (
        <section className="connect-section">
          <h3>{t("connect.thisComputer")}</h3>
          <EntryGrid entries={localEntries} connections={rows} connectedIds={connectedIds} />
        </section>
      )}
      {manage && <SharedConnections entries={entries} connections={rows} />}
    </div>
  );
}

function EntryGrid({ entries, connections, connectedIds }: { entries: CatalogEntry[]; connections: CatalogConnection[]; connectedIds: Set<string> }) {
  const { t } = useTranslation();
  if (entries.length === 0) return <div className="forge-state" role="status"><Plug size={32} aria-hidden="true" /><p>{t("connect.empty")}</p></div>;
  return (
    <ul className="connect-grid" role="list">
      {entries.map((entry, index) => (
        <li key={entry.id} className={`connect-card${index > 5 ? " connect-card-deferred" : ""}`}>
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
  async function finish(request: () => Promise<unknown>) {
    setPending(true);
    setError("");
    try {
      await request();
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
      void finish(async () => {
        const { data } = await api.post<{ authorization_url?: string }>("/api/v1/connections/" + entry.id + "/oauth/start", {});
        if (data.authorization_url) window.location.assign(data.authorization_url);
      });
      return;
    }
    if (entry.auth_kind === "api_key" || entry.auth_kind === "pat") {
      setAskSecret(true);
      return;
    }
    void finish(() => api.post("/api/v1/connections", { entry_id: entry.id, owner_kind: "user" }));
  }
  return (
    <article className="connect-card-content">
      <div className="connect-card-heading">
        <span className="connect-app-icon">{entry.icon ? <img src={entry.icon} alt="" width={28} height={28} loading="lazy" /> : <Plug size={24} aria-hidden="true" />}</span>
        <strong>{entry.name}</strong>
      </div>
      <p className="connect-description" title={entry.description}>{entry.description}</p>
      {connected ? <button type="button" className="forge-button connect-is-connected" disabled><Check size={16} aria-hidden="true" />{t("connect.connectedState")}</button> : (
        <button type="button" className="forge-button" onClick={connect} disabled={pending || connection?.status === "disabled_by_policy"} aria-busy={pending} aria-label={`${pending ? t("connect.connecting") : t("connect.connect")} ${entry.name}`}>
          {pending ? <LoaderCircle size={16} className="connect-spinner" aria-hidden="true" /> : null}
          {pending ? t("connect.connecting") : t("connect.connect")}
        </button>
      )}
      {askSecret && !connected && (
        <form className="connect-secret-form" onSubmit={(event) => {
          event.preventDefault();
          void finish(() => api.post("/api/v1/connections", { entry_id: entry.id, owner_kind: "user", secret }));
        }}>
          <label className="text-sm">
            {t("connect.secretLabel")}
            <input className="forge-input" type="password" autoComplete="off" name="connector-secret" value={secret} onChange={(event) => setSecret(event.target.value)} required />
          </label>
          <button type="submit" className="forge-button forge-button-primary" disabled={pending}>{t("connect.saveSecret")}</button>
        </form>
      )}
      {connection && !["connected", "revoked"].includes(connection.status) ? <span className="connect-connection-state">{t(`connect.status.${connection.status}`, { defaultValue: connection.status })}</span> : null}
      {error ? <p className="forge-error" role="alert">{error}</p> : null}
    </article>
  );
}

function SharedConnections({ entries, connections }: { entries: CatalogEntry[]; connections: CatalogConnection[] }) {
  const { t } = useTranslation();
  const client = useQueryClient();
  const canAdmin = useAuthStore((state) => state.hasPermission("connector:admin") || state.hasPermission("admin:*"));
  const shared = connections.filter((row) => row.owner_kind === "tenant" || row.owner_kind === "space");
  const [pending, setPending] = useState("");
  const [error, setError] = useState("");
  async function update(connectionId: string, path: "revoke" | "policy") {
    if (pending) return;
    setPending(connectionId); setError("");
    try {
      if (path === "policy") await api.post(`/api/v1/connections/${connectionId}/policy`, { status: "disabled_by_policy" });
      else await api.post(`/api/v1/connections/${connectionId}/revoke`);
      await client.invalidateQueries({ queryKey: ["catalog", "connections"] });
    } catch { setError(t("connect.failed")); }
    finally { setPending(""); }
  }
  return (
    <section className="connect-section">
      <h3>{t("connect.shared")}</h3>
      {error && <p className="forge-error" role="alert">{error}</p>}
      {shared.length === 0 ? <p>{t("connect.noShared")}</p> : (
        <ul className="flex flex-col gap-2">
          {shared.map((row) => {
            const entry = entries.find((item) => item.id === row.entry_id);
            return (
              <li key={row.connection_id} className="connect-shared-row">
                <span>{entry?.name ?? row.entry_id}</span>
                <span>{row.owner_kind}</span>
                <span>{row.status}</span>
                <button className="forge-button" type="button" disabled={Boolean(pending)} onClick={() => void update(row.connection_id, "revoke")}>{t("connect.revoke")}</button>
                {canAdmin && row.status !== "disabled_by_policy" ? (
                  <button className="forge-button" type="button" disabled={Boolean(pending)} onClick={() => void update(row.connection_id, "policy")}>{t("connect.policy")}</button>
                ) : null}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
