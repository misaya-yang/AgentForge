from __future__ import annotations

from ai_gateway_core.skills import SkillManifest, SkillRegistry, SkillSource


def _manifest(
    *,
    version_id: str,
    instruction: str,
) -> SkillManifest:
    skill_id = version_id.replace("2", "1", 1)
    return SkillManifest(
        name="same-name",
        title="Same Name",
        description="Tenant-scoped helper",
        entrypoint=f"db://{skill_id}/{version_id}",
        instructions=instruction,
        source=SkillSource.USER,
        artifact_type="tenant_instruction",
        skill_id=skill_id,
        version_id=version_id,
        content_hash="a" * 64,
        generated=False,
        enabled=True,
    )


def test_same_named_skill_cache_isolated_by_tenant_user_and_version() -> None:
    registry = SkillRegistry()
    version_a = "21111111-1111-4111-8111-111111111111"
    version_b = "22222222-2222-4222-8222-222222222222"
    registry.register_scoped(
        _manifest(version_id=version_a, instruction="TENANT A ONLY"),
        tenant_id="tenant-a",
        user_id="user-a",
    )
    registry.register_scoped(
        _manifest(version_id=version_b, instruction="TENANT B ONLY"),
        tenant_id="tenant-b",
        user_id="user-b",
    )

    tenant_a = registry.list(
        scope=("tenant-a", "user-a"),
        allowed_names=frozenset({"same-name"}),
        allowed_versions={"same-name": version_a},
    )
    tenant_b = registry.list(
        scope=("tenant-b", "user-b"),
        allowed_names=frozenset({"same-name"}),
        allowed_versions={"same-name": version_b},
    )
    assert [item.instructions for item in tenant_a] == ["TENANT A ONLY"]
    assert [item.instructions for item in tenant_b] == ["TENANT B ONLY"]
    assert registry.list() == []


def test_wrong_user_or_version_cannot_read_scoped_skill() -> None:
    registry = SkillRegistry()
    version = "23333333-3333-4333-8333-333333333333"
    registry.register_scoped(
        _manifest(version_id=version, instruction="PRIVATE"),
        tenant_id="tenant-a",
        user_id="user-a",
    )

    assert registry.get_scoped(
        "same-name",
        tenant_id="tenant-a",
        user_id="user-b",
        version_id=version,
    ) is None
    assert registry.list(
        scope=("tenant-a", "user-a"),
        allowed_versions={"same-name": "24444444-4444-4444-8444-444444444444"},
    ) == []


def test_selection_cannot_expand_beyond_exact_scoped_version() -> None:
    registry = SkillRegistry()
    allowed_version = "25555555-5555-4555-8555-555555555555"
    other_version = "26666666-6666-4666-8666-666666666666"
    registry.register_scoped(
        _manifest(version_id=allowed_version, instruction="ALLOWED"),
        tenant_id="tenant-a",
        user_id="user-a",
    )
    registry.register_scoped(
        _manifest(version_id=other_version, instruction="FORBIDDEN"),
        tenant_id="tenant-a",
        user_id="user-a",
    )

    selected = registry.select_for_query(
        "tenant scoped helper",
        scope=("tenant-a", "user-a"),
        allowed_names=frozenset({"same-name"}),
        allowed_versions={"same-name": allowed_version},
    )
    assert [item.skill.instructions for item in selected] == ["ALLOWED"]
