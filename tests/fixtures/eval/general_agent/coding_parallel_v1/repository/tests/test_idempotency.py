import unittest

from settlement.idempotency import request_fingerprint


class RequestFingerprintTests(unittest.TestCase):
    def test_fingerprint_is_stable_across_nested_object_order(self) -> None:
        first = {
            "tenant_id": "tenant-a",
            "external_id": "payout-1042",
            "metadata": {
                "invoice": {"currency": "USD", "country": "US"},
                "source": "reconciliation",
            },
        }
        replay = {
            "external_id": "payout-1042",
            "metadata": {
                "source": "reconciliation",
                "invoice": {"country": "US", "currency": "USD"},
            },
            "tenant_id": "tenant-a",
        }

        self.assertEqual(request_fingerprint(first), request_fingerprint(replay))

    def test_fingerprint_remains_tenant_scoped(self) -> None:
        base = {"external_id": "same-external-id", "metadata": {"batch": 7}}
        self.assertNotEqual(
            request_fingerprint({**base, "tenant_id": "tenant-a"}),
            request_fingerprint({**base, "tenant_id": "tenant-b"}),
        )


if __name__ == "__main__":
    unittest.main()
