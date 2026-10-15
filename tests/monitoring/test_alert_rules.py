from __future__ import annotations

import yaml


def test_dlq_and_consumer_lag_alerts_are_configured() -> None:
    with open("docker/monitoring/alert-rules.yml", encoding="utf-8") as fh:
        data = yaml.safe_load(fh)

    alerts = {
        rule["alert"]
        for group in data.get("groups", [])
        for rule in group.get("rules", [])
        if "alert" in rule
    }

    assert {"HighDLQDepth", "CriticalDLQDepth", "EventConsumerLag"} <= alerts
