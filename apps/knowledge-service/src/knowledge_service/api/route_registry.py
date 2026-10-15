"""Required API mounts shared by application construction and offline contracts.

Loading routers does not construct the service app, enter its lifespan, or resolve
dependencies. Keep runtime order and prefixes here, including private routes; an
exporter must select public routes explicitly rather than republishing internals.
"""
from __future__ import annotations

from importlib import import_module

API_MOUNTS = (
    ("capability_plane", ""),
    ("bm25_v2", "/api/v1"),
    ("knowledge", "/api/v1"),
    ("confluence_manual_sync", "/api/v1"),
    ("eval", "/api/v1"),
    ("embedding_migration", "/api/v1"),
)


def include_required_api_routes(target):
    for module, prefix in API_MOUNTS:
        router = import_module(f"{__package__}.routes.{module}").router
        target.include_router(router, prefix=prefix)
