from __future__ import annotations

from ai_gateway_core.skills.artifact_repository import DatabaseSkillArtifactRepository


def test_skill_artifact_queries_use_authoritative_assistant_schema() -> None:
    query = DatabaseSkillArtifactRepository._artifact_select()

    assert "FROM assistant.assistant_skill_versions AS version" in query
    assert "JOIN assistant.assistant_skills AS skill" in query
    assert "FROM assistant.assistant_skill_version_revocations revoked" in query
