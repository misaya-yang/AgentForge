"""
透明代理模块

提供通用的 HTTP 透明代理功能，支持：
- 动态路由转发
- SSE 流式传输
- 插件化中间件（鉴权、限流、上下文注入）
- 流式计费抽样
"""

from .billing_interceptor import BillingInterceptor, UsageData
from .config_loader import ProxyConfigLoader, ProxyServiceConfig
from .context_injector import ContextInjector, RequestContext
from .response_cache import ResponseCache
from .transparent_proxy import ProxyRequest, ProxyResponse, TransparentProxy

__all__ = [
    "TransparentProxy",
    "ProxyRequest",
    "ProxyResponse",
    "ProxyConfigLoader",
    "ProxyServiceConfig",
    "BillingInterceptor",
    "UsageData",
    "ContextInjector",
    "RequestContext",
    "ResponseCache",
]
