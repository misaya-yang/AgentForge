from __future__ import annotations

from pathlib import Path

import pytest

from scripts.harness import agent_runtime_text_gate as text_gate


def _clear_provider_environment(monkeypatch: pytest.MonkeyPatch) -> None:
    for name in (
        "DASHSCOPE_CHAT_API_KEY",
        "DASHSCOPE_API_KEY",
        "DASHSCOPE_CHAT_BASE_URL",
        "DASHSCOPE_BASE_URL",
        "AI_PLATFORM_AGENT_RUNTIME_TEXT_TTFT_LIMIT_SECONDS",
        "AI_PLATFORM_AGENT_RUNTIME_LONG_OUTPUT_MIN_CHARS",
    ):
        monkeypatch.delenv(name, raising=False)


def test_gate_config_reads_runtime_only_provider_inputs(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _clear_provider_environment(monkeypatch)
    env_file = tmp_path / ".env"
    env_file.write_text(
        "DASHSCOPE_API_KEY=test-only-key\n"
        "DASHSCOPE_BASE_URL=https://dashscope-intl.aliyuncs.com\n"
        "AI_PLATFORM_AGENT_RUNTIME_TEXT_TTFT_LIMIT_SECONDS=5\n"
        "AI_PLATFORM_AGENT_RUNTIME_LONG_OUTPUT_MIN_CHARS=800\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(text_gate, "_runtime_image_is_valid", lambda _image: True)

    config = text_gate.GateConfig.load(
        env_file=env_file,
        runtime_image="runtime:test",
    )

    assert config.runtime_image == "runtime:test"
    assert config.provider_api_key == "test-only-key"
    assert config.provider_base_url == "https://dashscope-intl.aliyuncs.com/compatible-mode"
    assert config.ttft_limit_seconds == 5
    assert config.long_output_min_chars == 800


def test_gate_config_fails_closed_without_provider_credential(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _clear_provider_environment(monkeypatch)
    monkeypatch.setattr(text_gate, "_runtime_image_is_valid", lambda _image: True)

    with pytest.raises(text_gate.GateError, match="credential is required"):
        text_gate.GateConfig.load(
            env_file=tmp_path / "missing.env",
            runtime_image="runtime:test",
        )


def test_gate_config_rejects_unlocked_runtime_before_live_traffic(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _clear_provider_environment(monkeypatch)
    monkeypatch.setenv("DASHSCOPE_API_KEY", "test-only-key")
    monkeypatch.setattr(text_gate, "_runtime_image_is_valid", lambda _image: False)

    with pytest.raises(text_gate.GateError, match="locked Agent Runtime"):
        text_gate.GateConfig.load(
            env_file=tmp_path / "missing.env",
            runtime_image="runtime:dirty",
        )
