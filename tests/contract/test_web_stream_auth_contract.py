"""Regression guards for authenticated browser streaming transports."""

from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def test_shared_sse_transports_attach_the_current_bearer_token() -> None:
    source = (ROOT / "web" / "src" / "lib" / "sse.ts").read_text(encoding="utf-8")

    assert 'import { getAuthToken } from "@/lib/api"' in source
    assert 'resolved.set("Authorization", `Bearer ${token}`)' in source
    assert source.count("headers: authenticatedHeaders(init.headers)") == 2, (
        "both sseFetch and sseFetchEvents must use authenticated headers"
    )


def test_playground_stream_direct_fetch_attaches_the_current_bearer_token() -> None:
    """The one remaining hand-rolled streaming fetch must still send the token.

    This guarantee used to live on ``hooks/useAgentStream.ts``. That hook was
    the retired Python AgentLoop client and has been deleted; the Playground
    stream is now the only reachable path that builds its own fetch headers
    instead of going through ``sseFetch``.
    """

    source = (
        ROOT / "web" / "src" / "pages" / "playground" / "hooks" / "usePlaygroundStream.ts"
    ).read_text(encoding="utf-8")

    assert "const token = useAuthStore.getState().token;" in source
    assert 'Authorization: `Bearer ${token}`' in source
