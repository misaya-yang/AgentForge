"""
适配器注册表

提供插件化适配器发现机制：
- 自动扫描适配器模块
- 支持装饰器注册
- 支持外部插件加载
- 适配器元数据管理
"""

from __future__ import annotations

import importlib
import inspect
import pkgutil
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from ai_gateway_core.logging import get_logger
from .base import ProtocolAdapter

logger = get_logger(__name__)

# 全局适配器注册表
_adapter_registry: dict[str, type[ProtocolAdapter]] = {}
_adapter_metadata: dict[str, AdapterMetadata] = {}


@dataclass
class AdapterMetadata:
    """
    适配器元数据

    描述适配器的能力和配置。
    """

    name: str
    adapter_class: type[ProtocolAdapter]
    description: str = ""
    version: str = "1.0.0"
    author: str = ""

    # 支持的能力
    capabilities: set[str] = field(default_factory=set)
    # 支持的内容类型
    content_types: set[str] = field(default_factory=set)
    # 支持的调用模式
    invocation_modes: set[str] = field(default_factory=lambda: {"invoke", "stream"})
    # 配置项
    config_schema: dict[str, Any] | None = None

    def to_dict(self) -> dict[str, Any]:
        """转换为字典"""
        return {
            "name": self.name,
            "description": self.description,
            "version": self.version,
            "author": self.author,
            "capabilities": list(self.capabilities),
            "content_types": list(self.content_types),
            "invocation_modes": list(self.invocation_modes),
        }


def register_adapter(
    name: str,
    description: str = "",
    version: str = "1.0.0",
    capabilities: set[str] | None = None,
    content_types: set[str] | None = None,
    invocation_modes: set[str] | None = None,
) -> Callable[[type[ProtocolAdapter]], type[ProtocolAdapter]]:
    """
    适配器注册装饰器

    用于声明式地注册适配器。

    Usage:
        @register_adapter(
            "custom_llm",
            description="Custom LLM adapter",
            capabilities={"text_generation", "chat"},
            content_types={"text"},
        )
        class CustomLLMAdapter(ProtocolAdapter):
            ...
    """

    def decorator(cls: type[ProtocolAdapter]) -> type[ProtocolAdapter]:
        metadata = AdapterMetadata(
            name=name,
            adapter_class=cls,
            description=description or cls.__doc__ or "",
            version=version,
            capabilities=capabilities or set(),
            content_types=content_types or set(),
            invocation_modes=invocation_modes or {"invoke", "stream"},
        )

        _adapter_registry[name] = cls
        _adapter_metadata[name] = metadata

        logger.debug(f"Registered adapter: {name} ({cls.__name__})")
        return cls

    return decorator


def get_adapter(name: str) -> type[ProtocolAdapter] | None:
    """获取已注册的适配器类"""
    return _adapter_registry.get(name)


def get_adapter_metadata(name: str) -> AdapterMetadata | None:
    """获取适配器元数据"""
    return _adapter_metadata.get(name)


def list_adapters() -> list[AdapterMetadata]:
    """列出所有已注册的适配器"""
    return list(_adapter_metadata.values())


def register_adapter_class(name: str, adapter_class: type[ProtocolAdapter]) -> None:
    """
    手动注册适配器类

    Args:
        name: 适配器名称
        adapter_class: 适配器类
    """
    metadata = AdapterMetadata(
        name=name,
        adapter_class=adapter_class,
        description=adapter_class.__doc__ or "",
    )

    _adapter_registry[name] = adapter_class
    _adapter_metadata[name] = metadata

    logger.debug(f"Registered adapter: {name} ({adapter_class.__name__})")


class AdapterDiscovery:
    """
    适配器发现服务

    自动扫描和加载适配器。
    """

    def __init__(self):
        self._discovered: set[str] = set()

    def discover_from_package(self, package_name: str) -> list[str]:
        """
        从 Python 包中发现适配器

        扫描包中所有模块，查找继承自 ProtocolAdapter 的类。

        Args:
            package_name: 包名（如 "src.adapters"）

        Returns:
            发现的适配器名称列表
        """
        discovered = []

        try:
            package = importlib.import_module(package_name)
        except ImportError as e:
            logger.warning(f"Failed to import package {package_name}: {e}")
            return discovered

        # 获取包路径
        package_path = getattr(package, "__path__", None)
        if not package_path:
            return discovered

        # 遍历包中的模块
        for _importer, module_name, _is_pkg in pkgutil.iter_modules(package_path):
            if module_name.startswith("_"):
                continue

            full_module_name = f"{package_name}.{module_name}"

            try:
                module = importlib.import_module(full_module_name)

                # 查找适配器类
                for _name, obj in inspect.getmembers(module, inspect.isclass):
                    if (
                        issubclass(obj, ProtocolAdapter)
                        and obj is not ProtocolAdapter
                        and obj.__module__ == full_module_name
                    ):
                        # 检查是否已通过装饰器注册
                        if obj not in _adapter_registry.values():
                            # 使用模块名作为适配器名称
                            adapter_name = module_name
                            register_adapter_class(adapter_name, obj)
                            discovered.append(adapter_name)
                            self._discovered.add(adapter_name)
                        else:
                            # 已注册，获取名称
                            for reg_name, reg_cls in _adapter_registry.items():
                                if reg_cls is obj:
                                    discovered.append(reg_name)
                                    self._discovered.add(reg_name)
                                    break

            except Exception as e:
                logger.warning(f"Failed to import module {full_module_name}: {e}")

        return discovered

    def discover_from_directory(self, directory: str) -> list[str]:
        """
        从目录中发现适配器

        加载指定目录下的 Python 文件作为适配器模块。

        Args:
            directory: 目录路径

        Returns:
            发现的适配器名称列表
        """
        discovered = []
        dir_path = Path(directory)

        if not dir_path.exists():
            logger.warning(f"Directory not found: {directory}")
            return discovered

        for py_file in dir_path.glob("*.py"):
            if py_file.name.startswith("_"):
                continue

            module_name = py_file.stem

            try:
                # 动态加载模块
                spec = importlib.util.spec_from_file_location(
                    f"plugins.{module_name}",
                    py_file,
                )
                if spec and spec.loader:
                    module = importlib.util.module_from_spec(spec)
                    spec.loader.exec_module(module)

                    # 查找适配器类
                    for _name, obj in inspect.getmembers(module, inspect.isclass):
                        if issubclass(obj, ProtocolAdapter) and obj is not ProtocolAdapter:
                            adapter_name = module_name
                            register_adapter_class(adapter_name, obj)
                            discovered.append(adapter_name)
                            self._discovered.add(adapter_name)

            except Exception as e:
                logger.warning(f"Failed to load plugin {py_file}: {e}")

        return discovered

    def discover_from_entry_points(self, group: str = "gateway.adapters") -> list[str]:
        """
        从 entry_points 发现适配器

        支持通过 pip 安装的插件包。

        Args:
            group: entry_points 组名

        Returns:
            发现的适配器名称列表
        """
        discovered = []

        try:
            from importlib.metadata import entry_points

            eps = entry_points()
            if hasattr(eps, "select"):
                # Python 3.10+
                adapter_eps = eps.select(group=group)
            else:
                # Python 3.9
                adapter_eps = eps.get(group, [])

            for ep in adapter_eps:
                try:
                    adapter_class = ep.load()
                    if inspect.isclass(adapter_class) and issubclass(
                        adapter_class, ProtocolAdapter
                    ):
                        register_adapter_class(ep.name, adapter_class)
                        discovered.append(ep.name)
                        self._discovered.add(ep.name)
                        logger.info(f"Loaded adapter plugin: {ep.name}")
                except Exception as e:
                    logger.warning(f"Failed to load adapter plugin {ep.name}: {e}")

        except ImportError:
            logger.debug("importlib.metadata not available, skipping entry_points discovery")

        return discovered

    @property
    def discovered_adapters(self) -> set[str]:
        """获取已发现的适配器名称"""
        return self._discovered.copy()


# 全局发现服务实例
_discovery = AdapterDiscovery()


def discover_adapters(
    packages: list[str] | None = None,
    directories: list[str] | None = None,
    entry_points: bool = True,
) -> list[str]:
    """
    发现并注册适配器

    Args:
        packages: 要扫描的包列表
        directories: 要扫描的目录列表
        entry_points: 是否从 entry_points 发现

    Returns:
        所有发现的适配器名称
    """
    discovered = []

    # 从包发现
    if packages:
        for package in packages:
            discovered.extend(_discovery.discover_from_package(package))

    # 从目录发现
    if directories:
        for directory in directories:
            discovered.extend(_discovery.discover_from_directory(directory))

    # 从 entry_points 发现
    if entry_points:
        discovered.extend(_discovery.discover_from_entry_points())

    logger.info(f"Discovered {len(discovered)} adapters")
    return discovered


def auto_register_builtin_adapters() -> None:
    """
    自动注册内置适配器

    手动注册所有内置适配器（保持向后兼容）。
    """
    from .comfyui import ComfyUIAdapter
    from .dify import DifyAdapter
    from .generic_rest import GenericRESTAdapter
    from .langgraph import LangGraphAdapter
    from .openai import OpenAIAdapter
    from .stable_diffusion import Text2ImageAdapter
    from .tts import TTSAdapter
    from .whisper import WhisperAdapter

    builtins = [
        ("langgraph", LangGraphAdapter),
        ("openai", OpenAIAdapter),
        ("stable_diffusion", Text2ImageAdapter),
        ("comfyui", ComfyUIAdapter),
        ("whisper", WhisperAdapter),
        ("tts", TTSAdapter),
        ("dify", DifyAdapter),
        ("generic_rest", GenericRESTAdapter),
    ]

    for name, cls in builtins:
        if name not in _adapter_registry:
            register_adapter_class(name, cls)

    logger.info(f"Registered {len(builtins)} built-in adapters")
