from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime
from typing import Any


@dataclass
class SessionMessage:
    """会话消息"""

    role: str
    content: Any
    timestamp: datetime = field(default_factory=datetime.utcnow)
    metadata: dict[str, Any] | None = None


@dataclass
class Session:
    """会话数据"""

    session_id: str
    user_id: str
    tenant_id: str = ""
    service_id: str | None = None
    created_at: datetime = field(default_factory=datetime.utcnow)
    updated_at: datetime | None = None
    metadata: dict[str, Any] | None = None
    history: list[SessionMessage] = field(default_factory=list)
    state: dict[str, Any] | None = field(default_factory=dict)
    config: dict[str, Any] | None = field(default_factory=dict)
    status: str = "active"
    expires_at: datetime | None = None
    agent_id: str | None = None
    agent_version_id: str | None = None
    agent_draft_revision: int | None = None
    publication_id: str | None = None
    channel: str | None = None
    runtime_fingerprint: str | None = None
    agent_spec_hash: str | None = None
