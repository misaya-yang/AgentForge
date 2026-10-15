from datetime import datetime, timedelta, timezone

from src.services.metrics.data_status import compute_data_status


def test_delayed_when_no_timestamp():
    status, freshness = compute_data_status(None, now=datetime(2026, 1, 15, tzinfo=timezone.utc))
    assert status == "delayed"
    assert freshness == 9999


def test_ok_when_fresh():
    now = datetime(2026, 1, 15, tzinfo=timezone.utc)
    last = now - timedelta(minutes=10)
    status, freshness = compute_data_status(last, now=now)
    assert status == "ok"
    assert freshness == 10


def test_delayed_when_stale():
    now = datetime(2026, 1, 15, tzinfo=timezone.utc)
    last = now - timedelta(minutes=61)
    status, freshness = compute_data_status(last, now=now)
    assert status == "delayed"
    assert freshness == 61


def test_empty_when_zero_requests():
    now = datetime(2026, 1, 15, tzinfo=timezone.utc)
    last = now - timedelta(minutes=5)
    status, freshness = compute_data_status(last, now=now, total_requests=0)
    assert status == "empty"
    assert freshness == 5
