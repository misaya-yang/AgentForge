"""Stable OpenAPI contract helpers."""

from __future__ import annotations

import re

from fastapi.routing import APIRoute


def stable_openapi_operation_id(route: APIRoute) -> str:
    """Match FastAPI's IDs while making multi-method routes deterministic.

    FastAPI's default generator selects the first item from ``route.methods``,
    which is a set. Sorting preserves the published ``delete`` suffix for the
    catch-all proxy routes and leaves single-method IDs unchanged.
    """
    operation_id = re.sub(r"\W", "_", f"{route.name}{route.path_format}")
    methods = sorted(route.methods or ())
    if not methods:
        raise ValueError(f"route {route.path_format!r} declares no HTTP methods")
    return f"{operation_id}_{methods[0].lower()}"
