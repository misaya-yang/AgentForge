"""Offline contract checks for Agent Eval observations from the V2 Runtime.

This is intentionally independent of any retired Python execution loop. It validates the
public Thread/Turn/Item observation shape used by the live candidate adapter,
including lifecycle pairing and retention receipts that a prose answer cannot
substitute for.
"""

from __future__ import annotations

from ai_gateway_core.eval.runtime_contract import (
    assert_runtime_observation,
    validate_runtime_observation,
)

__all__ = ["assert_runtime_observation", "validate_runtime_observation"]
