from __future__ import annotations

from datetime import datetime, timezone


def compute_data_status(
    last_ingested_at: datetime | None,
    now: datetime | None = None,
    total_requests: int | None = None,
    max_age_minutes: int = 60,
) -> tuple[str, int]:
    if now is None:
        now = datetime.now(timezone.utc)

    if last_ingested_at is None:
        return ("empty" if total_requests == 0 else "delayed"), 9999

    # 确保时区一致：如果 last_ingested_at 是 offset-naive，假定为 UTC
    if last_ingested_at.tzinfo is None:
        last_ingested_at = last_ingested_at.replace(tzinfo=timezone.utc)

    age_minutes = int((now - last_ingested_at).total_seconds() / 60)

    if age_minutes > max_age_minutes:
        return "delayed", age_minutes

    if total_requests == 0:
        return "empty", age_minutes

    return "ok", age_minutes
