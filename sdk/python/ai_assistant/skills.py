"""Tenant-scoped instruction skill discovery and explicit instruction tests."""
from __future__ import annotations

import re
from typing import TYPE_CHECKING, Any

from ai_assistant.exceptions import AssistantError

if TYPE_CHECKING:
    from ai_assistant.transport.http import HTTPTransport


class SkillModule:
    """Instruction artifacts, distinct from the runtime capability catalog.

    Authorization, enabled-state filtering and lifecycle admission remain owned
    by the Gateway. Testing returns the Gateway result without executing it.
    """

    _BASE = "/api/v1/skills"

    def __init__(self, transport: HTTPTransport) -> None:
        self._transport = transport

    @staticmethod
    def _name(name: str) -> str:
        if not isinstance(name, str) or re.fullmatch(r"[a-z][a-z0-9-]{2,49}", name) is None:
            raise ValueError("Skill name must be kebab-case, 3-50 characters")
        return name

    @staticmethod
    def _record(value: Any) -> dict[str, Any]:
        if not isinstance(value, dict):
            raise ValueError("Invalid skill record")
        SkillModule._name(value.get("name"))
        return value

    async def list(self, *, enabled_only: bool = True) -> dict[str, Any]:
        """Return the full ``skills``/``total`` envelope, including server metadata."""
        if type(enabled_only) is not bool:
            raise ValueError("enabled_only must be a boolean")
        response = await self._transport.request(
            "GET", self._BASE, params={"enabled_only": str(enabled_only).lower()},
        )
        try:
            value = response.json()
            if (
                not isinstance(value, dict) or not isinstance(value.get("skills"), list)
                or type(value.get("total")) is not int or value["total"] < 0
            ):
                raise ValueError("Invalid skill catalog")
            for row in value["skills"]:
                self._record(row)
            return value
        except ValueError as exc:
            raise AssistantError("Invalid skill catalog response") from exc

    async def get(self, name: str) -> dict[str, Any]:
        """Read exact skill detail, retaining instructions and lifecycle fields."""
        name = self._name(name)
        response = await self._transport.request("GET", f"{self._BASE}/{name}")
        try:
            value = self._record(response.json())
            if value["name"] != name:
                raise ValueError("Mismatched skill identity")
            return value
        except ValueError as exc:
            raise AssistantError("Invalid skill detail response") from exc

    async def test(self, name: str, *, input: str) -> dict[str, Any]:  # noqa: A002
        """Explicitly request an instruction test once; preserve failure results.

        This does not enable the skill or execute returned instructions. It is a
        POST with no automatic replay, including after an inconclusive response.
        """
        name = self._name(name)
        if not isinstance(input, str):
            raise ValueError("Skill test input must be a string")
        response = await self._transport.request(
            "POST", f"{self._BASE}/{name}/test", json={"input": input},
        )
        try:
            value = response.json()
            if not isinstance(value, dict) or type(value.get("success")) is not bool:
                raise ValueError("Invalid skill test result")
            return value
        except ValueError as exc:
            raise AssistantError(
                "Invalid skill test response; do not automatically repeat"
            ) from exc
