"""Tests for API key authentication."""

import pytest

from src.core.auth.api_key import verify_api_key
from ai_gateway_core.exceptions import AuthError


class TestVerifyApiKey:
    def test_valid_key(self):
        verify_api_key("key-abc-123", ["key-abc-123", "key-def-456"])

    def test_valid_key_among_many(self):
        verify_api_key("key-def-456", ["key-abc-123", "key-def-456", "key-ghi-789"])

    def test_invalid_key_raises(self):
        with pytest.raises(AuthError, match="Invalid API key"):
            verify_api_key("wrong-key", ["key-abc-123"])

    def test_empty_valid_keys_raises(self):
        with pytest.raises(AuthError, match="Invalid API key"):
            verify_api_key("any-key", [])

    def test_empty_key_against_valid_raises(self):
        with pytest.raises(AuthError, match="Invalid API key"):
            verify_api_key("", ["key-abc-123"])

    def test_single_valid_key(self):
        verify_api_key("only-key", ["only-key"])
