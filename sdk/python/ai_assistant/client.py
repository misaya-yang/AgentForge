"""
``AssistantClient`` — the main entry point for the AI Assistant SDK.

Instantiate once, then access sub-modules for chat, sessions, knowledge,
images, artifacts, and tools.

Usage::

    from ai_assistant import AssistantClient

    async with AssistantClient("my-api-key", base_url="https://gateway.example.com") as client:
        # Non-streaming chat
        response = await client.chat.send("What is the weather today?")
        print(response.content)

        # Streaming chat
        async for event in client.chat.stream("Tell me a joke"):
            if event.is_text():
                print(event.text, end="")

        # Session management
        sessions = await client.sessions.list()

        # Knowledge-base Q&A
        datasets = await client.knowledge.list_datasets()
        answer = await client.knowledge.ask("What is Zakat?", [datasets[0].dataset_id])

        # Image generation
        result = await client.images.generate("A sunset over the ocean")

        # Artifacts
        artifacts = await client.artifacts.list_by_session(sessions[0].session_id)
"""

from __future__ import annotations

from typing import Any

from ai_assistant.artifacts import ArtifactModule
from ai_assistant.auth import AuthConfig, AuthManager
from ai_assistant.chat import ChatModule
from ai_assistant.images import ImageModule
from ai_assistant.knowledge import KnowledgeModule
from ai_assistant.mcp import MCPManager
from ai_assistant.runs import RunModule
from ai_assistant.sessions import SessionModule
from ai_assistant.skills import SkillModule
from ai_assistant.threads import ThreadModule
from ai_assistant.tools import ToolModule
from ai_assistant.transport.http import HTTPTransport


class AssistantClient:
    """Main SDK entry point.  Initialize once, use everywhere.

    Supports use as an async context manager for automatic resource cleanup::

        async with AssistantClient("key") as client:
            ...

    Or explicit lifecycle management::

        client = AssistantClient("key")
        try:
            ...
        finally:
            await client.close()
    """

    def __init__(
        self,
        api_key: str,
        *,
        base_url: str = "http://localhost:8080",
        tenant_id: str = "",
        user_id: str | None = None,
        timeout: float = 30.0,
        max_retries: int = 3,
    ) -> None:
        """
        Args:
            api_key: API key issued by the AI Gateway.
            base_url: Gateway base URL (no trailing slash).
            tenant_id: Multi-tenant identifier (empty = default tenant).
            user_id: Optional user identifier for per-user session scoping.
            timeout: Default HTTP request timeout in seconds.
            max_retries: Maximum automatic GET/HEAD retries on transient failures.
                Mutations are sent once.
        """
        self._auth = AuthManager(
            AuthConfig(
                api_key=api_key,
                base_url=base_url,
                tenant_id=tenant_id,
                user_id=user_id,
                timeout=timeout,
                max_retries=max_retries,
            )
        )
        self._transport = HTTPTransport(self._auth)

        # Module accessors — prefer attribute access: client.chat, client.sessions, etc.
        self.chat = ChatModule(self._transport)
        self.sessions = SessionModule(self._transport)
        self.knowledge = KnowledgeModule(self._transport, self.chat)
        self.images = ImageModule(self._transport)
        self.artifacts = ArtifactModule(self._transport)
        self.tools = ToolModule(self._transport)
        self.skills = SkillModule(self._transport)
        self.threads = ThreadModule(self._transport)
        self.runs = RunModule(self._transport)
        self.mcp = MCPManager()

    # -- Configuration accessors ---------------------------------------------

    @property
    def base_url(self) -> str:
        """The gateway base URL this client connects to."""
        return self._auth.base_url

    # -- Lifecycle -----------------------------------------------------------

    async def close(self) -> None:
        """Release all underlying HTTP connections and MCP servers.

        Safe to call multiple times.
        """
        await self.mcp.disconnect()
        await self._transport.close()

    async def __aenter__(self) -> AssistantClient:
        return self

    async def __aexit__(self, *args: Any) -> None:
        await self.close()

    # -- Convenience shortcuts -----------------------------------------------

    async def send(self, message: str, **kwargs: Any):
        """Shortcut for ``client.chat.send(message, **kwargs)``."""
        return await self.chat.send(message, **kwargs)

    async def stream(self, message: str, **kwargs: Any):
        """Shortcut for ``client.chat.stream(message, **kwargs)``."""
        events = self.chat.stream(message, **kwargs)
        body_failed = False
        try:
            async for event in events:
                yield event
        except BaseException as exc:
            body_failed = not isinstance(exc, GeneratorExit)
            raise
        finally:
            try:
                close = getattr(events, "aclose", None)
                if close is not None:
                    await close()
            except BaseException:
                if not body_failed:
                    raise
