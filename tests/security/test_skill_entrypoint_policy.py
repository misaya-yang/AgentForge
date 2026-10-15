from __future__ import annotations

from dataclasses import replace

import pytest
from ai_gateway_core.skills import (
    SkillExecutor,
    UserSkillPolicyError,
    parse_user_skill_md,
)


def _content(field: str = "", *, name: str = "safe-helper") -> str:
    return f"""---
name: {name}
title: Safe Helper
description: Instruction-only helper
generated: false
enabled: true
{field}---
# Instructions
Only return a bounded answer.
"""


@pytest.mark.parametrize(
    ("declaration", "field"),
    [
        ("entrypoint: builtin://shell\n", "entrypoint"),
        ("entrypoint: file:///tmp/run.py\n", "entrypoint"),
        ("entrypoint: https://evil.example/run\n", "entrypoint"),
        ("source: builtin\n", "source"),
        ("path: /tmp/run.py\n", "path"),
        ("command: python run.py\n", "command"),
        ("script: run.py\n", "script"),
        ("module: tenant.module\n", "module"),
        ("handler: execute\n", "handler"),
        ("url: https://evil.example/skill\n", "url"),
        ("executable: true\n", "executable"),
        ("artifact_type: bundled\n", "artifact_type"),
    ],
)
def test_tenant_upload_rejects_source_entrypoint_and_exec_aliases(
    declaration: str,
    field: str,
) -> None:
    with pytest.raises(UserSkillPolicyError) as error:
        parse_user_skill_md(_content(declaration))

    assert error.value.field == field
    assert error.value.code == "SKILL_ARTIFACT_FIELD_FORBIDDEN"


def test_platform_skill_name_cannot_be_overridden() -> None:
    with pytest.raises(UserSkillPolicyError) as error:
        parse_user_skill_md(
            _content(name="skill-create"),
            reserved_names=frozenset({"skill-create"}),
        )
    assert error.value.code == "SKILL_NAME_RESERVED"


@pytest.mark.asyncio
async def test_valid_upload_normalizes_to_exact_non_executable_db_entrypoint() -> None:
    normalized, manifest = parse_user_skill_md(_content())
    manifest = replace(
        manifest,
        skill_id="11111111-1111-4111-8111-111111111111",
        version_id="22222222-2222-4222-8222-222222222222",
        entrypoint=(
            "db://11111111-1111-4111-8111-111111111111/"
            "22222222-2222-4222-8222-222222222222"
        ),
        content_hash="a" * 64,
    )

    result = await SkillExecutor().execute(manifest, {"input": "test"})

    assert normalized.endswith("\n")
    assert manifest.source.value == "user"
    assert manifest.artifact_type == "tenant_instruction"
    assert result == {
        "success": True,
        "result": "# Instructions\nOnly return a bounded answer.",
        "type": "skill_instructions",
    }


@pytest.mark.asyncio
async def test_malformed_db_identity_fails_closed() -> None:
    _normalized, manifest = parse_user_skill_md(_content())
    forged = replace(
        manifest,
        skill_id="11111111-1111-4111-8111-111111111111",
        version_id="22222222-2222-4222-8222-222222222222",
        entrypoint="db://other/version",
    )
    result = await SkillExecutor().execute(forged, {"input": "test"})
    assert result["success"] is False
    assert result["error"] == "Skill artifact identity is invalid"
