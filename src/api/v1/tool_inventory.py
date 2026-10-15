"""Authenticated diagnostics for the Gateway-owned Runtime capability catalog."""

from __future__ import annotations

from typing import Any

from fastapi import APIRouter, Depends, Request

from ...core.assistant_capability_catalog import project_assistant_tools
from ...core.auth.user_resolver import UserContext
from ..deps import get_user_context

router = APIRouter(prefix="/debug", tags=["Debug"])


@router.get("/tools")
async def list_registered_tools(
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> dict[str, Any]:
    """Return the tenant-visible catalog used for the next Runtime turn."""
    if not getattr(user, "is_authenticated", False):
        from fastapi import HTTPException
        raise HTTPException(status_code=401, detail="authentication required")
    del request
    tools = project_assistant_tools(user)

    by_category: dict[str, list[str]] = {}
    for t in tools:
        category = str(t.get("category") or "unknown")
        by_category.setdefault(category, []).append(str(t["name"]))

    confluence_tools = [str(t["name"]) for t in tools if "confluence" in str(t["name"])]
    has_new_meta = "confluence_read" in confluence_tools and "confluence_write" in confluence_tools
    has_old_single_purpose = any(
        n in confluence_tools
        for n in (
            "search_confluence",
            "read_confluence_page",
            "create_confluence_page",
            "update_confluence_page",
            "delete_confluence_page",
            "add_confluence_comment",
            "list_confluence_spaces",
            "get_confluence_space",
        )
    )

    return {
        "total": len(tools),
        "by_category": by_category,
        "confluence": {
            "tools": confluence_tools,
            "new_meta_tools_present": has_new_meta,
            "stale_single_purpose_tools_present": has_old_single_purpose,
            "status": (
                "OK — new meta-tools loaded"
                if has_new_meta and not has_old_single_purpose
                else (
                    "STALE — deprecated single-purpose capabilities are still published"
                    if has_old_single_purpose
                    else (
                        "MISSING — Confluence capabilities are not visible to this tenant."
                    )
                )
            ),
        },
    }
