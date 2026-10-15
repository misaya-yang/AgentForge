"""Correct orchestration around the two intentionally defective helpers."""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any

from settlement.allocation import allocate_cents
from settlement.idempotency import request_fingerprint


@dataclass(frozen=True)
class Settlement:
    fingerprint: str
    tenant_id: str
    external_id: str
    allocations: dict[str, int]


class InMemoryLedger:
    def __init__(self) -> None:
        self._records: dict[str, Settlement] = {}

    def get(self, fingerprint: str) -> Settlement | None:
        return self._records.get(fingerprint)

    def put(self, settlement: Settlement) -> None:
        self._records[settlement.fingerprint] = settlement

    @property
    def count(self) -> int:
        return len(self._records)


class SettlementService:
    def __init__(self, ledger: InMemoryLedger) -> None:
        self._ledger = ledger

    def settle(
        self,
        request: Mapping[str, Any],
        beneficiaries: Sequence[tuple[str, int]],
    ) -> Settlement:
        fingerprint = request_fingerprint(request)
        existing = self._ledger.get(fingerprint)
        if existing is not None:
            return existing

        settlement = Settlement(
            fingerprint=fingerprint,
            tenant_id=str(request["tenant_id"]),
            external_id=str(request["external_id"]),
            allocations=allocate_cents(int(request["total_cents"]), beneficiaries),
        )
        self._ledger.put(settlement)
        return settlement
