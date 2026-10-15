import { useState } from "react";
import { Link } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Badge } from "@/components/ui/badge";
import { api } from "@/lib/api";
import { canAdministerTenantPolicies } from "@/lib/tenantGovernance";
import { useAuthStore } from "@/store/useAuthStore";
import { copyToClipboard } from "@/lib/clipboard";
import { LoadBalancerSettings } from "./settings/LoadBalancerSettings";
import { AuthSettings } from "./settings/AuthSettings";

// ===== Types =====

interface RateLimitRule {
  scope: string;
  scope_id?: string;
  requests: number;
  window: number;
  burst: number;
  enabled: boolean;
}

interface ApiKeyBody {
  name: string;
  expires_in?: number;
}

interface CapacityBudgetStatus {
  key: string;
  limit: number;
  inflight: number;
  queue_depth: number;
  enforced: boolean;
  source_status: string;
}

interface ApiKey {
  id: string;
  name: string;
  key?: string;
  tenant_id?: string;
  roles?: string[];
  enabled: boolean;
  created_at: string;
  expires_at?: string;
}

// ===== API 调用 =====

async function getSystemStatus() {
  const { data } = await api.get("/api/v1/config/status");
  return data;
}

async function getRateLimits() {
  const { data } = await api.get("/api/v1/config/rate-limits");
  return data;
}

async function createRateLimit(rule: RateLimitRule) {
  const { data } = await api.post("/api/v1/config/rate-limits", rule);
  return data;
}

async function getApiKeys() {
  const { data } = await api.get("/api/v1/config/api-keys");
  return data;
}

async function createApiKey(body: ApiKeyBody) {
  const { data } = await api.post("/api/v1/config/api-keys", body);
  return data;
}

export function SettingsPage() {
  const canGovern = useAuthStore(state => canAdministerTenantPolicies(state.user));
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [newApiKey, setNewApiKey] = useState<string | null>(null);

  // 查询
  const statusQuery = useQuery({ queryKey: ["system-status"], queryFn: getSystemStatus });
  const rateLimitsQuery = useQuery({ queryKey: ["rate-limits"], queryFn: getRateLimits });
  const apiKeysQuery = useQuery({ queryKey: ["api-keys"], queryFn: getApiKeys });

  const status = statusQuery.data ?? {};
  const rateLimits = rateLimitsQuery.data?.rules ?? [];
  const apiKeys = apiKeysQuery.data?.keys ?? [];
  const capacity = status.capacity ?? {};
  const capacityBudgets: CapacityBudgetStatus[] = capacity.budgets ?? [];

  // 限流表单
  const [rateLimitForm, setRateLimitForm] = useState({
    scope: "global",
    scope_id: "",
    requests: 100,
    window: 60,
    burst: 10,
  });

  // API Key 表单
  const [apiKeyForm, setApiKeyForm] = useState({
    name: "",
    tenant_id: "",
    roles: ["user"],
  });

  // Mutations
  const createRateLimitMutation = useMutation({
    mutationFn: createRateLimit,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["rate-limits"] });
      setRateLimitForm({ scope: "global", scope_id: "", requests: 100, window: 60, burst: 10 });
    },
  });

  const createApiKeyMutation = useMutation({
    mutationFn: createApiKey,
    onSuccess: (data) => {
      setNewApiKey(data.api_key);
      queryClient.invalidateQueries({ queryKey: ["api-keys"] });
      setApiKeyForm({ name: "", tenant_id: "", roles: ["user"] });
    },
  });

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold text-foreground">{t("settings.title")}</h1>
          <p className="text-muted-foreground">{t("settings.subtitle")}</p>
        </div>
      </div>

      {/* 配置层级说明 */}
      <Card className="relative overflow-hidden border-border/80 bg-muted/20">
        <div className="absolute left-0 top-0 bottom-0 w-1 bg-primary" />
        <CardContent className="pt-4 pl-5">
          <div className="flex items-start gap-3">
            <div className="mt-0.5 p-1.5 rounded-full bg-primary/10 dark:bg-primary/20">
              <svg className="h-4 w-4 text-primary" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M13 16h-1v-4h-1m1-4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
              </svg>
            </div>
            <div>
              <h3 className="font-semibold text-sm text-foreground">
                {t("settings.configLevels.title")}
              </h3>
              <p className="text-xs text-muted-foreground mt-1">
                {t("settings.configLevels.description")}
              </p>
              <ul className="text-xs text-muted-foreground mt-1.5 space-y-1 list-disc list-inside">
                <li>{t("settings.configLevels.globalConfig")}</li>
                <li>{t("settings.configLevels.serviceConfig")}</li>
              </ul>
              <p className="text-xs text-foreground mt-2 font-medium">
                {t("settings.configLevels.priority")}
              </p>
            </div>
          </div>
        </CardContent>
      </Card>

      {/* 统一系统状态与连接器概览条 */}
      <div className="flex flex-wrap items-center justify-between gap-4 p-3.5 rounded-lg border border-border/70 bg-card">
        <div className="flex flex-wrap items-center gap-3">
          <span className="text-xs text-muted-foreground uppercase tracking-wider font-semibold mr-1">
            {t("settings.systemStatus.title")}
          </span>
          <div className="flex items-center gap-2 px-2.5 py-1 rounded-md bg-muted/60 border border-border/50">
            <div className={`h-2 w-2 rounded-full ${status.database?.connected ? "bg-emerald-500" : "bg-muted-foreground"}`} />
            <span className="text-xs font-medium text-foreground">PostgreSQL</span>
            <Badge variant="outline" className={`text-[10px] py-0 px-1.5 ${status.database?.connected ? "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400 border-emerald-500/20" : "bg-muted text-muted-foreground"}`}>
              {status.database?.enabled ? (status.database?.connected ? t("settings.systemStatus.connected") : t("settings.systemStatus.disconnected")) : t("settings.systemStatus.notEnabled")}
            </Badge>
          </div>
          <div className="flex items-center gap-2 px-2.5 py-1 rounded-md bg-muted/60 border border-border/50">
            <div className={`h-2 w-2 rounded-full ${status.redis?.connected ? "bg-emerald-500" : "bg-muted-foreground"}`} />
            <span className="text-xs font-medium text-foreground">Redis</span>
            <Badge variant="outline" className={`text-[10px] py-0 px-1.5 ${status.redis?.connected ? "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400 border-emerald-500/20" : "bg-muted text-muted-foreground"}`}>
              {status.redis?.enabled ? (status.redis?.connected ? t("settings.systemStatus.connected") : t("settings.systemStatus.disconnected")) : t("settings.systemStatus.notEnabled")}
            </Badge>
          </div>
          <div className="flex items-center gap-2 px-2.5 py-1 rounded-md bg-muted/60 border border-border/50">
            <span className="text-xs font-medium text-muted-foreground">{t("settings.systemStatus.loadBalancer")}</span>
            <span className="text-xs font-semibold text-foreground font-mono">{status.load_balancer?.strategy || "round_robin"}</span>
          </div>
          <div className="flex items-center gap-2 px-2.5 py-1 rounded-md bg-muted/60 border border-border/50">
            <span className="text-xs font-medium text-muted-foreground">{t("settings.systemStatus.capacity")}</span>
            <span className="text-xs font-semibold text-foreground font-mono">{capacity.mode || "single-node"}</span>
          </div>
        </div>

        {canGovern && <Link to="/settings/governance" className="inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium text-primary transition-colors hover:bg-primary/5 hover:underline">{t("tenantGovernance.title")}</Link>}
        <Link to="/settings/knowledge-api-keys" className="inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium text-primary hover:underline">{t("knowledgeKeys.title")}</Link>
        <Link to="/settings/plugins" className="inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium text-primary transition-colors hover:bg-primary/5 hover:underline">{t("pluginRegistry.title")}</Link>
        <Link
          to="/settings/connectors"
          className="inline-flex items-center gap-1.5 text-xs font-medium text-primary hover:underline px-3 py-1.5 rounded-md hover:bg-primary/5 transition-colors"
        >
          {t("settings.connectors.title")}
          <svg className="h-3.5 w-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M9 5l7 7-7 7" />
          </svg>
        </Link>
      </div>

      <Tabs defaultValue="auth" className="space-y-4">
        <div className="ui-scroll-affordance w-full pb-1">
          <TabsList className="h-auto min-w-max justify-start">
            <TabsTrigger value="auth">{t("settings.tabs.auth")}</TabsTrigger>
            <TabsTrigger value="rate-limit">{t("settings.tabs.rateLimit")}</TabsTrigger>
            <TabsTrigger value="capacity">{t("settings.tabs.capacity")}</TabsTrigger>
            <TabsTrigger value="load-balancer">
              {t("settings.tabs.loadBalancer")}
            </TabsTrigger>
            <TabsTrigger value="api-keys">
              {t("settings.tabs.apiKeys")}
            </TabsTrigger>
          </TabsList>
        </div>

        {/* 鉴权配置 */}
        <TabsContent value="auth" forceMount className="space-y-4 data-[state=inactive]:hidden">
          <AuthSettings />
        </TabsContent>

        {/* 限流配置 */}
        <TabsContent value="rate-limit" className="space-y-4">
          <Card>
            <CardHeader>
              <CardTitle>{t("settings.rateLimit.addTitle")}</CardTitle>
              <CardDescription>{t("settings.rateLimit.addDescription")}</CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
                <div className="space-y-2">
                  <Label>{t("settings.rateLimit.scope")}</Label>
                  <Select
                    value={rateLimitForm.scope}
                    onValueChange={(value) => setRateLimitForm({ ...rateLimitForm, scope: value })}
                  >
                    <SelectTrigger>
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="global">{t("settings.rateLimit.global")}</SelectItem>
                      <SelectItem value="ip">{t("settings.rateLimit.ip")}</SelectItem>
                      <SelectItem value="user">{t("settings.rateLimit.user")}</SelectItem>
                      <SelectItem value="tenant">{t("settings.rateLimit.tenant")}</SelectItem>
                      <SelectItem value="service">{t("settings.rateLimit.service")}</SelectItem>
                    </SelectContent>
                  </Select>
                </div>

                {rateLimitForm.scope !== "global" && (
                  <div className="space-y-2">
                    <Label>{t("settings.rateLimit.scopeId")}</Label>
                    <Input
                      placeholder={rateLimitForm.scope === "service" ? t("settings.rateLimit.serviceIdPlaceholder") : t("settings.rateLimit.scopeIdPlaceholder")}
                      value={rateLimitForm.scope_id}
                      onChange={(e) => setRateLimitForm({ ...rateLimitForm, scope_id: e.target.value })}
                    />
                  </div>
                )}
              </div>

              <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
                <div className="space-y-2">
                  <Label>{t("settings.rateLimit.requests")}</Label>
                  <Input
                    type="number"
                    min={1}
                    max={100000}
                    value={rateLimitForm.requests}
                    onChange={(e) => {
                      const val = parseInt(e.target.value) || 1;
                      setRateLimitForm({ ...rateLimitForm, requests: Math.max(1, Math.min(100000, val)) });
                    }}
                  />
                  <p className="text-xs text-muted-foreground">{t("settings.rateLimit.requestsRange")}</p>
                </div>
                <div className="space-y-2">
                  <Label>{t("settings.rateLimit.window")}</Label>
                  <Input
                    type="number"
                    min={1}
                    max={86400}
                    value={rateLimitForm.window}
                    onChange={(e) => {
                      const val = parseInt(e.target.value) || 60;
                      setRateLimitForm({ ...rateLimitForm, window: Math.max(1, Math.min(86400, val)) });
                    }}
                  />
                  <p className="text-xs text-muted-foreground">{t("settings.rateLimit.windowRange")}</p>
                </div>
                <div className="space-y-2">
                  <Label>{t("settings.rateLimit.burst")}</Label>
                  <Input
                    type="number"
                    min={0}
                    max={1000}
                    value={rateLimitForm.burst}
                    onChange={(e) => {
                      const val = parseInt(e.target.value) || 0;
                      setRateLimitForm({ ...rateLimitForm, burst: Math.max(0, Math.min(1000, val)) });
                    }}
                  />
                  <p className="text-xs text-muted-foreground">{t("settings.rateLimit.burstRange")}</p>
                </div>
              </div>

              <Button
                onClick={() => createRateLimitMutation.mutate({ ...rateLimitForm, enabled: true })}
                disabled={createRateLimitMutation.isPending}
                className="transition-[color,background-color,border-color] duration-150"
              >
                {t("settings.rateLimit.addRule")}
              </Button>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>{t("settings.rateLimit.currentRules")}</CardTitle>
            </CardHeader>
            <CardContent>
              {rateLimits.length === 0 ? (
                <p className="text-sm text-muted-foreground">{t("settings.rateLimit.noRules")}</p>
              ) : (
                <div className="overflow-x-auto">
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>{t("settings.rateLimit.tableHeaders.scope")}</TableHead>
                        <TableHead className="hidden md:table-cell">{t("settings.rateLimit.tableHeaders.scopeId")}</TableHead>
                        <TableHead>{t("settings.rateLimit.tableHeaders.requests")}</TableHead>
                        <TableHead className="hidden sm:table-cell">{t("settings.rateLimit.tableHeaders.window")}</TableHead>
                        <TableHead className="hidden lg:table-cell">{t("settings.rateLimit.tableHeaders.burst")}</TableHead>
                        <TableHead>{t("settings.rateLimit.tableHeaders.status")}</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {rateLimits.map((rule: RateLimitRule) => (
                        <TableRow key={`${rule.scope}-${rule.scope_id || 'default'}-${rule.requests}`}>
                          <TableCell>{rule.scope}</TableCell>
                          <TableCell className="hidden md:table-cell">{rule.scope_id || "-"}</TableCell>
                          <TableCell>{rule.requests}</TableCell>
                          <TableCell className="hidden sm:table-cell">{rule.window}s</TableCell>
                          <TableCell className="hidden lg:table-cell">{rule.burst}</TableCell>
                          <TableCell>
                            <Badge variant={rule.enabled ? "default" : "secondary"}>
                              {rule.enabled ? t("common.active") : t("common.disabled")}
                            </Badge>
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </div>
              )}
            </CardContent>
          </Card>
        </TabsContent>

        <TabsContent value="capacity" className="space-y-4">
          <Card>
            <CardHeader>
              <CardTitle>{t("settings.capacity.title")}</CardTitle>
              <CardDescription>
                {capacity.gateway_instance_id || "gateway-1"} · {capacity.cluster_epoch || "uat-2026-05"}
              </CardDescription>
            </CardHeader>
            <CardContent>
              <div className="overflow-x-auto">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>{t("settings.capacity.budget")}</TableHead>
                      <TableHead>{t("settings.capacity.limit")}</TableHead>
                      <TableHead>{t("settings.capacity.inFlight")}</TableHead>
                      <TableHead>{t("settings.capacity.queue")}</TableHead>
                      <TableHead>{t("settings.capacity.status")}</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {capacityBudgets.map((budget) => (
                      <TableRow key={budget.key}>
                        <TableCell className="font-mono text-xs">{budget.key}</TableCell>
                        <TableCell>{budget.limit}</TableCell>
                        <TableCell>{budget.inflight}</TableCell>
                        <TableCell>{budget.queue_depth}</TableCell>
                        <TableCell>
                          <Badge variant={budget.enforced ? "default" : "secondary"}>
                            {budget.enforced ? budget.source_status : t("settings.capacity.notEnforced")}
                          </Badge>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>
            </CardContent>
          </Card>
        </TabsContent>

        {/* 负载均衡 */}
        <TabsContent value="load-balancer" className="space-y-4">
          <LoadBalancerSettings />
        </TabsContent>

        {/* API Keys */}
        <TabsContent value="api-keys" className="space-y-4">
          <Card>
            <CardHeader>
              <CardTitle>{t("settings.apiKeys.createTitle")}</CardTitle>
              <CardDescription>{t("settings.apiKeys.createDescription")}</CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
                <div className="space-y-2">
                  <Label>{t("settings.apiKeys.name")}</Label>
                  <Input
                    placeholder={t("settings.apiKeys.namePlaceholder")}
                    value={apiKeyForm.name}
                    onChange={(e) => setApiKeyForm({ ...apiKeyForm, name: e.target.value })}
                  />
                </div>
                <div className="space-y-2">
                  <Label>{t("settings.apiKeys.tenantId")}</Label>
                  <Input
                    placeholder={t("settings.apiKeys.tenantIdPlaceholder")}
                    value={apiKeyForm.tenant_id}
                    onChange={(e) => setApiKeyForm({ ...apiKeyForm, tenant_id: e.target.value })}
                  />
                </div>
              </div>

              <Button
                onClick={() => createApiKeyMutation.mutate(apiKeyForm)}
                disabled={createApiKeyMutation.isPending || !apiKeyForm.name}
                className="transition-[color,background-color,border-color] duration-150"
              >
                {t("settings.apiKeys.generate")}
              </Button>

              {newApiKey && (
                <div className="rounded-lg border border-green-500 bg-green-50 dark:bg-green-950 p-4">
                  <p className="text-sm font-medium text-green-800 dark:text-green-200 mb-2">
                    ⚠️ {t("settings.apiKeys.newKeyWarning")}
                  </p>
                  <code className="block bg-white dark:bg-gray-900 rounded p-2 text-sm break-all">
                    {newApiKey}
                  </code>
                  <Button
                    variant="outline"
                    size="sm"
                    className="mt-2"
                    onClick={() => {
                      copyToClipboard(newApiKey);
                    }}
                  >
                    {t("common.copy")}
                  </Button>
      </div>
              )}
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>{t("settings.apiKeys.listTitle")}</CardTitle>
              <CardDescription>{t("knowledgeKeys.configBoundary")}</CardDescription>
              <Link to="/settings/knowledge-api-keys" className="text-sm text-primary hover:underline">{t("knowledgeKeys.manageDatabase")}</Link>
            </CardHeader>
            <CardContent>
              {apiKeys.length === 0 ? (
                <p className="text-sm text-muted-foreground">{t("settings.apiKeys.noKeys")}</p>
              ) : (
                <div className="overflow-x-auto">
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>{t("settings.apiKeys.tableHeaders.name")}</TableHead>
                        <TableHead className="hidden md:table-cell">{t("settings.apiKeys.tableHeaders.tenant")}</TableHead>
                        <TableHead className="hidden sm:table-cell">{t("settings.apiKeys.tableHeaders.roles")}</TableHead>
                        <TableHead>{t("settings.apiKeys.tableHeaders.status")}</TableHead>
                        <TableHead className="hidden lg:table-cell">{t("settings.apiKeys.tableHeaders.createdAt")}</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {apiKeys.map((key: ApiKey) => (
                        <TableRow key={key.id}>
                          <TableCell>{key.name}</TableCell>
                          <TableCell className="hidden md:table-cell">{key.tenant_id || "-"}</TableCell>
                          <TableCell className="hidden sm:table-cell">
                            {(key.roles || []).map((r: string) => (
                              <Badge key={r} variant="outline" className="mr-1">
                                {r}
                              </Badge>
                            ))}
                          </TableCell>
                          <TableCell>
                            <Badge variant={key.enabled ? "default" : "secondary"}>
                              {key.enabled ? t("common.active") : t("common.disabled")}
                            </Badge>
                          </TableCell>
                          <TableCell className="hidden lg:table-cell text-xs text-muted-foreground">
                            {key.created_at ? new Date(key.created_at).toLocaleString() : "-"}
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </div>
              )}
            </CardContent>
          </Card>
        </TabsContent>
      </Tabs>
    </div>
  );
}
