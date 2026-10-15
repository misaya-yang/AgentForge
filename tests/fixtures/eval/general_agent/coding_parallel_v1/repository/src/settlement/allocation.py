"""Deterministic integer-cent allocation."""

from __future__ import annotations

from collections.abc import Sequence


def allocate_cents(total_cents: int, beneficiaries: Sequence[tuple[str, int]]) -> dict[str, int]:
    """Allocate positive integer cents according to positive integer weights."""
    if total_cents < 0:
        raise ValueError("total_cents must be non-negative")
    if not beneficiaries or any(weight <= 0 for _, weight in beneficiaries):
        raise ValueError("beneficiaries must have positive weights")
    if len({beneficiary_id for beneficiary_id, _ in beneficiaries}) != len(beneficiaries):
        raise ValueError("beneficiary IDs must be unique")

    total_weight = sum(weight for _, weight in beneficiaries)
    return {
        beneficiary_id: total_cents * weight // total_weight
        for beneficiary_id, weight in beneficiaries
    }
