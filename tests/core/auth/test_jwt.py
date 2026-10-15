"""Tests for JWT token decoding and validation."""

from datetime import datetime, timedelta, timezone

import jwt as pyjwt
import pytest

from src.core.auth.jwt import decode_jwt_token
from ai_gateway_core.exceptions import AuthError

SECRET = "test-secret-for-jwt-tests"
ALGORITHMS = ["HS256"]


def _make_token(claims: dict | None = None, secret: str = SECRET, **overrides) -> str:
    payload = {
        "sub": "user_1",
        "user_id": "user_1",
        "exp": datetime.now(timezone.utc) + timedelta(hours=1),
        "iat": datetime.now(timezone.utc),
    }
    if claims:
        payload.update(claims)
    payload.update(overrides)
    return pyjwt.encode(payload, secret, algorithm="HS256")


class TestDecodeJwtToken:
    def test_valid_token(self):
        token = _make_token()
        payload = decode_jwt_token(token, SECRET, ALGORITHMS)
        assert payload["sub"] == "user_1"

    def test_expired_token(self):
        token = _make_token(exp=datetime.now(timezone.utc) - timedelta(hours=1))
        with pytest.raises(AuthError, match="expired"):
            decode_jwt_token(token, SECRET, ALGORITHMS)

    def test_invalid_signature(self):
        token = _make_token(secret="wrong-secret")
        with pytest.raises(AuthError, match="signature"):
            decode_jwt_token(token, SECRET, ALGORITHMS)

    def test_malformed_token(self):
        with pytest.raises(AuthError, match="Malformed|Invalid"):
            decode_jwt_token("not.a.jwt", SECRET, ALGORITHMS)

    def test_audience_valid(self):
        token = _make_token(aud="my-app")
        payload = decode_jwt_token(token, SECRET, ALGORITHMS, audience="my-app")
        assert payload["aud"] == "my-app"

    def test_audience_mismatch(self):
        token = _make_token(aud="my-app")
        with pytest.raises(AuthError, match="audience"):
            decode_jwt_token(token, SECRET, ALGORITHMS, audience="wrong-app")

    def test_issuer_valid(self):
        token = _make_token(iss="auth-server")
        payload = decode_jwt_token(token, SECRET, ALGORITHMS, issuer="auth-server")
        assert payload["iss"] == "auth-server"

    def test_issuer_mismatch(self):
        token = _make_token(iss="auth-server")
        with pytest.raises(AuthError, match="issuer"):
            decode_jwt_token(token, SECRET, ALGORITHMS, issuer="wrong-issuer")

    def test_custom_claims_preserved(self):
        token = _make_token(claims={"tier": "enterprise", "tenant_id": "t1"})
        payload = decode_jwt_token(token, SECRET, ALGORITHMS)
        assert payload["tier"] == "enterprise"
        assert payload["tenant_id"] == "t1"

    def test_no_audience_validation_when_none(self):
        token = _make_token(aud="anything")
        payload = decode_jwt_token(token, SECRET, ALGORITHMS, audience=None)
        assert payload["aud"] == "anything"
