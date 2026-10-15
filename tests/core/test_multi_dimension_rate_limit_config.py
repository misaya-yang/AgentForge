from __future__ import annotations

from src.core.gateway.multi_dimension_rate_limiter import TierLimit, create_rate_limit_config


def test_rate_limit_config_merges_tier_overrides_without_dropping_admin(monkeypatch) -> None:
    monkeypatch.delenv("RATE_LIMIT_ANONYMOUS_LIMIT", raising=False)
    monkeypatch.delenv("RATE_LIMIT_NORMAL_LIMIT", raising=False)
    monkeypatch.delenv("RATE_LIMIT_ADMIN_LIMIT", raising=False)

    config = create_rate_limit_config(
        user_tier_overrides={"normal": TierLimit(requests=123, window=60)}
    )

    assert config.user_tier_limits["normal"].requests == 123
    assert config.user_tier_limits["admin"].requests == 10000
    assert config.user_tier_limits["anonymous"].requests == 10


def test_rate_limit_config_reads_assistant_chat_limit_from_env(monkeypatch) -> None:
    monkeypatch.setenv("RATE_LIMIT_ASSISTANT_CHAT_LIMIT", "240")

    config = create_rate_limit_config()

    assert config.operation_limits["assistant_chat"].requests == 240
