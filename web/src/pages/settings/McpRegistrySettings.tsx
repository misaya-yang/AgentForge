import type { RequestBody } from "@/api/contracts";
import { useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";
import { api } from "@/lib/api";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import { Button } from "@/components/ui/button";
import { registryConnections, registryServers, registryTools } from "./mcpRegistryView";
function Registry({ identity, isCurrent }: { identity: string; isCurrent: () => boolean }) {
  const { i18n } = useTranslation(), text = (en: string, zh: string) => i18n.language.startsWith("zh") ? zh : en;
  const [selected, setSelected] = useState(""), [review, setReview] = useState(false), [connection, setConnection] = useState("");
  const [busy, setBusy] = useState(false), [notice, setNotice] = useState(""); const pending = useRef(false);
  const [baseline, setBaseline] = useState<{ server: string; hashes: Record<string, string> }>();
  const servers = useQuery({ queryKey: ["mcp-registry", identity], gcTime: 0, retry: false,
    queryFn: async ({ signal }) => { const { data } = await api.get<unknown>("/api/v1/mcp/servers", { signal }); if (!isCurrent()) throw new Error("Session changed"); return registryServers(data); } });
  const tools = useQuery({ queryKey: ["mcp-registry-tools", identity, selected], enabled: Boolean(selected), gcTime: 0, retry: false,
    queryFn: async ({ signal }) => {
      const { data } = await api.get<unknown>(`/api/v1/mcp/servers/${encodeURIComponent(selected)}/tools`, { signal });
      if (!isCurrent()) throw new Error("Session changed");
      return registryTools(data, selected);
    } });
  const connections = useQuery({ queryKey: ["mcp-registry-connections", identity, selected], enabled: Boolean(selected), gcTime: 0, retry: false,
    queryFn: async ({ signal }) => {
      const { data } = await api.get<unknown>(`/api/v1/mcp/servers/${encodeURIComponent(selected)}/connections`, { signal });
      if (!isCurrent()) throw new Error("Session changed");
      return registryConnections(data, selected);
    } });
  const selectedConnection = connections.data?.find(row => row.connection_id === connection);
  const discover = async () => {
    if (pending.current || !review || !selected || !selectedConnection || !isCurrent()) return;
    pending.current = true; setBusy(true); setNotice(""); setReview(false);
    if (tools.data) setBaseline({ server: selected, hashes: Object.fromEntries(tools.data.map(tool => [tool.tool_id, tool.schema_hash])) });
    try {
      const { data } = await api.post<{ breaking: boolean; changed: unknown[]; removed: unknown[] }>(`/api/v1/mcp/servers/${encodeURIComponent(selected)}/discover`, ({ connection_id: selectedConnection.connection_id, principal_type: selectedConnection.principal_type } satisfies RequestBody<"POST /api/v1/mcp/servers/{server_id}/discover">));
      if (!isCurrent()) return;
      setNotice(data.breaking ? text("Server reports a breaking schema change. Review pinned snapshots and downstream approval policy before use.", "服务端报告破坏性 schema 变更。使用前请审阅固定快照及下游审批策略。") : text("Discovery acknowledged. Review the refreshed snapshot below; this is not a permission grant.", "发现已确认。请审阅下方刷新后的快照；这不是权限授权。"));
      await tools.refetch();
    } catch { if (isCurrent()) setNotice(text("Discovery outcome unconfirmed or unavailable. Refresh metadata before another explicit discovery; no POST was retried.", "发现结果未确认或不可用。请先刷新元数据，再决定是否再次明确执行；未自动重试 POST。")); }
    finally { pending.current = false; if (isCurrent()) setBusy(false); }
  };
  const active = servers.data?.find(server => server.server_id === selected);
  return <main className="space-y-5" aria-busy={busy || servers.isFetching || tools.isFetching}>
    <Link to="/settings">{text("Back to settings", "返回设置")}</Link><h1 className="text-2xl font-semibold">{text("MCP registry and schema review", "MCP 注册表与 schema 审阅")}</h1>
    <p>{text("Registry snapshots describe tools. They do not prove a current permission grant or approve an invocation. Existing runs keep their pinned snapshots. Protocol revision is not reported by this registry API.", "注册表快照描述工具，不代表当前权限授权或调用审批。已有运行继续使用固定快照。此注册表 API 尚未报告协议修订号。")}</p>
    {(servers.isError || tools.isError || connections.isError) && <p role="alert">{text("Registry or snapshot unavailable. This is not an empty tool list.", "注册表或快照不可用；这不代表工具列表为空。")}</p>}
    <label className="block">{text("Server", "服务")}<select className="mt-1 block w-full rounded border bg-background p-2" disabled={busy || servers.isPending || servers.isError} value={selected} onChange={e => { setSelected(e.target.value); setReview(false); setBaseline(undefined); setNotice(""); setConnection(""); }}>
      <option value="">{text("Select a server", "选择服务")}</option>{servers.data?.map(server => <option key={server.server_id} value={server.server_id}>{server.name} · {server.transport} · {server.health_status}</option>)}
    </select></label>
    <Button variant="outline" disabled={servers.isFetching || busy} onClick={() => void servers.refetch()}>{text("Refresh registry", "刷新注册表")}</Button>
    {selected && <section className="space-y-3 rounded-xl border p-4">
      <h2 className="font-semibold">{active?.name || selected}</h2>
      <label className="block">{text("Configured connection", "已配置连接")}<select className="mt-1 block w-full rounded border bg-background p-2" value={connection} disabled={busy || connections.isPending || connections.isError} onChange={e => { setConnection(e.target.value); setReview(false); }}>
        <option value="">{text("Select an enabled connection", "选择启用的连接")}</option>{connections.data?.map(row => <option key={row.connection_id} value={row.connection_id}>{row.connection_id} · {row.principal_type}</option>)}
      </select></label>
      {connections.data?.length === 0 && <p role="status">{text("No enabled connection is available. Configure a tenant connection before discovery.", "没有可用的启用连接。发现前请先配置租户连接。")}</p>}
      <label className="flex gap-2"><input type="checkbox" checked={review} disabled={busy} onChange={e => setReview(e.target.checked)} />{text("I reviewed this server and connection. Discovery can publish changed registry snapshots; it does not invoke tools or grant access.", "我已审阅此服务及连接。发现操作可能发布新的注册表快照，但不会调用工具或授予访问权限。")}</label>
      <div className="flex gap-2"><Button disabled={!review || !selectedConnection || busy || tools.isError || tools.isPending} onClick={() => void discover()}>{text("Discover and review changes", "发现并审阅变更")}</Button>
        <Button variant="outline" disabled={tools.isFetching || busy} onClick={() => void tools.refetch()}>{text("Read snapshot", "读取快照")}</Button></div>
      {tools.isPending && <p role="status">{text("Loading snapshot…", "正在读取快照…")}</p>}
      {!tools.isError && tools.data?.length === 0 && <p role="status">{text("This successful snapshot contains no tools.", "此成功读取的快照未包含工具。")}</p>}
      {tools.data?.map(tool => <article key={tool.tool_id} className="space-y-1 rounded border p-3 text-sm">
        <h3 className="font-medium">{tool.runtime_name}</h3><p className="break-all">{text("Snapshot", "快照")}: {tool.snapshot_id} · v{tool.schema_version} · {tool.schema_hash}</p>
        <p>{tool.risk_level} · {tool.read_only ? text("Reported read-only", "报告为只读") : text("May write", "可能写入")} · {tool.enabled ? text("Enabled metadata", "元数据启用") : text("Disabled metadata", "元数据禁用")}</p>
        {baseline?.server === selected && <p role="status">{!baseline.hashes[tool.tool_id] ? text("Added since selected baseline", "相对所选基线新增") : baseline.hashes[tool.tool_id] !== tool.schema_hash ? text("Schema changed since selected baseline", "schema 相对所选基线已变更") : text("Schema hash unchanged", "schema 指纹未变")}</p>}
        <p>{text("Input fields", "输入字段")}: {Object.keys(tool.input_schema.properties ?? {}).join(", ") || "—"}</p><p>{text("Required fields", "必填字段")}: {tool.input_schema.required?.join(", ") || "—"}</p>
      </article>)}
      {!tools.isError && tools.data && baseline?.server === selected && Object.keys(baseline.hashes).filter(id => !tools.data?.some(tool => tool.tool_id === id)).map(id => <p key={id} role="status">{text("Removed since baseline", "相对基线移除")}: {id}</p>)}
    </section>}
    {notice && <p role="status">{notice}</p>}
  </main>;
}
export function McpRegistrySettings() {
  const scope = useAuthenticatedReadScope();
  return scope.isCurrent() ? <Registry key={scope.id} identity={scope.id} isCurrent={scope.isCurrent} /> : null;
}
