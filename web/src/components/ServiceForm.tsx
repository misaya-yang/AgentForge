import type { RequestBody } from "@/api/contracts";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import YAML from "yaml";

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { SdkAgentServiceForm } from "@/components/SdkAgentServiceForm";
import { DifyServiceForm } from "@/components/DifyServiceForm";
import { ConfigEditor } from "@/components/ConfigEditor";
import { registerService } from "@/api/gateway";
import { api } from "@/lib/api";

function buildDefaultYaml(t: (key: string) => string) {
  return `# ${t("services.langgraph.yaml.header")}
service_id: my-langgraph-agent
name: "My LangGraph Agent"
description: "${t("services.langgraph.yaml.description")}"
service_type: langgraph
supported_modes: [sync, stream]
status: active

connector_type: http
connector_config:
  # ${t("services.langgraph.yaml.proxyConfig")}
  proxy_mode: transparent  # ${t("services.langgraph.yaml.proxyMode")}
  upstream_url: "http://localhost:2024"
  assistant_id: "agent"  # LangGraph assistant ID
  
  # ${t("services.langgraph.yaml.authConfig")}
  auth_token: ""  # ${t("services.langgraph.yaml.authToken")}
  forward_auth: true  # ${t("services.langgraph.yaml.forwardAuth")}
  
  # ${t("services.langgraph.yaml.timeoutConfig")}
  timeout_connect: 5
  timeout_read: 300
  timeout_write: 60
  
  # ${t("services.langgraph.yaml.loadBalance")}
  # upstream_urls: ["http://server1:2024", "http://server2:2024"]
  load_balance_strategy: round_robin  # ${t("services.langgraph.yaml.loadStrategy")}

session_enabled: true

metadata:
  adapter_type: langgraph
`;
}

interface LangGraphFormData {
  deploymentUrl: string;
  graphId: string;
  langsmithApiKey: string;
}

export function ServiceForm({ onRegistered }: { onRegistered?: () => void }) {
  const { t } = useTranslation();
  const defaultYaml = useMemo(() => buildDefaultYaml(t), [t]);
  const defaultYamlRef = useRef(defaultYaml);
  const [tab, setTab] = useState<string>("simple");
  const [yamlText, setYamlText] = useState(defaultYaml);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [sdkSaving, setSdkSaving] = useState(false);
  const [difySaving, setDifySaving] = useState(false);
  const registrationSaving = sdkSaving || difySaving;

  // 简化表单数据 - 只需要两个核心字段
  const [formData, setFormData] = useState<LangGraphFormData>({
    deploymentUrl: "http://localhost:2024",
    graphId: "agent",
    langsmithApiKey: "",
  });

  useEffect(() => {
    if (yamlText === defaultYamlRef.current) {
      setYamlText(defaultYaml);
    }
    defaultYamlRef.current = defaultYaml;
  }, [defaultYaml, yamlText]);

  async function handleSimpleRegister() {
    if (!formData.deploymentUrl || !formData.graphId) {
      setError(t("services.errors.requiredFields"));
      return;
    }

    setSaving(true);
    setError(null);
    try {
      // 自动生成 service_id - 包含 URL 信息以保证唯一性
      // 从 URL 中提取主机名和端口作为前缀
      let urlPrefix = "";
      try {
        const url = new URL(formData.deploymentUrl);
        // 使用主机名（去掉 localhost）和端口
        const host = url.hostname === "localhost" ? "" : url.hostname.split(".")[0];
        const port = url.port && url.port !== "80" && url.port !== "443" ? `-${url.port}` : "";
        urlPrefix = host ? `${host}${port}-` : (port ? `local${port}-` : "");
      } catch {
        // URL 解析失败，使用简单哈希
        const hash = formData.deploymentUrl.split("").reduce((a, b) => {
          a = ((a << 5) - a) + b.charCodeAt(0);
          return a & a;
        }, 0);
        urlPrefix = `svc${Math.abs(hash).toString(36).slice(0, 4)}-`;
      }
      const graphIdClean = formData.graphId.toLowerCase().replace(/[^a-z0-9-]/g, "-");
      const serviceId = `${urlPrefix}${graphIdClean}`.replace(/^-+|-+$/g, "").replace(/-+/g, "-");
      
      await api.post("/api/v1/config/services/langgraph", ({
        service_id: serviceId,
        name: formData.graphId,
        deployment_url: formData.deploymentUrl,
        graph_id: formData.graphId,
        langsmith_api_key: formData.langsmithApiKey || undefined,
        session_enabled: true,
        proxy_mode: "transparent",
      } satisfies RequestBody<"POST /api/v1/config/services/langgraph">));
      onRegistered?.();
      setOpen(false);
      // 重置表单
      setFormData({
        deploymentUrl: "http://localhost:2024",
        graphId: "agent",
        langsmithApiKey: "",
      });
    } catch (err) {
      const message = err instanceof Error ? err.message : t("services.errors.registerFailed");
      setError(message);
    } finally {
      setSaving(false);
    }
  }

  async function handleYamlRegister() {
    setSaving(true);
    setError(null);
    try {
      const obj = YAML.parse(yamlText) as Record<string, unknown>;
      await registerService(obj);
      onRegistered?.();
      setOpen(false);
    } catch (err) {
      const message = err instanceof Error ? err.message : t("services.errors.registerFailed");
      setError(message);
    } finally {
      setSaving(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={(next) => { if (!registrationSaving) setOpen(next); }}>
      <DialogTrigger asChild>
        <Button variant="primary" size="sm" className="h-10 sm:h-8">{t("services.addService")}</Button>
      </DialogTrigger>
      <DialogContent className="w-[95vw] max-w-2xl max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{t("services.registerService")}</DialogTitle>
          <DialogDescription className="sr-only">
            {t(
              "services.registerServiceDescription",
              "Register a LangGraph, Dify or Agent SDK service, or use advanced YAML configuration."
            )}
          </DialogDescription>
        </DialogHeader>

        <Tabs value={tab} onValueChange={(next) => { if (!registrationSaving) setTab(next); }}>
          <TabsList className="grid w-full grid-cols-2 sm:grid-cols-4 h-auto">
            <TabsTrigger value="simple" disabled={registrationSaving}>LangGraph</TabsTrigger>
            <TabsTrigger value="dify" disabled={registrationSaving}>Dify</TabsTrigger>
            <TabsTrigger value="sdk" disabled={registrationSaving}>{t("services.sdkAgent.tab")}</TabsTrigger>
            <TabsTrigger value="advanced" disabled={registrationSaving}>{t("services.tabs.advanced")}</TabsTrigger>
          </TabsList>

          <TabsContent value="simple" className="space-y-4 pt-4">
            <div className="rounded-lg border bg-muted/30 p-4">
              <div className="flex items-center gap-2 text-sm font-medium text-muted-foreground mb-3">
                <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M13 10V3L4 14h7v7l9-11h-7z" />
                </svg>
                {t("services.langgraph.title")}
              </div>
              <p className="text-xs text-muted-foreground mb-4">
                {t("services.langgraph.description")}
              </p>

              <div className="grid gap-4">
                <div className="space-y-2">
                  <Label htmlFor="deploymentUrl">
                    {t("services.langgraph.deploymentUrl")} <span className="text-red-500">*</span>
                  </Label>
                  <Input
                    id="deploymentUrl"
                    placeholder="http://localhost:2024"
                    value={formData.deploymentUrl}
                    onChange={(e) => setFormData({ ...formData, deploymentUrl: e.target.value })}
                  />
                  <p className="text-xs text-muted-foreground">
                    {t("services.langgraph.deploymentUrlDesc")}
                  </p>
                </div>

                <div className="space-y-2">
                  <Label htmlFor="graphId">
                    {t("services.langgraph.graphId")} <span className="text-red-500">*</span>
                  </Label>
                  <Input
                    id="graphId"
                    placeholder="agent"
                    value={formData.graphId}
                    onChange={(e) => setFormData({ ...formData, graphId: e.target.value })}
                  />
                  <p className="text-xs text-muted-foreground">
                    {t("services.langgraph.graphIdDesc")}
                  </p>
                </div>

                <div className="space-y-2">
                  <Label htmlFor="langsmithApiKey">{t("services.langgraph.langsmithApiKey")}</Label>
                  <Input
                    id="langsmithApiKey"
                    type="password"
                    placeholder="lsv2_pt_..."
                    value={formData.langsmithApiKey}
                    onChange={(e) => setFormData({ ...formData, langsmithApiKey: e.target.value })}
                  />
                  <p className="text-xs text-muted-foreground">
                    {t("services.langgraph.langsmithApiKeyDesc")}
                  </p>
                </div>
              </div>
            </div>

            {error && <div className="text-sm text-destructive">{error}</div>}

            <div className="flex justify-end gap-2">
              <Button variant="outline" onClick={() => setOpen(false)}>
                {t("common.cancel")}
              </Button>
              <Button variant="primary" onClick={handleSimpleRegister} disabled={saving}>
                {saving ? t("services.registering") : t("services.register")}
              </Button>
            </div>
          </TabsContent>

          <TabsContent value="dify" className="space-y-4 pt-4">
            <DifyServiceForm
              onBusyChange={setDifySaving}
              onRegistered={() => {
                onRegistered?.();
                setOpen(false);
              }}
              onCancel={() => setOpen(false)}
            />
          </TabsContent>

          <TabsContent value="sdk" className="space-y-4 pt-4">
            <SdkAgentServiceForm
              onBusyChange={setSdkSaving}
              onRegistered={() => { onRegistered?.(); setOpen(false); }}
              onCancel={() => setOpen(false)}
            />
          </TabsContent>

          <TabsContent value="advanced" className="space-y-4 pt-4">
            <ConfigEditor value={yamlText} onChange={setYamlText} height={360} />
            {error && <div className="text-sm text-destructive">{error}</div>}
            <div className="flex justify-end gap-2">
              <Button variant="outline" onClick={() => setOpen(false)}>
                {t("common.cancel")}
              </Button>
              <Button variant="primary" onClick={handleYamlRegister} disabled={saving}>
                {saving ? t("services.registering") : t("services.register")}
              </Button>
            </div>
          </TabsContent>
        </Tabs>
      </DialogContent>
    </Dialog>
  );
}
