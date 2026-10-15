from __future__ import annotations

from types import SimpleNamespace

import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient

from src.api import deps
from src.api.v1 import assistant as assistant_routes
from src.core.auth.user_resolver import UserContext


def _client(
    *,
    model_rows: list[dict] | None = None,
    tier: str = "normal",
    user_id: str = "user-1",
    tenant_id: str = "tenant-1",
) -> TestClient:
    app = FastAPI()
    app.include_router(assistant_routes.router)
    app.dependency_overrides[deps.get_user_context] = lambda: UserContext(
        user_id=user_id,
        tenant_id=tenant_id,
        tier=tier,
        roles=[],
        is_authenticated=True,
    )
    app.state.model_service = _ModelService(model_rows or [])
    app.state.model_meta = _ModelMeta()
    app.state.settings = SimpleNamespace(default_model="missing-default")
    app.state.kb_proxy = _KnowledgeProxy([])
    return TestClient(app)


class _ModelService:
    def __init__(self, rows: list[dict]):
        self.rows = rows
        self.calls: list[dict] = []

    async def list_models(self, **kwargs):
        self.calls.append(kwargs)
        return self.rows


class _ModelMeta:
    def __init__(self, configured: set[str] | None = None):
        self.configured = configured or set()
        self.calls: list[tuple[str, str]] = []

    async def is_provider_configured(self, tenant_id: str, provider_id: str) -> bool:
        self.calls.append((tenant_id, provider_id))
        return provider_id in self.configured


class _KnowledgeProxy:
    def __init__(self, datasets: list[dict], error: Exception | None = None):
        self.datasets = datasets
        self.error = error
        self.calls: list[UserContext] = []

    async def list_datasets(self, user):
        self.calls.append(user)
        if self.error:
            raise self.error
        return self.datasets


class _TenantPolicyDatabase:
    def __init__(self, row: dict):
        self.row = row
        self.tenants: list[str] = []

    async def fetchrow(self, _query: str, tenant_id: str):
        self.tenants.append(tenant_id)
        return self.row


class _UnavailableTenantPolicyDatabase:
    enabled = False
    _pool = None

    async def fetchrow(self, _query: str, _tenant_id: str):
        raise AssertionError("disabled database must not be queried")


class _PoollessTenantPolicyDatabase:
    enabled = True
    _pool = None

    async def fetchrow(self, _query: str, _tenant_id: str):
        raise AssertionError("poolless database must not be queried")


class _RaisingTenantPolicyDatabase:
    enabled = True
    _pool = object()

    async def fetchrow(self, _query: str, _tenant_id: str):
        raise RuntimeError("database connection lost")


def _rows() -> list[dict]:
    return [
        {
            "model_id": "premium-model",
            "display_name": "Premium",
            "provider_id": "qwen",
            "access_level": "premium",
            "is_enabled": True,
            "sort_order": 1,
            "context_window": 128000,
            "max_output_tokens": 8192,
            "effective_capabilities": {"reasoning": {"options": ["low"]}},
            "capability_revision": 7,
        },
        {
            "model_id": "public-z",
            "display_name": "Public Z",
            "provider_id": "anthropic",
            "access_level": "public",
            "is_enabled": True,
            "sort_order": 2,
            "context_window": 200000,
            "max_output_tokens": 8192,
            "capability_revision": 1,
        },
        {
            "model_id": "disabled",
            "display_name": "Disabled",
            "provider_id": "qwen",
            "access_level": "public",
            "is_enabled": False,
        },
        {
            "model_id": "unverified-state",
            "display_name": "Unverified state",
            "provider_id": "qwen",
            "access_level": "public",
            "context_window": 128000,
            "max_output_tokens": 8192,
            "capability_revision": 1,
        },
        {
            "model_id": "corrupt-access",
            "display_name": "Corrupt",
            "provider_id": "qwen",
            "access_level": "not-a-level",
            "is_enabled": True,
        },
        {
            "model_id": "corrupt-shape",
            "display_name": "Corrupt shape",
            "provider_id": "qwen",
            "access_level": "public",
            "is_enabled": True,
            "context_window": "not-an-integer",
            "max_output_tokens": 8192,
            "capability_revision": 1,
        },
        {
            "model_id": "other-tenant",
            "display_name": "Other tenant",
            "provider_id": "other",
            "tenant_id": "tenant-2",
            "access_level": "public",
            "is_enabled": True,
        },
    ]


def test_models_are_gateway_owned_and_fail_closed_for_access() -> None:
    with _client(model_rows=_rows()) as client:
        response = client.get("/assistant/models")
        model_service = client.app.state.model_service

    assert response.status_code == 200
    assert [model["id"] for model in response.json()["models"]] == ["public-z"]
    assert response.json()["models"][0]["effective_capabilities"] == {}
    assert model_service.calls == [{"tenant_id": "tenant-1", "include_disabled": False}]


def test_admin_sees_known_restricted_models_but_not_corrupt_access() -> None:
    with _client(model_rows=_rows(), tier="admin") as client:
        response = client.get("/assistant/models")

    assert response.status_code == 200
    assert [model["id"] for model in response.json()["models"]] == [
        "public-z",
        "premium-model",
    ]


def test_datasets_preserve_stats_and_pass_user_to_kb_proxy() -> None:
    with _client() as client:
        client.app.state.kb_proxy = _KnowledgeProxy(
            [
                {
                    "dataset_id": "ds-1",
                    "name": "Knowledge",
                    "statistics": {"document_count": 3, "segment_count": 9},
                    "embedding_model": "multimodal-embedding-v1",
                }
            ]
        )
        response = client.get("/assistant/datasets")
        proxy = client.app.state.kb_proxy

    assert response.status_code == 200
    assert response.json()["datasets"] == [
        {
            "dataset_id": "ds-1",
            "name": "Knowledge",
            "description": None,
            "document_count": 3,
            "chunk_count": 9,
            "embedding_model": "multimodal-embedding-v1",
            "is_multimodal": True,
        }
    ]
    assert len(proxy.calls) == 1
    assert proxy.calls[0].tenant_id == "tenant-1"


def test_datasets_downstream_failure_is_stable_503() -> None:
    with _client() as client:
        client.app.state.kb_proxy = _KnowledgeProxy([], error=RuntimeError("downstream offline"))
        response = client.get("/assistant/datasets")

    assert response.status_code == 503
    assert response.json() == {"detail": "Knowledge service is unavailable"}


def test_tools_are_gateway_owned_and_do_not_call_assistant_proxy() -> None:
    with _client() as client:
        response = client.get("/assistant/tools")

    assert response.status_code == 200
    tools = response.json()["tools"]
    assert tools
    assert {"name", "description", "category", "risk_level"} <= set(tools[0])


def test_tools_apply_tenant_policy_without_cross_tenant_leak() -> None:
    with _client(tenant_id="tenant-a") as client:
        client.app.state.database = _TenantPolicyDatabase(
            {
                "allowed_tools": ["search_knowledge_base"],
                "blocked_tools": [],
                "allowed_categories": [],
            }
        )
        response = client.get("/assistant/tools")

    assert response.status_code == 200
    assert [tool["name"] for tool in response.json()["tools"]] == ["search_knowledge_base"]
    assert client.app.state.database.tenants == ["tenant-a"]


def test_tools_apply_tenant_category_policy() -> None:
    with _client(tenant_id="tenant-a") as client:
        client.app.state.database = _TenantPolicyDatabase(
            {"allowed_tools": [], "blocked_tools": [], "allowed_categories": ["retrieval"]}
        )
        response = client.get("/assistant/tools")

    assert response.status_code == 200
    assert [tool["name"] for tool in response.json()["tools"]] == [
        "confluence_read",
        "read_tool_artifact",
        "search_knowledge_base",
        "web_fetch",
        "search_web",
        "read_attachment",
    ]


def test_tools_project_source_metadata_and_real_categories() -> None:
    from src.core.assistant_capability_catalog import project_assistant_tools

    tools = project_assistant_tools(
        UserContext(
            user_id="user-1",
            tenant_id="tenant-1",
            tier="normal",
            roles=[],
            is_authenticated=True,
        ),
        tenant_policy={"allowed_categories": ["retrieval"]},
    )
    by_name = {tool["name"]: tool for tool in tools}
    assert by_name["search_knowledge_base"]["category"] == "retrieval"
    assert by_name["search_knowledge_base"]["when_to_use"].startswith("Use this tool")
    assert by_name["web_fetch"]["when_not_to_use"].startswith("Do not use")
    assert "generate_image" not in by_name


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("category", "kb"),
        ("required_permissions", ["permission with spaces"]),
        ("requires_confirmation", "yes"),
        ("when_to_use", "\u0000"),
    ],
)
def test_tools_fail_closed_on_corrupt_catalog_metadata(field: str, value: object) -> None:
    from src.core import assistant_capability_catalog

    _, records = assistant_capability_catalog.load_assistant_capability_catalog()
    record = dict(records[0])
    record[field] = value
    with pytest.raises(assistant_capability_catalog.AssistantCapabilityCatalogError):
        assistant_capability_catalog._validate_record(record)


def test_required_permission_lattice_matches_legacy_registry() -> None:
    from src.core.assistant_capability_catalog import _user_has_required_permissions

    normal = UserContext(
        user_id="u",
        tenant_id="t",
        tier="normal",
        roles=["reader"],
        is_authenticated=True,
    )
    admin = UserContext(
        user_id="a",
        tenant_id="t",
        tier="normal",
        roles=["admin"],
        is_authenticated=True,
    )
    assert _user_has_required_permissions(normal, ["role:reader"])
    assert not _user_has_required_permissions(normal, ["tier:premium"])
    assert _user_has_required_permissions(admin, ["write:anything"])
    assert not _user_has_required_permissions(normal, ["tier:untrusted"])


@pytest.mark.parametrize(
    "value",
    [
        {"high_risk_tools": ["same", "same"], "medium_risk_tools": []},
        {"high_risk_tools": ["bad name"], "medium_risk_tools": []},
        {"high_risk_tools": [], "medium_risk_tools": ["same", "same"]},
        {"high_risk_tools": [], "medium_risk_tools": ["bad/name"]},
    ],
)
def test_gateway_policy_metadata_rejects_duplicate_or_invalid_names(value: dict) -> None:
    from src.core import assistant_capability_catalog

    with pytest.raises(assistant_capability_catalog.AssistantCapabilityCatalogError):
        assistant_capability_catalog._validate_gateway_policy(value)


def test_tools_fail_closed_when_catalog_is_corrupt(
    monkeypatch: pytest.MonkeyPatch, tmp_path
) -> None:
    from src.core import assistant_capability_catalog

    path = tmp_path / "catalog.json"
    path.write_text('{"schema_version":"wrong"}', encoding="utf-8")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_CATALOG_PATH", str(path))
    assistant_capability_catalog.clear_assistant_capability_catalog_cache()
    try:
        with _client() as client:
            response = client.get("/assistant/tools")
    finally:
        assistant_capability_catalog.clear_assistant_capability_catalog_cache()

    assert response.status_code == 503
    assert response.json() == {"detail": "Assistant tool catalog is unavailable"}


def test_policies_are_gateway_owned_and_tenant_scoped() -> None:
    with _client(tenant_id="tenant-a") as client:
        client.app.state.database = _TenantPolicyDatabase(
            {"allowed_tools": [], "blocked_tools": ["web_fetch"], "allowed_categories": []}
        )
        response = client.get("/assistant/policies")

    assert response.status_code == 200
    assert response.json()["policies"]["blocked_tools"] == ["web_fetch"]
    assert response.json()["policies"]["default_execution_profile"] == "safe"
    assert response.json()["policies"]["supported_execution_profiles"] == ["safe"]
    assert response.json()["policies"]["high_risk_tools"] == [
        "system_run_lite",
        "browser_action_lite",
    ]
    assert response.json()["policies"]["medium_risk_tools"] == [
        "execute_python_code",
        "confluence_write",
    ]
    assert client.app.state.database.tenants == ["tenant-a"]


@pytest.mark.parametrize(
    "database",
    [
        _UnavailableTenantPolicyDatabase(),
        _PoollessTenantPolicyDatabase(),
        _RaisingTenantPolicyDatabase(),
    ],
)
def test_policies_fail_closed_when_tenant_policy_storage_is_unavailable(database) -> None:
    with _client() as client:
        client.app.state.database = database
        response = client.get("/assistant/policies")

    assert response.status_code == 503
    assert response.json() == {"detail": "Assistant policy is unavailable"}


def test_tools_require_authentication() -> None:
    app = FastAPI()
    app.include_router(assistant_routes.router)
    app.dependency_overrides[deps.get_user_context] = lambda: UserContext(
        user_id="anonymous", tenant_id="public", is_authenticated=False
    )
    with TestClient(app) as client:
        response = client.get("/assistant/tools")
    assert response.status_code == 401


def test_config_reuses_visible_models_and_falls_back_to_first_visible() -> None:
    with _client(model_rows=_rows()) as client:
        client.app.state.model_meta = _ModelMeta(configured={"anthropic", "qwen"})
        client.app.state.assistant_capability_catalog_getter = lambda: [
            {"name": "search"},
            {"name": "search"},
            {"name": "describe"},
        ]
        response = client.get("/assistant/config")
        model_service = client.app.state.model_service
        model_meta = client.app.state.model_meta

    assert response.status_code == 200
    body = response.json()
    assert body["default_model_id"] == "public-z"
    assert body["available_providers"] == ["anthropic"]
    assert body["kb_enabled"] is True
    assert body["web_search_enabled"] is True
    assert body["tools_available"] == ["search", "describe"]
    assert len(model_service.calls) == 1
    assert model_meta.calls == [("tenant-1", "anthropic")]
