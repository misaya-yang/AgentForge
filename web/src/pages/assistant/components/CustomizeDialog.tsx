/**
 * Customize Dialog — manage Skills and MCP connections.
 * Opens from the sidebar "Customize" button.
 */

import { useState, useEffect, useLayoutEffect, useRef, useCallback } from "react";
import { useTranslation } from "react-i18next";
import {
  Puzzle,
  Cable,
  Upload,
  Trash2,
  RefreshCw,
  Loader2,
  ChevronDown,
  Zap,
} from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import {
  listSkills,
  uploadSkill,
  enableSkill,
  disableSkill,
  deleteSkill,
  type Skill,
} from "@/api/skills";
import {
  listMCPServers,
  listMCPTools,
  refreshMCPServer,
  type MCPServer,
  type MCPTool,
} from "@/api/mcp";

interface CustomizeDialogProps {
  open: boolean;
  onClose: () => void;
}

function activationBlocked(error: unknown): boolean {
  const data = (error as { response?: { data?: unknown } } | null)?.response?.data;
  return JSON.stringify(data)?.includes('"SKILL_ACTIVATION_GATES_REQUIRED"') === true;
}

function CapabilityError({ message, retry }: { message: string; retry: () => void }) {
  const { t } = useTranslation();
  return <div role="alert" className="space-y-2 rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm">
    <p>{message}</p>
    <Button size="sm" variant="outline" onClick={retry}>{t("common.retry")}</Button>
  </div>;
}

export function CustomizeDialog(props: CustomizeDialogProps) {
  const scope = useAuthenticatedReadScope();
  if (!props.open || !scope.isCurrent()) return null;
  return <CustomizeWorkspace key={scope.id} {...props} isCurrent={scope.isCurrent} />;
}

function CustomizeWorkspace({ open, onClose, isCurrent }: CustomizeDialogProps & { isCurrent: () => boolean }) {
  const { t } = useTranslation();
  const mounted = useRef(true);
  useLayoutEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  const ownsView = useCallback(() => mounted.current && isCurrent(), [isCurrent]);
  const close = () => {
    if (!ownsView()) return;
    mounted.current = false;
    onClose();
  };

  return (
    <Dialog open={open} onOpenChange={(v) => !v && close()}>
      <DialogContent className="sm:max-w-2xl max-h-[80vh] overflow-hidden flex flex-col">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Zap className="h-5 w-5 text-primary" />
            {t("assistant.customizeTitle", "自定义 AI 助手")}
          </DialogTitle>
        </DialogHeader>

        <Tabs defaultValue="skills" className="flex-1 overflow-hidden flex flex-col">
          <TabsList className="w-full">
            <TabsTrigger value="skills" className="flex-1 gap-2">
              <Puzzle className="h-4 w-4" />
              {t("assistant.skills", "Skills 技能")}
            </TabsTrigger>
            <TabsTrigger value="mcp" className="flex-1 gap-2">
              <Cable className="h-4 w-4" />
              {t("assistant.mcpConnections", "MCP 连接")}
            </TabsTrigger>
          </TabsList>

          <TabsContent value="skills" className="flex-1 overflow-y-auto mt-4">
            <SkillsTab isCurrent={ownsView} />
          </TabsContent>

          <TabsContent value="mcp" className="flex-1 overflow-y-auto mt-4">
            <MCPTab isCurrent={ownsView} />
          </TabsContent>
        </Tabs>
      </DialogContent>
    </Dialog>
  );
}

// ---------------------------------------------------------------------------
// Skills Tab
// ---------------------------------------------------------------------------

function SkillsTab({ isCurrent }: { isCurrent: () => boolean }) {
  const { t } = useTranslation();
  const [skills, setSkills] = useState<Skill[]>([]);
  const [loading, setLoading] = useState(true);
  const [uploading, setUploading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [readFailed, setReadFailed] = useState(false);
  const pending = useRef(false);
  const fileRef = useRef<HTMLInputElement>(null);

  const load = useCallback(async () => {
    if (!isCurrent()) return;
    setLoading(true);
    setError("");
    setReadFailed(false);
    try {
      const data = await listSkills({ enabled_only: false });
      if (!isCurrent()) return;
      if (!Array.isArray(data.skills)) throw new Error("Invalid Skills read");
      setSkills(data.skills);
    } catch {
      if (isCurrent()) {
        setReadFailed(true);
        setError(t("assistant.skillsLoadFailed"));
      }
    } finally {
      if (isCurrent()) setLoading(false);
    }
  }, [isCurrent, t]);

  useEffect(() => { void load(); }, [load]);

  const handleUpload = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file || pending.current || !isCurrent()) return;
    pending.current = true;
    setUploading(true);
    setError("");
    try {
      await uploadSkill(file);
      await load();
    } catch {
      if (isCurrent()) setError(t("assistant.skillUploadFailed"));
    } finally {
      pending.current = false;
      if (isCurrent()) setUploading(false);
      if (fileRef.current) fileRef.current.value = "";
    }
  };

  const handleToggle = async (skill: Skill, enabled: boolean) => {
    if (pending.current || !isCurrent()) return;
    pending.current = true;
    setBusy(true);
    setError("");
    try {
      if (enabled) await enableSkill(skill.name);
      else await disableSkill(skill.name);
      if (!isCurrent()) return;
      setSkills((prev) =>
        prev.map((s) => (s.name === skill.name ? { ...s, enabled } : s)),
      );
    } catch (error) {
      if (isCurrent()) setError(t(activationBlocked(error) ? "assistant.skillActivationBlocked" : "assistant.skillChangeFailed"));
    } finally {
      pending.current = false;
      if (isCurrent()) setBusy(false);
    }
  };

  const handleDelete = async (skill: Skill) => {
    if (pending.current || !isCurrent()) return;
    if (!confirm(`Delete skill "${skill.name}"?`)) return;
    if (!isCurrent()) return;
    pending.current = true;
    setBusy(true);
    setError("");
    try {
      await deleteSkill(skill.name);
      if (!isCurrent()) return;
      setSkills((prev) => prev.filter((s) => s.name !== skill.name));
    } catch {
      if (isCurrent()) setError(t("assistant.skillDeleteFailed"));
    } finally {
      pending.current = false;
      if (isCurrent()) setBusy(false);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="h-4 w-4 animate-spin mr-2" />
        {t("common.loading", "加载中...")}
      </div>
    );
  }

  if (readFailed) return <CapabilityError message={error} retry={() => void load()} />;

  return (
    <div className="space-y-3">
      {error && <CapabilityError message={error} retry={() => void load()} />}
      {/* Upload button */}
      <div className="flex justify-end">
        <input ref={fileRef} type="file" accept=".md" onChange={handleUpload} className="hidden" />
        <Button
          size="sm"
          variant="outline"
          className="gap-2"
          disabled={uploading || busy}
          onClick={() => fileRef.current?.click()}
        >
          {uploading ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Upload className="h-3.5 w-3.5" />}
          {t("assistant.uploadSkill", "上传 SKILL.md")}
        </Button>
      </div>

      {/* Skills list */}
      {skills.length === 0 ? (
        <div className="text-center py-8 text-muted-foreground text-sm">
          <Puzzle className="h-8 w-8 mx-auto mb-2 opacity-30" />
          {t("assistant.noSkills", "暂无技能")}
        </div>
      ) : (
        skills.map((skill) => (
          <div
            key={skill.name}
            role="group"
            aria-label={skill.title || skill.name}
            className="flex items-center gap-3 p-3 rounded-lg border bg-card hover:bg-accent/20 transition-colors"
          >
            <div className="flex-1 min-w-0">
              <div className="flex items-center gap-2">
                <span className="font-medium text-sm">{skill.title || skill.name}</span>
                <Badge variant={skill.source === "builtin" ? "secondary" : "outline"} className="text-[10px] px-1.5 py-0">
                  {skill.source === "builtin"
                    ? t("assistant.skillBuiltin", "内置")
                    : t("assistant.skillUser", "用户")}
                </Badge>
              </div>
              <p className="text-xs text-muted-foreground mt-0.5 truncate">
                {skill.summary || skill.description}
              </p>
              {skill.tags?.length > 0 && (
                <div className="flex gap-1 mt-1">
                  {skill.tags.slice(0, 3).map((tag) => (
                    <span key={tag} className="text-[10px] px-1.5 py-0 bg-muted rounded">{tag}</span>
                  ))}
                </div>
              )}
            </div>

            <div className="flex items-center gap-2 shrink-0">
              {skill.source !== "builtin" && (
                <Button
                  size="icon"
                  variant="ghost"
                  className="h-7 w-7 text-muted-foreground hover:text-destructive"
                  aria-label={t("assistant.deleteSkillLabel", { name: skill.title || skill.name })}
                  disabled={busy || uploading}
                  onClick={() => handleDelete(skill)}
                >
                  <Trash2 className="h-3.5 w-3.5" />
                </Button>
              )}
              <Switch
                aria-label={t("assistant.enableSkillLabel", { name: skill.title || skill.name })}
                disabled={busy || uploading}
                checked={skill.enabled}
                onCheckedChange={(checked) => handleToggle(skill, checked)}
              />
            </div>
          </div>
        ))
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// MCP Tab
// ---------------------------------------------------------------------------

function MCPTab({ isCurrent }: { isCurrent: () => boolean }) {
  const { t } = useTranslation();
  const [servers, setServers] = useState<MCPServer[]>([]);
  const [tools, setTools] = useState<MCPTool[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState<string | null>(null);
  const [expandedServer, setExpandedServer] = useState<string | null>(null);
  const [error, setError] = useState("");
  const pending = useRef(false);

  const load = useCallback(async () => {
    if (!isCurrent()) return;
    setLoading(true);
    setError("");
    try {
      const [srvData, toolData] = await Promise.all([listMCPServers(), listMCPTools()]);
      if (!isCurrent()) return;
      if (!Array.isArray(srvData.servers) || !Array.isArray(toolData.tools)) throw new Error("Invalid MCP read");
      setServers(srvData.servers);
      setTools(toolData.tools);
    } catch {
      if (isCurrent()) setError(t("assistant.mcpLoadFailed"));
    } finally {
      if (isCurrent()) setLoading(false);
    }
  }, [isCurrent, t]);

  useEffect(() => { void load(); }, [load]);

  const handleRefresh = async (name: string) => {
    if (pending.current || !isCurrent()) return;
    pending.current = true;
    setRefreshing(name);
    try {
      await refreshMCPServer(name);
      await load();
    } catch {
      if (isCurrent()) setError(t("assistant.mcpRefreshFailed"));
    } finally {
      pending.current = false;
      if (isCurrent()) setRefreshing(null);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="h-4 w-4 animate-spin mr-2" />
        {t("common.loading", "加载中...")}
      </div>
    );
  }

  if (error) return <CapabilityError message={error} retry={() => void load()} />;

  return (
    <div className="space-y-3">
      {servers.length === 0 ? (
        <div className="text-center py-8 text-muted-foreground text-sm">
          <Cable className="h-8 w-8 mx-auto mb-2 opacity-30" />
          {t("assistant.noMCPServers", "未配置 MCP 服务器")}
        </div>
      ) : (
        servers.map((srv) => {
          const serverTools = tools.filter((t) => t.name.startsWith(`mcp_${srv.name}__`));
          const isExpanded = expandedServer === srv.name;

          return (
            <div key={srv.name} className="rounded-lg border bg-card">
              {/* Server header */}
              <div
                className="flex items-center gap-3 p-3 cursor-pointer hover:bg-accent/20 transition-colors"
                onClick={() => setExpandedServer(isExpanded ? null : srv.name)}
              >
                <div
                  className={`h-2.5 w-2.5 rounded-full ${
                    srv.connected ? "bg-emerald-500" : "bg-red-400"
                  }`}
                />
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="font-medium text-sm">{srv.name}</span>
                    <Badge variant={srv.connected ? "default" : "destructive"} className="text-[10px] px-1.5 py-0">
                      {srv.connected
                        ? t("assistant.mcpConnected", "已连接")
                        : t("assistant.mcpDisconnected", "未连接")}
                    </Badge>
                  </div>
                  <p className="text-xs text-muted-foreground truncate">{srv.description}</p>
                </div>
                <span className="text-xs text-muted-foreground shrink-0">
                  {t("assistant.mcpTools", { count: srv.tool_count })}
                </span>
                <Button
                  size="icon"
                  variant="ghost"
                  className="h-7 w-7"
                  onClick={(e) => {
                    e.stopPropagation();
                    handleRefresh(srv.name);
                  }}
                  disabled={refreshing !== null}
                >
                  {refreshing === srv.name ? (
                    <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <RefreshCw className="h-3.5 w-3.5" />
                  )}
                </Button>
                <ChevronDown
                  className={`h-4 w-4 transition-transform ${isExpanded ? "rotate-180" : ""}`}
                />
              </div>

              {/* Expanded tools list */}
              {isExpanded && serverTools.length > 0 && (
                <div className="border-t px-3 pb-3 pt-2 space-y-1.5">
                  <p className="text-[11px] text-muted-foreground uppercase tracking-wide">
                    {t("assistant.availableTools", "可用工具")}
                  </p>
                  {serverTools.map((tool) => (
                    <div key={tool.name} className="flex items-start gap-2 text-xs pl-2">
                      <Zap className="h-3 w-3 mt-0.5 text-primary shrink-0" />
                      <div className="min-w-0">
                        <span className="font-mono text-foreground">
                          {tool.name.replace(`mcp_${srv.name}__`, "")}
                        </span>
                        <p className="text-muted-foreground truncate">{tool.description}</p>
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </div>
          );
        })
      )}
    </div>
  );
}
