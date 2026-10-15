import unittest

from settlement import InMemoryLedger, SettlementService


class SettlementServiceTests(unittest.TestCase):
    def test_reordered_retry_is_idempotent_and_balanced_end_to_end(self) -> None:
        ledger = InMemoryLedger()
        service = SettlementService(ledger)
        first_request = {
            "tenant_id": "tenant-a",
            "external_id": "settlement-9001",
            "total_cents": 10,
            "metadata": {
                "window": {"end": "12:00", "start": "11:00"},
                "region": "us",
            },
        }
        replay_request = {
            "metadata": {
                "region": "us",
                "window": {"start": "11:00", "end": "12:00"},
            },
            "total_cents": 10,
            "external_id": "settlement-9001",
            "tenant_id": "tenant-a",
        }
        beneficiaries = [("merchant-c", 1), ("merchant-a", 1), ("merchant-b", 1)]

        first = service.settle(first_request, beneficiaries)
        replay = service.settle(replay_request, beneficiaries)

        self.assertIs(replay, first)
        self.assertEqual(ledger.count, 1)
        self.assertEqual(
            first.allocations,
            {"merchant-a": 4, "merchant-b": 3, "merchant-c": 3},
        )
        self.assertEqual(sum(first.allocations.values()), first_request["total_cents"])


if __name__ == "__main__":
    unittest.main()
