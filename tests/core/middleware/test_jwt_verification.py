"""
Tests for JWT signature verification in streaming middleware.

CRITICAL SECURITY TEST: Ensures JWT tokens are properly verified before
trusting authentication claims.
"""

from dataclasses import dataclass, field
from datetime import datetime, timedelta

import jwt
import pytest


@dataclass
class MockStreamingAuthConfig:
    """Mock config for testing."""

    jwt_enabled: bool = True
    jwt_secret: str = "test-secret-key-minimum-32-chars!"
    jwt_algorithms: list[str] = field(default_factory=lambda: ["HS256"])
    api_key_enabled: bool = False
    api_key_header: str = "X-API-Key"
    guest_session_enabled: bool = True
    guest_session_header: str = "X-Guest-Session"
    anonymous_enabled: bool = True
    anonymous_cookie: str = "ag_anon_id"
    anonymous_header: str = "X-AG-Anonymous-Id"
    whitelist_paths: list[str] = field(default_factory=list)


class TestJWTVerification:
    """Test JWT signature verification in streaming middleware."""

    @pytest.fixture
    def jwt_secret(self):
        return "test-secret-key-minimum-32-chars!"

    @pytest.fixture
    def valid_token(self, jwt_secret):
        """Create a valid JWT signed with correct secret."""
        payload = {
            "sub": "user123",
            "tenant_id": "tenant1",
            "tier": "premium",
            "roles": ["user"],
            "exp": datetime.utcnow() + timedelta(hours=1),
        }
        return jwt.encode(payload, jwt_secret, algorithm="HS256")

    @pytest.fixture
    def forged_token(self):
        """Token signed with wrong secret - should be rejected."""
        payload = {
            "sub": "attacker",
            "tenant_id": "victim_tenant",
            "tier": "admin",
            "roles": ["admin"],
            "exp": datetime.utcnow() + timedelta(hours=1),
        }
        return jwt.encode(payload, "wrong-secret-totally-different!", algorithm="HS256")

    @pytest.fixture
    def expired_token(self, jwt_secret):
        """Expired JWT - should be rejected."""
        payload = {"sub": "user123", "exp": datetime.utcnow() - timedelta(hours=1)}
        return jwt.encode(payload, jwt_secret, algorithm="HS256")

    @pytest.fixture
    def config(self, jwt_secret):
        return MockStreamingAuthConfig(jwt_secret=jwt_secret)

    def test_valid_jwt_is_accepted(self, config, valid_token):
        """Valid JWT with correct signature should authenticate user."""
        from src.core.middleware.streaming import StreamingAuthMiddleware

        middleware = StreamingAuthMiddleware(app=None, config=config)

        scope = {
            "type": "http",
            "headers": [(b"authorization", f"Bearer {valid_token}".encode())],
            "client": ("127.0.0.1", 12345),
        }

        user_info = middleware._extract_user_info(scope)

        assert user_info["is_authenticated"] is True
        assert user_info["user_id"] == "user123"
        assert user_info["tenant_id"] == "tenant1"
        assert user_info["tier"] == "premium"

    def test_forged_jwt_is_rejected(self, config, forged_token):
        """Forged JWT with wrong signature must NOT authenticate."""
        from src.core.middleware.streaming import StreamingAuthMiddleware

        middleware = StreamingAuthMiddleware(app=None, config=config)

        scope = {
            "type": "http",
            "headers": [(b"authorization", f"Bearer {forged_token}".encode())],
            "client": ("127.0.0.1", 12345),
        }

        user_info = middleware._extract_user_info(scope)

        # CRITICAL: Forged token must NOT be authenticated
        assert user_info["is_authenticated"] is False
        # Should fall through to anonymous
        assert "anon:" in user_info["user_id"]

    def test_expired_jwt_is_rejected(self, config, expired_token):
        """Expired JWT should not authenticate."""
        from src.core.middleware.streaming import StreamingAuthMiddleware

        middleware = StreamingAuthMiddleware(app=None, config=config)

        scope = {
            "type": "http",
            "headers": [(b"authorization", f"Bearer {expired_token}".encode())],
            "client": ("127.0.0.1", 12345),
        }

        user_info = middleware._extract_user_info(scope)

        assert user_info["is_authenticated"] is False

    def test_malformed_jwt_is_rejected(self, config):
        """Malformed JWT should not authenticate."""
        from src.core.middleware.streaming import StreamingAuthMiddleware

        middleware = StreamingAuthMiddleware(app=None, config=config)

        scope = {
            "type": "http",
            "headers": [(b"authorization", b"Bearer invalid.token.here")],
            "client": ("127.0.0.1", 12345),
        }

        user_info = middleware._extract_user_info(scope)

        assert user_info["is_authenticated"] is False

    def test_jwt_disabled_falls_through(self):
        """When JWT is disabled, should not try to verify."""
        from src.core.middleware.streaming import StreamingAuthMiddleware

        config = MockStreamingAuthConfig(jwt_enabled=False, jwt_secret="")
        middleware = StreamingAuthMiddleware(app=None, config=config)

        # Even with a valid-looking token, should not authenticate
        scope = {
            "type": "http",
            "headers": [(b"authorization", b"Bearer some.token.here")],
            "client": ("127.0.0.1", 12345),
        }

        user_info = middleware._extract_user_info(scope)

        # Should fall through to anonymous since JWT is disabled
        assert user_info["is_authenticated"] is False

    def test_empty_jwt_secret_falls_through(self, valid_token):
        """When JWT secret is empty, should not authenticate JWT."""
        from src.core.middleware.streaming import StreamingAuthMiddleware

        config = MockStreamingAuthConfig(jwt_enabled=True, jwt_secret="")
        middleware = StreamingAuthMiddleware(app=None, config=config)

        scope = {
            "type": "http",
            "headers": [(b"authorization", f"Bearer {valid_token}".encode())],
            "client": ("127.0.0.1", 12345),
        }

        user_info = middleware._extract_user_info(scope)

        # Should fall through to anonymous since no secret configured
        assert user_info["is_authenticated"] is False


class TestJWTLogging:
    """Test that JWT errors are properly logged."""

    @pytest.fixture
    def config(self):
        return MockStreamingAuthConfig(jwt_secret="test-secret-key-minimum-32-chars!")

    def test_invalid_signature_is_logged(self, config, caplog):
        """Invalid JWT signature should log a warning."""
        import logging

        from src.core.middleware.streaming import StreamingAuthMiddleware

        middleware = StreamingAuthMiddleware(app=None, config=config)

        # Create token with wrong secret
        forged = jwt.encode(
            {"sub": "attacker", "exp": datetime.utcnow() + timedelta(hours=1)},
            "wrong-secret",
            algorithm="HS256",
        )

        scope = {
            "type": "http",
            "headers": [(b"authorization", f"Bearer {forged}".encode())],
            "client": ("127.0.0.1", 12345),
        }

        with caplog.at_level(logging.WARNING):
            middleware._extract_user_info(scope)

        # Should have logged a warning about invalid signature
        assert any(
            "signature" in record.message.lower() or "jwt" in record.message.lower()
            for record in caplog.records
        )

    def test_expired_token_is_logged(self, config, caplog):
        """Expired JWT should log appropriately."""
        import logging

        from src.core.middleware.streaming import StreamingAuthMiddleware

        middleware = StreamingAuthMiddleware(app=None, config=config)

        expired = jwt.encode(
            {"sub": "user", "exp": datetime.utcnow() - timedelta(hours=1)},
            config.jwt_secret,
            algorithm="HS256",
        )

        scope = {
            "type": "http",
            "headers": [(b"authorization", f"Bearer {expired}".encode())],
            "client": ("127.0.0.1", 12345),
        }

        with caplog.at_level(logging.WARNING):
            middleware._extract_user_info(scope)

        # Should have logged about expiration
        assert any("expir" in record.message.lower() for record in caplog.records)


class TestStreamingSessionIdValidation:
    def test_session_uuid_must_be_canonical_v4(self):
        from src.core.middleware.streaming import StreamingAuthMiddleware

        assert StreamingAuthMiddleware._is_valid_session_id(
            "550e8400-e29b-41d4-a716-446655440000"
        )
        assert not StreamingAuthMiddleware._is_valid_session_id(
            "550e8400e29b41d4a716446655440000"
        )
        assert not StreamingAuthMiddleware._is_valid_session_id(
            "550e8400-e29b-11d4-a716-446655440000"
        )

    def test_guest_session_allows_uppercase_hex_suffix_only(self):
        from src.core.middleware.streaming import StreamingAuthMiddleware

        assert StreamingAuthMiddleware._is_valid_session_id(
            "guest_550E8400E29B41D4A716446655440000"
        )
        assert not StreamingAuthMiddleware._is_valid_session_id("guest_admin<script>")

    def test_anonymous_id_sanitizer_keeps_header_safe_subset(self):
        from src.core.middleware.streaming import StreamingAuthMiddleware

        assert (
            StreamingAuthMiddleware._sanitize_anon_id("abc'\"\\<>:._-\n")
            == "abc:._-"
        )
