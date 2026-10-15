"""Tests for JWT configuration helpers."""

import src.core.auth.jwt_config as jwt_config_mod
from src.core.auth.jwt_config import (
    DEFAULT_JWT_ALGORITHM,
    get_jwt_algorithms,
    get_jwt_secret,
)


class TestGetJwtSecret:
    def setup_method(self):
        # Reset global warning flag between tests
        jwt_config_mod._warned_insecure = False

    def test_configured_secret_returned(self):
        assert get_jwt_secret("my-strong-secret") == "my-strong-secret"

    def test_none_raises_outside_dev_mode(self, monkeypatch):
        monkeypatch.delenv("JWT_SECRET", raising=False)
        monkeypatch.delenv("GATEWAY_DEV_MODE", raising=False)
        try:
            get_jwt_secret(None)
        except RuntimeError as exc:
            assert "JWT secret not configured" in str(exc)
        else:  # pragma: no cover
            raise AssertionError("expected missing JWT secret to fail")

    def test_empty_returns_dev_secret_in_explicit_dev_mode(self, monkeypatch):
        monkeypatch.setenv("GATEWAY_DEV_MODE", "true")
        monkeypatch.delenv("JWT_SECRET", raising=False)
        assert get_jwt_secret("") == "dev-only-insecure-secret"

    def test_warning_logged_once(self, caplog, monkeypatch):
        monkeypatch.setenv("GATEWAY_DEV_MODE", "true")
        monkeypatch.delenv("JWT_SECRET", raising=False)
        jwt_config_mod._warned_insecure = False
        get_jwt_secret(None)
        assert "insecure JWT secret" in caplog.text
        caplog.clear()
        get_jwt_secret(None)
        assert caplog.text == ""  # No second warning


class TestGetJwtAlgorithms:
    def test_configured_algorithms(self):
        assert get_jwt_algorithms(["RS256", "HS384"]) == ["RS256", "HS384"]

    def test_none_returns_default(self):
        assert get_jwt_algorithms(None) == [DEFAULT_JWT_ALGORITHM]

    def test_empty_list_returns_default(self):
        assert get_jwt_algorithms([]) == [DEFAULT_JWT_ALGORITHM]
