"""
Built-in Skill: skill_create — AI-assisted skill creation.

Helps users create new SKILL.md files through conversation and prepares them
for review. Generated skills must not be registered or enabled until review,
eval, and rollback evidence is present.
"""

from __future__ import annotations

import logging
from typing import Any

from ..models import SkillManifest, SkillSource

logger = logging.getLogger(__name__)

SKILL_CREATE_MANIFEST = SkillManifest(
    name="skill-create",
    title="Skill Creator",
    description="Create, edit, and manage custom skills for this AI assistant",
    entrypoint="builtin://skill_create",
    summary="Help users create new skills through conversation",
    version="1.0.0",
    tags=["meta", "creation", "management"],
    permissions=["skill:create", "skill:edit", "llm:generate"],
    source=SkillSource.BUILTIN,
    instructions="""# Skill Creator

## Instructions
Help the user create a new skill by gathering requirements and generating a
reviewable SKILL.md file. Follow the propose-review-test-enable loop.

### Workflow
1. Ask what the skill should do (purpose, trigger scenarios)
2. Ask what tools/permissions it needs (kb:read, llm:generate, web:search, etc.)
3. Ask for example inputs/outputs
4. Generate the SKILL.md with proper YAML frontmatter + markdown instructions
5. Mark generated skills as proposed and disabled by default
6. Validate the manifest (check for dangerous permissions)
7. Require independent critic evidence, eval evidence, and rollback metadata
8. Enable only after review approves the exact manifest version

### Validation Rules
- name: kebab-case, 3-50 chars, unique
- permissions: no os:*, exec:*, filesystem:write*
- instructions: < 5000 tokens
- config values: must be JSON-serializable
- generated skills must include generated: true, enabled: false, and
  lifecycle_status: proposed until activation gates pass

### Output
Return the complete proposed SKILL.md content that the user can review. Do not
register or enable the skill during creation.
""",
)


async def handle_skill_create(args: dict[str, Any], skill: SkillManifest) -> dict[str, Any]:
    """Handle the skill_create builtin — returns instructions for the LLM to follow."""
    user_input = args.get("input", "")

    return {
        "success": True,
        "result": (
            f"The user wants to create a skill: {user_input}\n\n"
            "Follow the Skill Creator propose-review-test-enable workflow:\n"
            "1. Ask clarifying questions about the skill's purpose and triggers\n"
            "2. Determine required permissions\n"
            "3. Generate a complete proposed SKILL.md file with generated: true, "
            "enabled: false, and lifecycle_status: proposed\n"
            "4. Present it to the user for review\n"
            "5. Do not register or enable it until independent critic evidence, "
            "eval evidence, and rollback metadata are recorded\n\n"
            "Output the proposed SKILL.md in a code block when ready."
        ),
        "type": "skill_instructions",
    }
