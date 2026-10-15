from __future__ import annotations

import hashlib
import inspect
from types import SimpleNamespace

import pytest
from ai_gateway_core.agents import spec as extracted_spec
from ai_gateway_core.persistence.repositories import agent_repository
from ai_gateway_core.persistence.repositories.base import BaseRepository

_LEGACY_SPEC_EXPORTS = {
    "AGENT_SPEC_SCHEMA_VERSION",
    "agent_spec_safety_errors",
    "canonical_spec",
    "hash_agent_spec",
    "redact_agent_spec_for_read",
    "sanitize_agent_copy_spec",
    "unsafe_agent_spec_paths",
    "validate_agent_spec",
}

_REPOSITORY_ERROR_TYPES = (
    "AgentRepositoryError",
    "AgentNotFoundError",
    "AgentDraftConflictError",
    "AgentLastOwnerError",
    "AgentPrincipalNotFoundError",
    "AgentValidationError",
    "AgentArchivedError",
    "AgentRuntimeUnavailableError",
    "AgentReleaseEvaluationNotFoundError",
    "AgentReleaseEvaluationStaleError",
    "AgentReleaseEvaluationTerminalError",
    "AgentReleaseGateError",
    "AgentReleaseIdempotencyConflictError",
    "AgentPublicationNotFoundError",
)


def test_agent_spec_extraction_preserves_legacy_imports_errors_and_mro() -> None:
    assert set(agent_repository.__all__) >= _LEGACY_SPEC_EXPORTS
    assert agent_repository.AGENT_SPEC_SCHEMA_VERSION == extracted_spec.AGENT_SPEC_SCHEMA_VERSION
    assert agent_repository.DatabaseAgentRepository.__bases__ == (BaseRepository,)

    for name in _LEGACY_SPEC_EXPORTS - {"AGENT_SPEC_SCHEMA_VERSION"}:
        legacy = getattr(agent_repository, name)
        extracted = getattr(extracted_spec, name)
        assert inspect.signature(legacy) == inspect.signature(extracted)
        assert legacy.__module__ == agent_repository.__name__

    for name in _REPOSITORY_ERROR_TYPES:
        error_type = getattr(agent_repository, name)
        assert error_type.__module__ == agent_repository.__name__


def test_legacy_hash_keeps_canonical_spec_monkeypatch_seam(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(agent_repository, "canonical_spec", lambda _spec: "patched-canonical")

    expected = hashlib.sha256(b"patched-canonical").hexdigest()
    assert agent_repository.hash_agent_spec({}) == expected


@pytest.mark.asyncio
async def test_repository_keeps_validate_agent_spec_monkeypatch_seam(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    repository = agent_repository.DatabaseAgentRepository(
        SimpleNamespace(enabled=False, _pool=None)
    )
    draft = {"revision": 7, "spec_hash": "hash-7", "spec": {"schema_version": "test"}}
    expected_errors = [
        {"field": "spec", "code": "PATCHED_VALIDATOR", "message": "patched"}
    ]

    async def get_draft(**_kwargs):
        return draft

    def validate(spec):
        assert spec is draft["spec"]
        return expected_errors

    monkeypatch.setattr(repository, "get_draft", get_draft)
    monkeypatch.setattr(agent_repository, "validate_agent_spec", validate)

    result = await repository.validate_draft(
        tenant_id="tenant-1",
        agent_id="agent-1",
        user_id="user-1",
        is_tenant_admin=False,
    )

    assert result == {
        "valid": False,
        "revision": 7,
        "spec_hash": "hash-7",
        "errors": expected_errors,
    }
