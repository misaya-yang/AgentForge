from fastapi import APIRouter

from .v1.agent_public import router as agent_public_router
from .v1.agent_runtime import router as agent_runtime_router
from .v1.agents import publication_router as agent_publications_router
from .v1.agents import router as agents_router
from .v1.api_keys import router as api_keys_router
from .v1.architecture_status import router as architecture_status_router
from .v1.artifact_shares import router as artifact_shares_router
from .v1.assistant import router as assistant_router
from .v1.auth import router as auth_router
from .v1.config import router as config_router
from .v1.connector_admin import router as connector_admin_router
from .v1.connectors import router as connectors_router
from .v1.conversation_shares import router as conversation_shares_router
from .v1.conversations import router as conversations_router
from .v1.dashboard import router as dashboard_router
from .v1.eval import router as eval_router
from .v1.files import router as files_router
from .v1.health import router as health_router
from .v1.invoke import router as invoke_router
from .v1.kb_tools import router as kb_tools_router
from .v1.knowledge import router as knowledge_router
from .v1.langgraph import router as langgraph_router
from .v1.mcp import legacy_router as legacy_mcp_router
from .v1.mcp import router as mcp_router
from .v1.metrics import router as metrics_router
from .v1.models import router as models_router
from .v1.presign import router as presign_router
from .v1.providers import router as providers_router
from .v1.proxy import router as proxy_router
from .v1.quiz import public_router as quiz_public_router
from .v1.quiz import router as quiz_router
from .v1.quota import router as quota_router
from .v1.roles import router as roles_router
from .v1.services import router as services_router
from .v1.sessions import router as sessions_router
from .v1.setup import router as setup_router
from .v1.skills import router as skills_router
from .v1.stream import router as stream_router
from .v1.submit import router as submit_router
from .v1.tasks import router as tasks_router
from .v1.tenant_policies import router as tenant_policies_router
from .v1.tool_inventory import router as debug_tools_router
from .v1.usage import router as usage_router
from .v1.users import router as users_router

api_router = APIRouter()

# 认证和用户管理 API
api_router.include_router(auth_router)  # 认证：登录/登出/改密
api_router.include_router(users_router)  # 用户管理 CRUD
api_router.include_router(roles_router)  # 角色权限管理
api_router.include_router(api_keys_router)  # API Key 管理
api_router.include_router(agents_router)  # Agent Studio identity, Draft, Version and ACL
api_router.include_router(agent_publications_router)  # Agent promotion and rollback
api_router.include_router(agent_runtime_router)  # Agent Studio resolved runtime
api_router.include_router(agent_public_router)  # Hosted and origin-bound Embed runtime

# 核心业务 API
api_router.include_router(invoke_router)
api_router.include_router(stream_router)
api_router.include_router(submit_router)
api_router.include_router(tasks_router)
api_router.include_router(sessions_router)
api_router.include_router(services_router)
api_router.include_router(health_router)
api_router.include_router(architecture_status_router)
api_router.include_router(setup_router)  # First-run onboarding state
api_router.include_router(config_router)
api_router.include_router(conversations_router)  # 简化的对话 API
api_router.include_router(langgraph_router)  # LangGraph 官方 API 代理（向后兼容）
api_router.include_router(proxy_router)  # 透明代理路由
api_router.include_router(knowledge_router)  # KBMS Knowledge Base
api_router.include_router(kb_tools_router)  # KB Tools API for LangGraph agents
api_router.include_router(debug_tools_router)  # /debug/tools — live registry visibility
api_router.include_router(metrics_router)  # 系统指标统计
api_router.include_router(usage_router)  # 使用量追踪与分析
api_router.include_router(quota_router)  # 用户配额管理
api_router.include_router(dashboard_router)  # LangSmith-style 实时监控仪表盘
api_router.include_router(eval_router)  # Agent trace eval console APIs
api_router.include_router(files_router)  # 文件上传 API
api_router.include_router(presign_router)  # P2: Presigned URL 直传 API
api_router.include_router(assistant_router)  # GPT-like Assistant API
api_router.include_router(quiz_router)  # Quiz grading shim (deprecated — see ai-quiz plugin)
api_router.include_router(quiz_public_router)  # Public quiz sharing (aliases over artifact_shares)
api_router.include_router(artifact_shares_router)  # Kind-generic artifact shares
api_router.include_router(skills_router)  # Skills upload & management
api_router.include_router(mcp_router)  # MCP server management
api_router.include_router(legacy_mcp_router)  # Existing Assistant MCP route compatibility
api_router.include_router(tenant_policies_router)  # ADR-002: Tenant isolation admin
api_router.include_router(conversation_shares_router)  # Conversation sharing with artifacts
api_router.include_router(connectors_router)  # Third-party connectors (Confluence, Outlook, etc.)
api_router.include_router(connector_admin_router)  # Connector catalog admin (settings surface)
api_router.include_router(providers_router)  # LLM Provider 管理
api_router.include_router(models_router)  # LLM Model 管理
