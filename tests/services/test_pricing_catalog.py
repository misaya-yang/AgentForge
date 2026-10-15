from decimal import Decimal

from src.services.billing.pricing_catalog import (
    DEFAULT_TOKEN_PRICING_PER_1K_USD,
    microcents_to_usd,
    resolve_pricing,
    usd_to_microcents,
)


def test_google_pricing_catalog_uses_updated_rates():
    gemini_flash = DEFAULT_TOKEN_PRICING_PER_1K_USD["gemini-2.5-flash"]
    gemini_flash_lite = DEFAULT_TOKEN_PRICING_PER_1K_USD["gemini-2.5-flash-lite"]
    gemini_pro = DEFAULT_TOKEN_PRICING_PER_1K_USD["gemini-2.5-pro"]

    assert gemini_flash["input"] == Decimal("0.0003")
    assert gemini_flash["output"] == Decimal("0.0025")
    assert gemini_flash_lite["input"] == Decimal("0.0001")
    assert gemini_flash_lite["output"] == Decimal("0.0004")
    assert gemini_pro["input"] == Decimal("0.00125")
    assert gemini_pro["output"] == Decimal("0.01")


def test_deepseek_pricing_catalog_uses_latest_rates():
    chat = DEFAULT_TOKEN_PRICING_PER_1K_USD["deepseek-chat"]
    reasoner = DEFAULT_TOKEN_PRICING_PER_1K_USD["deepseek-reasoner"]

    assert chat["input"] == Decimal("0.00028")
    assert chat["output"] == Decimal("0.00042")
    assert reasoner["input"] == Decimal("0.00028")
    assert reasoner["output"] == Decimal("0.00042")


def test_resolve_pricing_alias_and_prefix():
    assert resolve_pricing("gemini-3.0-pro") == resolve_pricing("gemini-3-pro-preview")
    assert resolve_pricing("gpt-4o-2024-11-20") == resolve_pricing("gpt-4o")
    assert resolve_pricing("gpt-4o-mini-2024-07-18") == resolve_pricing("gpt-4o-mini")
    assert resolve_pricing("models/gemini-2.5-flash") == resolve_pricing("gemini-2.5-flash")
    assert resolve_pricing("google/gemini-2.5-flash-lite") == resolve_pricing(
        "gemini-2.5-flash-lite"
    )
    assert resolve_pricing("google/models/gemini-2.5-flash") == resolve_pricing(
        "gemini-2.5-flash"
    )


def test_microcents_roundtrip():
    value = Decimal("0.325")
    microcents = usd_to_microcents(value)
    assert microcents == 325000
    assert microcents_to_usd(microcents) == 0.325
