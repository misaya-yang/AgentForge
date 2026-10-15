from datetime import datetime, timezone

from src.services.metrics.usage_recorder import UsageRecord, group_records_by_hour


def test_group_records_by_hour_aggregates_counts_and_tokens():
    now = datetime(2026, 1, 15, 10, 30, tzinfo=timezone.utc).timestamp()
    records = [
        UsageRecord(
            tenant_id="t1",
            user_id="u1",
            model="gpt",
            input_tokens=10,
            output_tokens=5,
            timestamp=now,
        ),
        UsageRecord(
            tenant_id="t1",
            user_id="u1",
            model="gpt",
            input_tokens=3,
            output_tokens=2,
            timestamp=now,
        ),
    ]
    aggregates = group_records_by_hour(records)
    assert len(aggregates) == 1
    agg = list(aggregates.values())[0]
    assert agg["request_count"] == 2
    assert agg["total_input_tokens"] == 13
    assert agg["total_output_tokens"] == 7
