import { Link } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { useAuthStore } from "@/store/useAuthStore";

const surfaces = [
  { path: "/eval?tab=traces", permission: "console:eval:view", en: "Trace viewer", zh: "轨迹查看", detail: "Model calls, tools, approvals and reported cost", hint: "模型调用、工具、审批与已报告成本" },
  { path: "/dashboard?tab=operations", permission: "console:dashboard:view", en: "Cost and quota explorer", zh: "成本与配额", detail: "Measured usage, costs and user quota observations", hint: "已测用量、成本与用户配额观测" },
  { path: "/settings/routing", permission: "routing:manage", en: "Routing policies", zh: "路由策略", detail: "Immutable versions, activation pins and server decision evidence", hint: "不可变版本、激活指纹与服务端决策证据" },
  { path: "/settings/guardrails", permission: "console:settings:view", en: "Guardrails", zh: "护栏", detail: "Policy editing and input/output decision logs", hint: "策略编辑与输入输出决策日志" },
  { path: "/settings/governance", permission: "console:settings:view", en: "MCP and audit governance", zh: "MCP 与审计治理", detail: "Tenant MCP configuration and scoped configuration/tool audit logs", hint: "租户 MCP 配置与配置、工具审计日志" },
  { path: "/settings/mcp-registry", permission: "console:settings:view", en: "MCP schema review", zh: "MCP schema 审阅", detail: "Registry discovery, immutable snapshot IDs and schema change evidence", hint: "注册表发现、不可变快照 ID 与 schema 变更证据" },
  { path: "/settings/identity", permission: "console:settings:view", en: "SSO, SCIM and passkeys", zh: "SSO、SCIM 与通行密钥", detail: "Configured identity providers, scoped credential grants and current session", hint: "已配置身份提供方、限定凭据授权与当前会话" },
  { path: "/knowledge", permission: "knowledge:dataset:view", en: "Knowledge inspection", zh: "知识检查", detail: "Document parsing, retrieval citations and multimodal source evidence", hint: "文档解析、检索引用与多模态来源证据" },
] as const;
export function PlatformCapabilities() {
  const { i18n } = useTranslation(), hasPermission = useAuthStore(state => state.hasPermission);
  const chinese = i18n.language.startsWith("zh");
  const available = surfaces.filter(surface => hasPermission(surface.permission));
  if (!available.length) return null;
  return <section className="space-y-3" aria-labelledby="platform-capabilities-title">
    <h2 id="platform-capabilities-title" className="text-lg font-semibold">{chinese ? "平台工作台" : "Platform workbench"}</h2>
    <p className="text-sm text-muted-foreground">{chinese ? "入口按当前权限展示；可用性、数据及写入结果均以各服务端响应为准。" : "Entries follow current permissions; availability, data and write outcomes come from each server response."}</p>
    <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">{available.map(surface => <Link key={surface.path} to={surface.path} className="rounded-xl border bg-card p-4 transition-colors hover:border-primary focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2">
      <h3 className="font-medium">{chinese ? surface.zh : surface.en}</h3><p className="mt-1 text-sm text-muted-foreground">{chinese ? surface.hint : surface.detail}</p>
    </Link>)}</div>
  </section>;
}
