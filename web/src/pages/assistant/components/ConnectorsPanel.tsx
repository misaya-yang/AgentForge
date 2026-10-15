/**
 * ConnectorsPanel — quick access to third-party data connectors from Chat.
 *
 * Backed by the connector stack (src/api/v1/connectors.py):
 * - GET  /api/v1/connectors/available          — catalog + connection status
 * - GET  /api/v1/connectors/auth/{provider}    — start OAuth (returns auth_url)
 * - POST /api/v1/connectors/{provider}/activate — expose MCP tools
 * - GET  /api/v1/connectors/{provider}/mcp-status
 * - DELETE /api/v1/connectors/{provider}       — disconnect
 *
 * Personal management lives at /connections; provider configuration stays in settings.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { useTranslation } from "react-i18next";
import { Plug } from "lucide-react";
import { api } from "@/lib/api";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";

interface ConnectorsPanelProps {
  open: boolean;
  onClose: () => void;
  onCountChange?: (count: number) => void;
  isCurrent?: () => boolean;
  authorizationReturnFailed?: boolean;
}

interface ConnectorInfo {
  provider: string;
  display_name: string;
  description?: string | null;
  icon_url?: string | null;
  enabled: boolean;
  connected: boolean;
  status?: string | null;
}

interface ConnectorMcpStatus {
  mcp_active?: boolean;
  tools?: Array<{ name: string; description: string }>;
}

interface ConnectorActivateResponse extends ConnectorMcpStatus {
  tool_count?: number;
}

function apiErrorMessage(error: unknown, fallback: string): string {
  if (error && typeof error === "object") {
    const maybe = error as {
      response?: { data?: { detail?: unknown } };
      message?: unknown;
    };
    const detail = maybe.response?.data?.detail;
    if (typeof detail === "string" && detail) return detail;
    if (typeof maybe.message === "string" && maybe.message) return maybe.message;
  }
  return fallback;
}

const alwaysCurrent = () => true;

export default function ConnectorsPanel(props: ConnectorsPanelProps) {
  const scope = useAuthenticatedReadScope();
  return scope.isCurrent() ? <ConnectorsPanelContent key={scope.id} {...props} authorizationScope={scope.isCurrent} /> : null;
}

function ConnectorsPanelContent({ open, onClose, onCountChange, isCurrent: parentCurrent = alwaysCurrent, authorizationReturnFailed = false, authorizationScope }: ConnectorsPanelProps & { authorizationScope: () => boolean }) {
  const isCurrent = useCallback(() => authorizationScope() && parentCurrent(), [authorizationScope, parentCurrent]);
  const { t } = useTranslation();
  const [connectors, setConnectors] = useState<ConnectorInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadFailed, setLoadFailed] = useState(false);
  const readGeneration = useRef(0);
  const activeRead = useRef<number | null>(null);
  const [mcpByProvider, setMcpByProvider] = useState<Record<string, ConnectorMcpStatus>>({});
  const [mcpReadStatus, setMcpReadStatus] = useState<Record<string, "loading" | "ready" | "failed">>({});
  const activeMcpReads = useRef(new Map<string, number>());
  const [busyProvider, setBusyProvider] = useState<string | null>(null);
  const activeMutation = useRef<symbol | null>(null);
  const [error, setError] = useState("");

  const loadMcpStatus = useCallback(async (provider: string, generation: number) => {
    if (!isCurrent() || generation !== readGeneration.current || activeMcpReads.current.get(provider) === generation) return;
    activeMcpReads.current.set(provider, generation);
    setMcpReadStatus(prev => ({ ...prev, [provider]: "loading" }));
    try {
      const { data } = await api.get<ConnectorMcpStatus>(`/api/v1/connectors/${provider}/mcp-status`);
      if (!isCurrent() || generation !== readGeneration.current) return;
      if (typeof data?.mcp_active !== "boolean") throw new Error("Invalid MCP status observation");
      setMcpByProvider(prev => ({ ...prev, [provider]: data }));
      setMcpReadStatus(prev => ({ ...prev, [provider]: "ready" }));
    } catch {
      if (isCurrent() && generation === readGeneration.current) setMcpReadStatus(prev => ({ ...prev, [provider]: "failed" }));
    } finally {
      if (activeMcpReads.current.get(provider) === generation) activeMcpReads.current.delete(provider);
    }
  }, [isCurrent]);

  const loadConnectors = useCallback(async () => {
    if (!isCurrent() || activeRead.current !== null) return;
    const generation = ++readGeneration.current;
    activeRead.current = generation;
    setLoading(true);
    try {
      const { data } = await api.get<ConnectorInfo[]>("/api/v1/connectors/available");
      if (!isCurrent() || generation !== readGeneration.current) return;
      setLoadFailed(false);
      setConnectors(data);
      setMcpReadStatus(Object.fromEntries(data.filter(conn => conn.connected).map(conn => [conn.provider, "loading"])));
      onCountChange?.(data.filter((c) => c.connected).length);
      for (const c of data.filter((x) => x.connected)) {
        if (!isCurrent() || generation !== readGeneration.current) return;
        await loadMcpStatus(c.provider, generation);
      }
    } catch {
      if (isCurrent() && generation === readGeneration.current) setLoadFailed(true);
    } finally {
      if (activeRead.current === generation) activeRead.current = null;
      if (isCurrent() && generation === readGeneration.current) setLoading(false);
    }
  }, [onCountChange, loadMcpStatus, isCurrent]);

  const invalidateRead = useCallback(() => {
    readGeneration.current++;
    activeRead.current = null;
    activeMcpReads.current.clear();
  }, []);

  useEffect(() => {
    if (!open) return;
    setBusyProvider(null);
    void loadConnectors();
    return () => {
      activeMutation.current = null;
      invalidateRead();
    };
  }, [open, loadConnectors, invalidateRead]);

  // State disables controls after render; the ref also owns same-turn clicks and
  // late completions after close/reopen. A failed mutation is never replayed.
  const beginMutation = (provider: string) => {
    if (!isCurrent() || activeMutation.current !== null) return null;
    const owner = Symbol(provider);
    activeMutation.current = owner;
    setBusyProvider(provider);
    setError("");
    return owner;
  };
  const mutationCurrent = (owner: symbol) => isCurrent() && activeMutation.current === owner;
  const finishMutation = (owner: symbol) => {
    if (!mutationCurrent(owner)) return;
    activeMutation.current = null;
    setBusyProvider(null);
  };
  const refreshAfterMutation = async (owner: symbol) => {
    if (!mutationCurrent(owner)) return;
    invalidateRead();
    await loadConnectors();
  };

  const handleConnect = async (provider: string) => {
    const owner = beginMutation(provider);
    if (!owner) return;
    try {
      const { data } = await api.get<{ auth_url: string }>(`/api/v1/connectors/auth/${provider}`);
      if (!mutationCurrent(owner)) return;
      window.location.href = data.auth_url;
    } catch (err: unknown) {
      if (!mutationCurrent(owner)) return;
      setError(apiErrorMessage(err, t("connectors.authFailed")));
      finishMutation(owner);
    }
  };

  const handleActivate = async (provider: string) => {
    const owner = beginMutation(provider);
    if (!owner) return;
    try {
      const { data } = await api.post<ConnectorActivateResponse>(
        `/api/v1/connectors/${provider}/activate`,
      );
      if (!mutationCurrent(owner)) return;
      alert(t("connectors.activatedToast", { count: data.tool_count ?? data.tools?.length ?? 0 }));
    } catch (err: unknown) {
      if (!mutationCurrent(owner)) return;
      setError(apiErrorMessage(err, t("connectors.unknownError")));
    }
    await refreshAfterMutation(owner);
    finishMutation(owner);
  };

  const handleDisconnect = async (provider: string) => {
    if (!isCurrent() || activeMutation.current !== null) return;
    if (!confirm(t("connectors.disconnectConfirm"))) return;
    const owner = beginMutation(provider);
    if (!owner) return;
    try {
      await api.delete(`/api/v1/connectors/${provider}`);
      if (!mutationCurrent(owner)) return;
      setMcpByProvider((prev) => {
        const next = { ...prev };
        delete next[provider];
        return next;
      });
    } catch (err: unknown) {
      if (!mutationCurrent(owner)) return;
      setError(apiErrorMessage(err, t("connectors.disconnectFailed")));
    }
    // Even a transport error can follow a committed DELETE. Reconcile with GET,
    // and keep catalog retry read-only if that observation also fails.
    await refreshAfterMutation(owner);
    finishMutation(owner);
  };

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onClick={onClose}>
      <div className="bg-white dark:bg-zinc-900 rounded-xl shadow-2xl w-[560px] max-h-[80vh] overflow-hidden"
           onClick={(e) => e.stopPropagation()}>
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-zinc-200 dark:border-zinc-700">
          <div>
            <h2 className="text-lg font-semibold">{t("connectors.title")}</h2>
            <p className="text-sm text-zinc-500">{t("connectors.subtitle")}</p>
          </div>
          <button type="button" onClick={onClose} className="p-2 rounded-lg hover:bg-zinc-100 dark:hover:bg-zinc-800 transition" aria-label={t("connectors.closeAria")}>
            <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
              <path d="M4 4L12 12M12 4L4 12" stroke="currentColor" strokeWidth="2" strokeLinecap="round"/>
            </svg>
          </button>
        </div>

        {/* Content */}
        <div className="px-6 py-4 space-y-4 overflow-y-auto max-h-[60vh]">
          {authorizationReturnFailed && <p role="status" className="text-sm text-amber-700 dark:text-amber-300">{t("connectors.authorizationReturnFailed")}</p>}
          {loading ? (
            <div className="text-sm text-zinc-500 py-4 text-center">{t("connectors.loading")}</div>
          ) : loadFailed ? (
            <div className="space-y-3">
              <p role="alert" className="text-sm text-red-500">{t("connectors.loadFailed")}</p>
              <button type="button" onClick={() => void loadConnectors()} className="rounded-lg border px-3 py-1 text-sm">{t("common.retry")}</button>
            </div>
          ) : connectors.length === 0 ? (
            <div className="text-sm text-zinc-500 py-4 text-center">
              {t("connectors.empty")}
            </div>
          ) : (
            connectors.map((conn) => {
              const mcp = mcpByProvider[conn.provider];
              const observation = mcpReadStatus[conn.provider];
              return (
                <div key={conn.provider} className="rounded-lg border border-zinc-200 dark:border-zinc-700 overflow-hidden">
                  <div className="flex items-center gap-3 p-3">
                    {conn.icon_url ? (
                      <img
                        src={conn.icon_url}
                        alt=""
                        className="h-5 w-5 rounded object-contain"
                      />
                    ) : (
                      <Plug className="h-5 w-5 text-zinc-500" aria-hidden="true" />
                    )}
                    <div className="flex-1 min-w-0">
                      <div className="text-sm font-medium">{conn.display_name}</div>
                      <div className="text-xs text-zinc-500 truncate">{conn.description}</div>
                    </div>
                    <div className={`w-2 h-2 rounded-full ${conn.connected ? "bg-green-500" : "bg-zinc-400"}`} />
                  </div>

                  <div className="px-3 pb-3 flex items-center justify-between">
                    {conn.connected ? (
                      <>
                        <span role={observation === "failed" ? "alert" : "status"} className={`text-xs ${observation === "ready" ? "text-green-600 dark:text-green-400" : "text-zinc-500"}`}>
                          {observation !== "ready"
                            ? t(observation === "failed" ? "connectors.mcpReadFailed" : "connectors.mcpLoading")
                            : mcp?.mcp_active
                              ? t("connectors.activated", { count: mcp.tools?.length ?? 0 })
                              : t("connectors.connectedNoTools")}
                        </span>
                        <div className="flex gap-2">
                          {observation === "failed" && <button type="button"
                            onClick={() => void loadMcpStatus(conn.provider, readGeneration.current)}
                            disabled={busyProvider !== null}
                            aria-label={`${t("connectors.retryStatus")} ${conn.display_name}`}
                            className="rounded-lg border px-3 py-1 text-xs disabled:opacity-50"
                          >{t("connectors.retryStatus")}</button>}
                          {observation === "ready" && !mcp?.mcp_active && (
                            <button
                              onClick={() => handleActivate(conn.provider)}
                              disabled={busyProvider !== null}
                              aria-label={`${t("connectors.activate")} ${conn.display_name}`}
                              className="px-3 py-1 text-xs font-medium text-white bg-violet-600 hover:bg-violet-700 rounded-lg transition disabled:opacity-50"
                            >
                              {busyProvider === conn.provider ? t("connectors.processing") : t("connectors.activate")}
                            </button>
                          )}
                          <button
                            onClick={() => handleDisconnect(conn.provider)}
                            disabled={busyProvider !== null}
                            aria-label={`${t("connectors.disconnect")} ${conn.display_name}`}
                            className="px-2 py-1 text-xs text-red-600 hover:bg-red-50 dark:hover:bg-red-900/20 rounded transition"
                          >
                            {t("connectors.disconnect")}
                          </button>
                        </div>
                      </>
                    ) : (
                      <>
                        <span role="status" className="text-xs text-zinc-400">{t(conn.status === "expired" ? "connectors.authorizationExpired" : conn.status === "revoked" ? "connectors.authorizationRevoked" : "connectors.notConnected")}</span>
                        <div className="flex gap-2">
                        <button
                          onClick={() => handleConnect(conn.provider)}
                          disabled={busyProvider !== null}
                          aria-label={`${t(conn.status === "expired" || conn.status === "revoked" ? "connectors.reconnect" : "connectors.connect")} ${conn.display_name}`}
                          className="px-3 py-1 text-xs font-medium text-white bg-blue-600 hover:bg-blue-700 rounded-lg transition disabled:opacity-50"
                        >
                          {busyProvider === conn.provider ? t("connectors.redirecting") : t(conn.status === "expired" || conn.status === "revoked" ? "connectors.reconnect" : "connectors.connect")}
                        </button>
                        {conn.status === "expired" && <button type="button"
                          onClick={() => handleDisconnect(conn.provider)}
                          disabled={busyProvider !== null}
                          aria-label={`${t("connectors.disconnect")} ${conn.display_name}`}
                          className="px-2 py-1 text-xs text-red-600 hover:bg-red-50 dark:hover:bg-red-900/20 rounded transition disabled:opacity-50"
                        >{t("connectors.disconnect")}</button>}
                        </div>
                      </>
                    )}
                  </div>
                </div>
              );
            })
          )}

          {error && <div role="alert" className="text-xs text-red-500">{error}</div>}
        </div>

        {/* Footer */}
        <div className="px-6 py-3 border-t border-zinc-200 dark:border-zinc-700 flex items-center justify-between">
          <span className="text-xs text-zinc-400">{t("connectors.footer")}</span>
        </div>
      </div>
    </div>
  );
}
