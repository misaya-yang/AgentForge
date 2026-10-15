from __future__ import annotations

from src.proxy.context_injector import ContextInjector, RequestContext


def _build_headers(
    original_headers: dict[str, str],
    *,
    user_id: str = "user-auth",
    tenant_id: str = "tenant-auth",
    is_authenticated: bool = True,
    roles: list[str] | None = None,
) -> dict[str, str]:
    injector = ContextInjector(
        inject_user_info=True,
        inject_request_info=True,
        forward_all_headers=True,
    )
    context = RequestContext(
        user_id=user_id,
        tenant_id=tenant_id,
        user_tier="premium" if is_authenticated else "anonymous",
        is_authenticated=is_authenticated,
        roles=roles or ["user"],
        original_headers=original_headers,
    )
    return injector.build_headers(context)


def test_client_user_id_header_is_overwritten_by_auth_context():
    headers = _build_headers(
        {
            "X-User-Id": "admin",
            "X-GW-User-ID": "root",
            "Content-Type": "application/json",
        }
    )

    assert headers["X-User-Id"] == "user-auth"
    assert headers["X-GW-User-ID"] == "user-auth"
    assert "admin" not in headers.values()
    assert "root" not in headers.values()


def test_client_tenant_header_is_overwritten_by_auth_context():
    headers = _build_headers(
        {
            "X-Tenant-Id": "tenant-other",
            "X-GW-Tenant-ID": "tenant-root",
        }
    )

    assert headers["X-Tenant-Id"] == "tenant-auth"
    assert headers["X-GW-Tenant-ID"] == "tenant-auth"
    assert "tenant-other" not in headers.values()
    assert "tenant-root" not in headers.values()


def test_client_authenticated_header_cannot_upgrade_anonymous_context():
    headers = _build_headers(
        {
            "X-GW-Authenticated": "true",
            "X-User-Type": "user",
            "X-User-Permissions": "read,write,admin,delete",
        },
        user_id="",
        tenant_id="",
        is_authenticated=False,
        roles=[],
    )

    assert headers["X-GW-Authenticated"] == "false"
    assert headers["X-User-Type"] == "anonymous"
    assert headers["X-User-Permissions"] == "read"


def test_transparent_forwarding_strips_gateway_prefixed_identity_headers():
    headers = _build_headers(
        {
            "X-GW-User-Type": "admin",
            "X-GW-User-Name": "Root",
            "X-GW-User-Permissions": "admin,delete",
            "X-GW-User-Tier": "admin",
            "X-Custom-Trace": "kept",
        }
    )

    assert headers["X-Custom-Trace"] == "kept"
    assert "X-GW-User-Type" not in headers
    assert "X-GW-User-Name" not in headers
    assert "X-GW-User-Permissions" not in headers
    assert headers["X-GW-User-Tier"] == "premium"


def test_langgraph_and_gateway_identity_headers_are_both_authoritative():
    headers = _build_headers(
        {
            "X-User-Id": "spoofed-user",
            "X-GW-User-ID": "spoofed-gateway-user",
            "X-Tenant-Id": "spoofed-tenant",
            "X-GW-Tenant-ID": "spoofed-gateway-tenant",
            "X-User-Tier": "admin",
            "X-GW-User-Tier": "admin",
            "X-GW-Authenticated": "false",
        },
        roles=["developer"],
    )

    assert headers["X-User-Id"] == "user-auth"
    assert headers["X-GW-User-ID"] == "user-auth"
    assert headers["X-Tenant-Id"] == "tenant-auth"
    assert headers["X-GW-Tenant-ID"] == "tenant-auth"
    assert headers["X-User-Tier"] == "premium"
    assert headers["X-GW-User-Tier"] == "premium"
    assert headers["X-GW-Authenticated"] == "true"


def test_service_auth_token_rejects_arbitrary_header_injection():
    injector = ContextInjector(inject_user_info=False, inject_request_info=False)
    context = RequestContext()

    try:
        injector.build_headers(context, service_auth_token="X-Evil: injected")
    except ValueError as exc:
        assert "service_auth_token" in str(exc)
    else:
        raise AssertionError("arbitrary service_auth_token header injection was accepted")


def test_service_auth_token_rejects_crlf_header_injection():
    injector = ContextInjector(inject_user_info=False, inject_request_info=False)
    context = RequestContext()

    try:
        injector.build_headers(context, service_auth_token="Bearer safe\r\nX-Evil: injected")
    except ValueError as exc:
        assert "service_auth_token" in str(exc)
    else:
        raise AssertionError("CRLF service_auth_token header injection was accepted")


def test_client_forwarded_ip_headers_are_rebuilt_from_context():
    injector = ContextInjector(
        inject_user_info=False,
        inject_request_info=False,
        forward_all_headers=True,
    )
    context = RequestContext(
        client_ip="203.0.113.9",
        original_headers={
            "X-Forwarded-For": "198.51.100.1",
            "X-Real-IP": "198.51.100.2",
            "X-Custom-Trace": "kept",
        },
    )

    headers = injector.build_headers(context)

    assert headers["X-Forwarded-For"] == "203.0.113.9"
    assert "X-Real-IP" not in headers
    assert headers["X-Custom-Trace"] == "kept"
