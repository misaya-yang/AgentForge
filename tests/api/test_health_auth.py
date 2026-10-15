"""
Tests for health endpoint authentication requirements.

Ensures sensitive health information is protected from unauthorized access.
"""

from dataclasses import dataclass
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest


@dataclass
class MockUserContext:
    """Mock UserContext for testing."""

    user_id: str
    tenant_id: str = ""
    tier: str = "normal"
    is_authenticated: bool = False
    ip: str = "127.0.0.1"
    roles: list[str] = None

    def __post_init__(self):
        if self.roles is None:
            self.roles = []


class TestHealthEndpointAuth:
    """Test health endpoint authentication requirements."""

    def test_basic_health_returns_minimal_info(self):
        """Basic /health endpoint should return minimal info (no service count)."""
        # Create async test
        import asyncio

        from src.api.v1.health import gateway_health

        async def run_test():
            mock_registry = AsyncMock()
            mock_registry.list.return_value = [{}, {}, {}]  # 3 services

            result = await gateway_health(registry=mock_registry)

            # Should only return status
            assert "status" in result
            assert result["status"] == "ok"
            # Should NOT expose service count (information disclosure)
            assert "services" not in result

        asyncio.run(run_test())

    def test_services_health_requires_auth(self):
        """Detailed /health/services requires authentication."""
        from fastapi import HTTPException

        from src.api.v1.health import require_admin

        # Unauthenticated user
        mock_user = MockUserContext(user_id="anon:123", is_authenticated=False, roles=[])

        with pytest.raises(HTTPException) as exc_info:
            require_admin(mock_user)

        assert exc_info.value.status_code == 401

    def test_services_health_requires_admin_role(self):
        """Detailed /health/services requires admin role."""
        from fastapi import HTTPException

        from src.api.v1.health import require_admin

        # Authenticated but not admin
        mock_user = MockUserContext(user_id="user123", is_authenticated=True, roles=["user"])

        with pytest.raises(HTTPException) as exc_info:
            require_admin(mock_user)

        assert exc_info.value.status_code == 403

    def test_admin_user_is_allowed(self):
        """Admin user should pass authentication."""
        from src.api.v1.health import require_admin

        # Admin user
        mock_user = MockUserContext(user_id="admin123", is_authenticated=True, roles=["admin"])

        # Should not raise
        result = require_admin(mock_user)
        assert result == mock_user

    def test_ops_role_is_allowed(self):
        """User with 'ops' role should also pass (for operations team)."""
        from src.api.v1.health import require_admin

        # Ops user
        mock_user = MockUserContext(user_id="ops123", is_authenticated=True, roles=["ops"])

        # Should not raise
        result = require_admin(mock_user)
        assert result == mock_user

    def test_providers_health_requires_admin(self):
        """Provider health endpoint requires admin."""
        from fastapi import HTTPException

        from src.api.v1.health import require_admin

        # Regular authenticated user
        mock_user = MockUserContext(
            user_id="user123", is_authenticated=True, roles=["user", "premium"]
        )

        with pytest.raises(HTTPException) as exc_info:
            require_admin(mock_user)

        assert exc_info.value.status_code == 403

    @pytest.mark.asyncio
    async def test_services_health_reports_initialized_agent_runtime(self):
        """The service view reports the configured Runtime control plane."""
        from src.api.v1 import health

        monitor = SimpleNamespace(all_status=lambda: {})
        request = SimpleNamespace(
            app=SimpleNamespace(
                state=SimpleNamespace(agent_runtime_control=object(), image_task_worker=None)
            )
        )
        user = MockUserContext(user_id="admin123", is_authenticated=True, roles=["admin"])

        result = await health.all_services_health(request=request, monitor=monitor, user=user)

        assert result["agent_runtime"]["status"] == "healthy"

    @pytest.mark.asyncio
    async def test_admin_health_exposes_private_core_and_optional_degradation(self):
        """Admin detail reports why capabilities degraded without changing core."""
        from src.api.v1 import health

        async def probe():
            return {
                "status": "ready",
                "core_ready": True,
                "degraded": True,
                "core": {
                    "auth_config": "healthy",
                    "database": "healthy",
                    "redis": "healthy",
                    "agent_runtime": "healthy",
                    "model_plane": "healthy",
                },
                "capabilities": {
                    "knowledge_service": "status_503",
                    "image_worker": "healthy",
                },
            }

        connector = SimpleNamespace(
            status="unhealthy",
            latency=0.1,
            last_check=None,
            error="timeout",
        )
        monitor = SimpleNamespace(all_status=lambda: {"optional_connector": connector})
        request = SimpleNamespace(
            app=SimpleNamespace(state=SimpleNamespace(gateway_health_probe=probe))
        )
        user = MockUserContext(user_id="admin123", is_authenticated=True, roles=["admin"])

        result = await health.all_services_health(request=request, monitor=monitor, user=user)

        assert result["gateway_core"] == {
            "status": "healthy",
            "dependencies": {
                "auth_config": "healthy",
                "database": "healthy",
                "redis": "healthy",
                "agent_runtime": "healthy",
                "model_plane": "healthy",
            },
        }
        assert result["knowledge_service"] == {
            "status": "status_503",
            "required": False,
        }
        assert result["assistant"]["status"] == "degraded"

    @pytest.mark.asyncio
    async def test_non_admin_cannot_trigger_private_dependency_probe(self):
        """Authorization runs before any dependency enumeration or network probe."""
        from fastapi import HTTPException

        from src.api.v1 import health

        called = False

        async def probe():
            nonlocal called
            called = True
            return {}

        request = SimpleNamespace(
            app=SimpleNamespace(state=SimpleNamespace(gateway_health_probe=probe))
        )
        monitor = SimpleNamespace(all_status=lambda: {})
        user = MockUserContext(user_id="user123", is_authenticated=True, roles=["user"])

        with pytest.raises(HTTPException, match="Admin access required"):
            await health.all_services_health(request=request, monitor=monitor, user=user)

        assert called is False


class TestHealthEndpointInfoProtection:
    """Test that sensitive info is not exposed."""

    def test_basic_health_does_not_leak_service_count(self):
        """Basic health should not leak infrastructure details."""
        import asyncio

        from src.api.v1.health import gateway_health

        async def run_test():
            mock_registry = AsyncMock()
            mock_registry.list.return_value = list(range(50))  # 50 services

            result = await gateway_health(registry=mock_registry)

            # Should not expose how many services we have
            assert "services" not in result
            # Should not expose provider info
            assert "providers" not in result
            # Should only have status
            assert list(result.keys()) == ["status"]

        asyncio.run(run_test())
