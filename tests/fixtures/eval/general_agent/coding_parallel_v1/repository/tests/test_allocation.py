import unittest

from settlement.allocation import allocate_cents


class AllocationTests(unittest.TestCase):
    def test_allocation_preserves_every_cent_and_breaks_ties_by_id(self) -> None:
        allocations = allocate_cents(10, [("merchant-c", 1), ("merchant-a", 1), ("merchant-b", 1)])

        self.assertEqual(allocations, {"merchant-a": 4, "merchant-b": 3, "merchant-c": 3})
        self.assertEqual(sum(allocations.values()), 10)

    def test_allocation_uses_largest_fractional_remainder(self) -> None:
        allocations = allocate_cents(11, [("reserve", 1), ("seller", 3), ("tax", 2)])

        self.assertEqual(allocations, {"reserve": 2, "seller": 5, "tax": 4})
        self.assertEqual(sum(allocations.values()), 11)


if __name__ == "__main__":
    unittest.main()
