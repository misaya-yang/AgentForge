from __future__ import annotations

from typing import Any

import pytest
from ai_gateway_core.models import (
    ModelCapabilityError,
    get_builtin_model_capabilities,
    list_model_capability_adapters,
    merge_model_capability_profiles,
    resolve_reasoning_option,
    safe_model_capability_profile,
)
from ai_gateway_core.models.capabilities import (
    _load_catalog,
    validate_model_capability_profile,
)


def test_catalog_profile_resolves_model_specific_option_without_runtime_model_branch() -> None:
    profile = get_builtin_model_capabilities("dashscope-intl", "qwen3.7-plus")
    assert profile is not None

    resolved = resolve_reasoning_option(profile, "minimal")

    assert resolved.effective == "minimal"
    assert resolved.settings == {
        "effort": "minimal",
        "chat_enabled": True,
        "chat_budget_tokens": 128,
    }
    assert profile["wire_protocols"]["preferred"] == "responses_v1"


def test_operator_override_wins_without_erasing_catalog_sections() -> None:
    catalog = get_builtin_model_capabilities("dashscope", "qwen3.7-plus")
    assert catalog is not None
    override = {
        "reasoning": {
            "default_option": "medium",
            "options": [
                {
                    "id": "medium",
                    "label": "Balanced",
                    "aliases": ["low", "high"],
                    "canonical_effort": "medium",
                    "settings": {
                        "effort": "medium",
                        "chat_enabled": True,
                        "chat_budget_tokens": 768,
                    },
                }
            ],
        }
    }

    effective = merge_model_capability_profiles(catalog, override)

    assert effective["reasoning"]["default_option"] == "medium"
    assert effective["prompt_cache"] == catalog["prompt_cache"]
    assert resolve_reasoning_option(effective, "auto").settings == {
        "effort": "medium",
        "chat_enabled": True,
        "chat_budget_tokens": 768,
    }


def test_unknown_requested_option_falls_back_to_profile_default_with_receipt() -> None:
    profile = get_builtin_model_capabilities("deepseek", "deepseek-v4-pro")
    assert profile is not None

    resolved = resolve_reasoning_option(profile, "medium")

    assert resolved.effective == "high"
    assert resolved.fallback_reason is None

    unknown = resolve_reasoning_option(profile, "invented")
    assert unknown.effective == "high"
    assert unknown.fallback_reason == "unsupported_reasoning_option"


def test_invalid_adapter_setting_is_rejected() -> None:
    profile = safe_model_capability_profile()
    profile["reasoning"] = {
        "adapter_id": "reasoning/deepseek-thinking-effort-v1",
        "default_option": "bad",
        "options": [
            {
                "id": "bad",
                "label": "Bad",
                "aliases": [],
                "canonical_effort": "low",
                "settings": {"enabled": True, "effort": "low"},
            }
        ],
        "visibility": "summary",
        "replay_policy": "preserve_during_tool_turn",
    }

    with pytest.raises(ModelCapabilityError, match="unsupported value"):
        merge_model_capability_profiles({}, profile)


def test_native_web_search_wire_requires_enabled_profile_capability() -> None:
    profile = safe_model_capability_profile()
    profile["tools"]["web_search_wire"] = "native"

    with pytest.raises(ModelCapabilityError, match="requires native_search.enabled"):
        validate_model_capability_profile(profile)


def test_qwen_responses_catalog_declares_provider_native_web_search() -> None:
    profile = get_builtin_model_capabilities("dashscope-intl", "qwen3.7-plus")
    assert profile is not None
    assert profile["native_search"] == {
        "adapter_id": "search/dashscope-native-v1",
        "enabled": True,
        "config": {},
    }
    assert profile["tools"]["web_search_wire"] == "native"


def test_adapter_catalog_is_typed_and_has_unique_ids() -> None:
    adapters = list_model_capability_adapters()
    ids = [adapter["id"] for adapter in adapters]
    assert len(ids) == len(set(ids))
    assert {adapter["kind"] for adapter in adapters} == {
        "reasoning",
        "prompt_cache",
        "native_search",
    }


def test_unhashable_profile_values_raise_model_capability_error_not_type_error() -> None:
    # Admin overrides arrive as arbitrary JSON; an object where a string is
    # expected must yield ModelCapabilityError (-> HTTP 422), never a
    # TypeError 500 from an unhashable membership test.
    mutations: tuple[tuple[tuple[str | int, ...], Any], ...] = (
        (("reasoning", "default_option"), {"bad": "value"}),
        (("reasoning", "visibility"), {"bad": "value"}),
        (("reasoning", "replay_policy"), {"bad": "value"}),
        (("reasoning", "options", 0, "canonical_effort"), {"bad": "value"}),
        (("reasoning", "options", 0, "label"), {"bad": "value"}),
    )
    for path, bad_value in mutations:
        profile = safe_model_capability_profile()
        cursor: Any = profile
        for key in path[:-1]:
            cursor = cursor[key]
        cursor[path[-1]] = bad_value
        with pytest.raises(ModelCapabilityError):
            merge_model_capability_profiles({}, profile)


def test_alias_shadowing_an_option_id_is_rejected_in_either_order() -> None:
    def two_options(alias_first: bool) -> dict[str, Any]:
        low = {
            "id": "low",
            "label": "Low",
            "aliases": [],
            "canonical_effort": "low",
            "settings": {},
        }
        high = {
            "id": "high",
            "label": "High",
            "aliases": ["low"],  # shadows the sibling option id
            "canonical_effort": "high",
            "settings": {},
        }
        profile = safe_model_capability_profile()
        profile["reasoning"] = {
            **profile["reasoning"],
            "default_option": "high",
            "options": [high, low] if alias_first else [low, high],
        }
        return profile

    for alias_first in (False, True):
        with pytest.raises(ModelCapabilityError, match="alias is duplicated"):
            merge_model_capability_profiles({}, two_options(alias_first))


def test_reserved_auto_token_is_rejected_as_option_id_and_alias() -> None:
    def profile_with(**option_overrides: Any) -> dict[str, Any]:
        option = {
            "id": "low",
            "label": "Low",
            "aliases": [],
            "canonical_effort": "low",
            "settings": {},
        }
        option.update(option_overrides)
        profile = safe_model_capability_profile()
        profile["reasoning"] = {
            **profile["reasoning"],
            "default_option": "low",
            "options": [option],
        }
        return profile

    with pytest.raises(ModelCapabilityError, match="invalid, duplicated, or reserved"):
        merge_model_capability_profiles({}, profile_with(id="auto"))
    with pytest.raises(ModelCapabilityError, match="alias is invalid or reserved"):
        merge_model_capability_profiles({}, profile_with(aliases=["auto"]))


def test_builtin_catalog_profiles_validate_and_have_unique_option_keys() -> None:
    entries = _load_catalog()
    assert entries, "packaged capability catalog must not be empty"
    for entry in entries:
        profile = entry.get("capabilities")
        assert isinstance(profile, dict), f"missing capabilities on {entry.get('model_id')}"
        options = profile["reasoning"]["options"]
        ids = [option["id"] for option in options]
        aliases = [alias for option in options for alias in option.get("aliases", [])]
        keys = ids + aliases
        assert len(keys) == len(set(keys)), (
            f"option id/alias shadowing in catalog entry {entry.get('model_id')}"
        )
        assert "auto" not in set(keys)
        assert profile["reasoning"]["default_option"] in set(ids)
        # Full end-to-end validation raises on any schema violation.
        validate_model_capability_profile(profile)


def test_builtin_catalog_has_no_ambiguous_provider_model_match() -> None:
    # get_builtin_model_capabilities returns the first matching entry; two
    # entries claiming the same (provider, model) pair would make the
    # builtin layer depend on catalog ordering.
    seen: set[tuple[str, str]] = set()
    for entry in _load_catalog():
        model_keys = {str(entry.get("model_id") or "").lower()}
        model_keys.update(str(item).lower() for item in entry.get("model_ids", []))
        for provider_key in entry.get("provider_keys", []):
            for model_key in model_keys:
                pair = (str(provider_key).lower(), model_key)
                assert pair not in seen, f"duplicate catalog match for {pair}"
                seen.add(pair)
