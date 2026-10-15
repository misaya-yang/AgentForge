"""
Tests for SQL query construction safety.

Ensures SQL parameters are safely indexed to prevent off-by-one errors.
"""

import re


class TestSQLSafety:
    """Test SQL query construction safety."""

    def test_parameter_indices_are_correct(self):
        """SQL parameter indices should match params list."""
        from src.persistence.database import build_service_query

        query, params = build_service_query(status="active", service_type="llm", tags=["ai", "ml"])

        # Count $N placeholders
        placeholders = re.findall(r"\$(\d+)", query)
        placeholder_nums = [int(p) for p in placeholders]

        # Verify sequential starting from 1
        assert placeholder_nums == list(range(1, len(params) + 1)), (
            f"Placeholders {placeholder_nums} don't match expected 1-{len(params)}"
        )

        # Verify params count matches
        assert len(params) == len(placeholders), (
            f"Params count {len(params)} != placeholder count {len(placeholders)}"
        )

    def test_no_parameters_produces_valid_query(self):
        """Query with no filters should have no placeholders."""
        from src.persistence.database import build_service_query

        query, params = build_service_query()

        placeholders = re.findall(r"\$(\d+)", query)
        assert len(placeholders) == 0
        assert len(params) == 0

    def test_single_parameter(self):
        """Query with one filter should have $1."""
        from src.persistence.database import build_service_query

        query, params = build_service_query(status="active")

        placeholders = re.findall(r"\$(\d+)", query)
        assert placeholders == ["1"]
        assert len(params) == 1
        assert params[0] == "active"

    def test_multiple_parameters_sequence(self):
        """Multiple parameters should be sequential ($1, $2, $3)."""
        from src.persistence.database import build_service_query

        # All three parameters
        query, params = build_service_query(status="active", service_type="llm", tags=["tag1"])

        placeholders = re.findall(r"\$(\d+)", query)
        assert placeholders == ["1", "2", "3"]
        assert params == ["active", "llm", ["tag1"]]

    def test_skipped_parameters_still_sequential(self):
        """Skipping middle parameter should still produce sequential indices."""
        from src.persistence.database import build_service_query

        # Skip service_type
        query, params = build_service_query(status="active", tags=["tag1"])

        placeholders = re.findall(r"\$(\d+)", query)
        assert placeholders == ["1", "2"]  # Not ["1", "3"]!
        assert params == ["active", ["tag1"]]

    def test_query_has_order_by(self):
        """Query should have ORDER BY clause."""
        from src.persistence.database import build_service_query

        query, _ = build_service_query()
        assert "ORDER BY" in query

    def test_query_is_safe_from_injection(self):
        """Query builder should use parameterized queries, not string formatting."""
        from src.persistence.database import build_service_query

        # Pass potentially malicious input
        query, params = build_service_query(
            status="'; DROP TABLE services; --", service_type="<script>alert(1)</script>"
        )

        # The malicious strings should be in params, not interpolated into query
        assert "DROP TABLE" not in query
        assert "<script>" not in query
        assert "'; DROP TABLE services; --" in params
        assert "<script>alert(1)</script>" in params
