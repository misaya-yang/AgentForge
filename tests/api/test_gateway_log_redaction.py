from __future__ import annotations

from src.proxy.billing_interceptor import StreamProcessor
from src.proxy.transparent_proxy import TransparentProxy
from src.services.metrics.redaction import redact_sensitive_data, redact_sensitive_text


def test_redaction_hides_common_secret_keys_and_headers():
    payload = {
        "api_key": "sk-test-secret",
        "_api_key": "runtime-secret",
        "Authorization": "Bearer bearer-secret",
        "Cookie": "sid=cookie-secret",
        "Set-Cookie": "sid=set-cookie-secret",
        "password": "password-secret",
        "auth_token": "token-secret",
        "api_key_fingerprint": "fp-safe",
    }

    redacted = redact_sensitive_data(payload)

    assert "sk-test-secret" not in str(redacted)
    assert "runtime-secret" not in str(redacted)
    assert "bearer-secret" not in str(redacted)
    assert "cookie-secret" not in str(redacted)
    assert "set-cookie-secret" not in str(redacted)
    assert "password-secret" not in str(redacted)
    assert "token-secret" not in str(redacted)
    assert redacted["api_key_fingerprint"] == "fp-safe"


def test_redact_sensitive_text_masks_json_like_error_payloads():
    message = (
        '{"error":{"message":"failed", "_api_key":"runtime-secret", '
        '"Authorization":"Bearer bearer-secret", "api_key_fingerprint":"fp-safe"}}'
    )

    scrubbed = redact_sensitive_text(message)

    assert "runtime-secret" not in scrubbed
    assert "bearer-secret" not in scrubbed
    assert '"api_key_fingerprint":"fp-safe"' in scrubbed


def test_proxy_error_metadata_does_not_leak_upstream_secrets():
    metadata = TransparentProxy._extract_error_metadata(
        {
            "__error__": {
                "type": "UpstreamError",
                "message": '{"_api_key":"runtime-secret","password":"secret"}',
            }
        }
    )

    assert "runtime-secret" not in str(metadata)
    assert "password\":\"secret" not in str(metadata)
    assert metadata["upstream_error_type"] == "UpstreamError"


def test_stream_error_metadata_does_not_leak_upstream_secrets():
    metadata = StreamProcessor._extract_error_payload(
        {
            "error": {
                "type": "ProviderError",
                "message": '{"auth_token":"token-secret","Cookie":"sid=secret"}',
            }
        }
    )

    assert "token-secret" not in str(metadata)
    assert "sid=secret" not in str(metadata)
    assert metadata["upstream_error_type"] == "ProviderError"
