"""Model-call accounting, terminalization, and interruption recovery."""

from __future__ import annotations

import asyncio
import contextlib
import math
import uuid
from typing import Any

from .authorization import _AuthorizedCall


def _cost_microusd(
    input_tokens: int,
    output_tokens: int,
    *,
    input_price_per_1k: float,
    output_price_per_1k: float,
) -> int:
    return max(
        0,
        math.ceil(
            input_tokens * max(input_price_per_1k, 0.0) * 1_000
            + output_tokens * max(output_price_per_1k, 0.0) * 1_000
        ),
    )


async def _complete_call(
    self,
    *,
    call: _AuthorizedCall,
    input_tokens: int,
    output_tokens: int,
    provider_request_id: str | None,
    _helpers: Any,
) -> None:
    pricing = call.snapshot.get("pricing") or {}
    cost = _helpers._cost_microusd(
        input_tokens,
        output_tokens,
        input_price_per_1k=float(pricing.get("input_price_per_1k") or 0),
        output_price_per_1k=float(pricing.get("output_price_per_1k") or 0),
    )
    await self.database.fetchrow(
        "SELECT complete_assistant_runtime_model_call($1, $2, $3, $4, $5)",
        call.call_id,
        input_tokens,
        output_tokens,
        cost,
        provider_request_id,
    )

async def _fail_call(self, call_id: uuid.UUID, code: str, *, dispatched: bool) -> None:
    status = "unknown" if dispatched else "failed"
    await self.database.execute(
        """
        UPDATE assistant_runtime_model_calls
           SET status = $2, error_code = $3, completed_at = NOW(), updated_at = NOW()
         WHERE call_id = $1 AND status IN ('reserved', 'dispatched')
        """,
        call_id,
        status,
        code,
    )

async def _mark_unknown_if_dispatched(self, call_id: uuid.UUID) -> None:
    async def mark_unknown() -> None:
        with contextlib.suppress(Exception):
            await self.database.execute(
                """
                UPDATE assistant_runtime_model_calls
                   SET status = 'unknown', error_code = 'stream_interrupted',
                       completed_at = NOW(), updated_at = NOW()
                 WHERE call_id = $1 AND status = 'dispatched'
                """,
                call_id,
            )

    # The request task can be cancelled when a client disconnects or a
    # parent turn accepts a sub-agent result. Keep the idempotent terminal
    # write alive so a dispatched call never remains permanently open.
    update_task = asyncio.create_task(mark_unknown())
    await asyncio.shield(update_task)
