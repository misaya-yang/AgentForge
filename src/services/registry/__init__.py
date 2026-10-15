from __future__ import annotations

from importlib import import_module
from typing import Any

__all__ = [
    "ServiceRegistry",
    "RegistryStorage",
    "MemoryRegistryStorage",
    "DatabaseRegistryStorage",
    "HealthMonitor",
    "LoadBalancer",
]


_EXPORTS = {
    "ServiceRegistry": (".service_registry", "ServiceRegistry"),
    "RegistryStorage": (".service_registry", "RegistryStorage"),
    "MemoryRegistryStorage": (".service_registry", "MemoryRegistryStorage"),
    "DatabaseRegistryStorage": (".database_storage", "DatabaseRegistryStorage"),
    "HealthMonitor": (".health_monitor", "HealthMonitor"),
    "LoadBalancer": (".load_balancer", "LoadBalancer"),
}


def __getattr__(name: str) -> Any:
    target = _EXPORTS.get(name)
    if not target:
        raise AttributeError(name)
    module_name, attr_name = target
    module = import_module(module_name, __package__)
    return getattr(module, attr_name)
