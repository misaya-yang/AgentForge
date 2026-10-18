import { lazy, Suspense, type ComponentType } from "react";
import { Navigate, Route, Routes } from "react-router-dom";

import { ProtectedRoute, ForbiddenPage } from "@/components/ProtectedRoute";
import { NotFoundPage } from "@/components/SystemStatusPage";
import { isAgentStudioEnabled } from "@/config/runtime";
import {
  loadTranslationNamespaces,
  type DeferredTranslationNamespace,
} from "@/i18n";

function lazyNamed<TModule, TKey extends keyof TModule>(
  loader: () => Promise<TModule>,
  exportName: TKey
) {
  return lazy(async () => {
    const module = await loader();
    return { default: module[exportName] as ComponentType<object> };
  });
}

function lazyNamedWithNamespaces<TModule, TKey extends keyof TModule>(
  loader: () => Promise<TModule>,
  exportName: TKey,
  ...namespaces: DeferredTranslationNamespace[]
) {
  return lazy(async () => {
    const [, module] = await Promise.all([
      loadTranslationNamespaces(...namespaces),
      loader(),
    ]);
    return { default: module[exportName] as ComponentType<object> };
  });
}

const LoginPage = lazyNamed(() => import("@/pages/Login"), "LoginPage");
const AppLayout = lazyNamed(() => import("@/layouts/AppLayout"), "AppLayout");
const SharePage = lazyNamed(() => import("@/pages/SharePage"), "SharePage");
const QuizPage = lazyNamed(() => import("@/pages/QuizPage"), "QuizPage");
const EnterpriseDashboard = lazyNamed(
  () => import("@/pages/dashboard/index"),
  "EnterpriseDashboard"
);
const WorkbenchPage = lazyNamed(() => import("@/pages/workbench"), "WorkbenchPage");
const ServicesPage = lazyNamed(() => import("@/pages/Services"), "ServicesPage");
const PlaygroundPage = lazyNamed(() => import("@/pages/playground"), "PlaygroundPage");
const TasksPage = lazyNamedWithNamespaces(() => import("@/pages/tasks"), "TasksPage", "tasks");
const SettingsPage = lazyNamed(() => import("@/pages/Settings"), "SettingsPage");
const GuardrailsSettingsPage = lazyNamed(() => import("@/pages/settings/GuardrailsSettings"), "GuardrailsSettingsPage");
const TenantGovernanceSettingsPage = lazyNamed(() => import("@/pages/settings/TenantGovernanceSettings"), "TenantGovernanceSettingsPage");
const RoleManagementPage = lazyNamed(() => import("@/pages/settings/RoleManagement"), "RoleManagementPage");
const RoutingPolicySettings = lazyNamed(() => import("@/pages/settings/RoutingPolicySettings"), "RoutingPolicySettings");
const EnterpriseIdentitySettings = lazyNamed(() => import("@/pages/settings/EnterpriseIdentitySettings"), "EnterpriseIdentitySettings");
const McpRegistrySettings = lazyNamed(() => import("@/pages/settings/McpRegistrySettings"), "McpRegistrySettings");
const KnowledgeApiKeysPage = lazyNamed(() => import("@/pages/settings/KnowledgeApiKeys"), "KnowledgeApiKeysPage");
const PluginsSettingsPage = lazyNamed(() => import("@/pages/settings/PluginsSettings"), "PluginsSettingsPage");
const ConnectionsPage = lazyNamed(() => import("@/pages/Connections"), "ConnectionsPage");
const ConnectorsSettingsPage = lazyNamed(
  () => import("@/pages/settings/ConnectorsSettings"),
  "ConnectorsSettingsPage"
);
const UserManagementPage = lazyNamed(() => import("@/pages/UserManagement"), "UserManagementPage");
const UserEditPage = lazyNamed(() => import("@/pages/UserEdit"), "UserEditPage");
const KnowledgeDatasetsPage = lazyNamed(() => import("@/pages/knowledge/Datasets"), "KnowledgeDatasetsPage");
const KnowledgeDatasetDetailPage = lazyNamedWithNamespaces(() => import("@/pages/knowledge/DatasetDetail"), "KnowledgeDatasetDetailPage", "knowledge", "eval");
const DatasetCreatePage = lazyNamedWithNamespaces(() => import("@/pages/knowledge/DatasetCreate"), "default", "knowledge");
const AssistantPage = lazyNamed(() => import("@/pages/assistant"), "AssistantPage");
const EvalPage = lazyNamedWithNamespaces(() => import("@/pages/eval"), "EvalPage", "eval", "knowledge");
const AgentListPage = lazyNamedWithNamespaces(() => import("@/pages/agents/AgentListPage"), "AgentListPage", "agents");
const AgentCreatePage = lazyNamedWithNamespaces(() => import("@/pages/agents/AgentCreatePage"), "AgentCreatePage", "agents");
const AgentStudioPage = lazyNamedWithNamespaces(() => import("@/pages/agents/AgentStudioPage"), "AgentStudioPage", "agents");
const AgentAnalyticsPage = lazyNamedWithNamespaces(() => import("@/pages/agents/AgentAnalyticsPage"), "AgentAnalyticsPage", "agents");
const AgentHostedPage = lazyNamedWithNamespaces(() => import("@/pages/agent-public"), "AgentHostedPage", "agents");

function RouteFallback() {
  return (
    <div className="flex min-h-[240px] items-center justify-center text-sm text-muted-foreground">
      Loading...
    </div>
  );
}

function AgentStudioFeature({ children }: { children: React.ReactNode }) {
  return isAgentStudioEnabled() ? <>{children}</> : <NotFoundPage />;
}

export function AppRouter() {
  return (
    <Suspense fallback={<RouteFallback />}>
      <Routes>
        {/* Public routes */}
        <Route path="/login" element={<LoginPage />} />
        <Route path="/403" element={<ForbiddenPage />} />
        <Route path="/share/:shareId" element={<SharePage />} />
        <Route path="/a/:publicId" element={<AgentHostedPage />} />
        <Route path="/quiz/:shareCode" element={<QuizPage />} />

        {/* Protected routes - wrapped with single ProtectedRoute at layout level */}
        <Route
          element={
            <ProtectedRoute>
              <AppLayout />
            </ProtectedRoute>
          }
        >
          <Route path="/" element={<Navigate to="/workbench" replace />} />
          <Route path="/workbench" element={<ProtectedRoute blockOnlyRole="model_tester" blockRedirectTo="/playground"><WorkbenchPage /></ProtectedRoute>} />
          <Route
            path="/dashboard"
            element={
              <ProtectedRoute requiredPermission="console:dashboard:view">
                <EnterpriseDashboard />
              </ProtectedRoute>
            }
          />
          <Route
            path="/services"
            element={
              <ProtectedRoute requiredPermission="console:services:view">
                <ServicesPage />
              </ProtectedRoute>
            }
          />
          <Route
            path="/knowledge"
            element={
              <ProtectedRoute requiredPermission="knowledge:dataset:view">
                <KnowledgeDatasetsPage />
              </ProtectedRoute>
            }
          />
          <Route
            path="/knowledge/create"
            element={
              <ProtectedRoute requiredPermission="knowledge:dataset:create">
                <DatasetCreatePage />
              </ProtectedRoute>
            }
          />
          <Route
            path="/knowledge/:datasetId"
            element={
              <ProtectedRoute requiredPermission="knowledge:dataset:view">
                <KnowledgeDatasetDetailPage />
              </ProtectedRoute>
            }
          />
          <Route
            path="/playground"
            element={
              <ProtectedRoute requiredPermission="conversation:playground:access">
                <PlaygroundPage />
              </ProtectedRoute>
            }
          />
          <Route
            path="/assistant"
            element={
              <ProtectedRoute
                requiredPermission="conversation:playground:access"
                blockOnlyRole="model_tester"
                blockRedirectTo="/playground"
              >
                <AssistantPage />
              </ProtectedRoute>
            }
          />
          <Route
            path="/agents"
            element={<AgentStudioFeature><AgentListPage /></AgentStudioFeature>}
          />
          <Route
            path="/agents/new"
            element={<AgentStudioFeature><AgentCreatePage /></AgentStudioFeature>}
          />
          <Route
            path="/agents/:agentId/analytics"
            element={<AgentStudioFeature><AgentAnalyticsPage /></AgentStudioFeature>}
          />
          <Route
            path="/agents/:agentId/*"
            element={<AgentStudioFeature><AgentStudioPage /></AgentStudioFeature>}
          />
          <Route
            path="/eval"
            element={
              <ProtectedRoute requiredPermission="console:eval:view">
                <EvalPage />
              </ProtectedRoute>
            }
          />
          <Route
            path="/tasks"
            element={
              <ProtectedRoute requiredPermission="console:dashboard:view">
                <TasksPage />
              </ProtectedRoute>
            }
          />
          <Route
            path="/settings"
            element={
              <ProtectedRoute requiredPermission="console:settings:view">
                <SettingsPage />
              </ProtectedRoute>
            }
          />
          <Route path="/users/roles" element={<ProtectedRoute requiredPermission="role:list"><RoleManagementPage /></ProtectedRoute>} />
          <Route path="/settings/routing" element={<ProtectedRoute requiredPermission="routing:manage"><RoutingPolicySettings /></ProtectedRoute>} />
          <Route path="/settings/identity" element={<ProtectedRoute requiredPermission="console:settings:view"><EnterpriseIdentitySettings /></ProtectedRoute>} />
          <Route path="/settings/mcp-registry" element={<ProtectedRoute requiredPermission="console:settings:view"><McpRegistrySettings /></ProtectedRoute>} />
          <Route path="/settings/guardrails" element={<ProtectedRoute requiredPermission="console:settings:view"><GuardrailsSettingsPage /></ProtectedRoute>} />
          <Route path="/settings/governance" element={<ProtectedRoute requiredPermission="console:settings:view"><TenantGovernanceSettingsPage /></ProtectedRoute>} />
          <Route path="/settings/knowledge-api-keys" element={<ProtectedRoute requiredPermission="console:settings:view"><KnowledgeApiKeysPage /></ProtectedRoute>} />
          <Route path="/settings/plugins" element={<ProtectedRoute requiredPermission="console:settings:view"><PluginsSettingsPage /></ProtectedRoute>} />
          <Route path="/connections" element={<ConnectionsPage />} />
          <Route
            path="/settings/connectors"
            element={
              <ProtectedRoute requiredPermission="console:settings:view">
                <ConnectorsSettingsPage />
              </ProtectedRoute>
            }
          />
          <Route
            path="/users"
            element={
              <ProtectedRoute requiredPermission="user:list">
                <UserManagementPage />
              </ProtectedRoute>
            }
          />
          <Route
            path="/users/:userId/edit"
            element={
              <ProtectedRoute requiredPermission="user:edit">
                <UserEditPage />
              </ProtectedRoute>
            }
          />
        </Route>
        <Route path="*" element={<NotFoundPage />} />
      </Routes>
    </Suspense>
  );
}
