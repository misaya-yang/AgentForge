from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import Any


class TaskStatus(str, Enum):
    PENDING = "pending"
    PROCESSING = "processing"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"

    def can_transition_to(self, status: TaskStatus) -> bool:
        return status in {
            self.PENDING: {self.PROCESSING, self.CANCELLED},
            self.PROCESSING: {self.COMPLETED, self.FAILED, self.CANCELLED},
        }.get(self, set())


@dataclass
class Task:
    """异步任务"""

    task_id: str
    request_id: str
    service_id: str
    status: TaskStatus
    created_at: datetime = field(default_factory=datetime.utcnow)
    updated_at: datetime | None = None
    user_id: str = ""
    tenant_id: str = ""
    callback_url: str | None = None
    result: Any = None
    error: str | None = None
    progress: float = 0.0

    # 扩展字段
    request_data: dict[str, Any] | None = None
    callback_sent: bool = False
    priority: int = 0
    retry_count: int = 0
    max_retries: int = 3
    metadata: dict[str, Any] | None = field(default_factory=dict)
    started_at: datetime | None = None
    completed_at: datetime | None = None

    @property
    def identity(self) -> tuple[str, str, str, str, str]:
        return self.task_id, self.request_id, self.service_id, self.user_id, self.tenant_id


@dataclass(frozen=True)
class TaskAuthority:
    """Detached lifecycle observation; never a claim or transition receipt."""

    identity: tuple[str, str, str, str, str]
    status: TaskStatus
