#!/usr/bin/env python3
"""Collect, validate, and merge real-provider cross-domain agent receipts.

The three commands intentionally have separate trust boundaries:

* ``collect`` sends only the scenario prompt to the configured Gateway and
  records observable SSE lifecycle facts.  It never evaluates answer quality.
* ``validate`` applies scenario-owned deterministic goldens to the collected
  answer and seals the result with mandatory, independent HMAC-SHA256.
* ``merge`` verifies every digest/signature before combining observations and
  golden results.  It never invents candidate scores or hard-gate verdicts.

Authentication, independent collector/golden HMAC keys, an operator-issued
64-hex suite nonce, and an independently hashed runtime attestation are
environment-only inputs. Output files are mode 0600; credentials are not logged.
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import json
import math
import os
import re
import secrets
import stat
import sys
import uuid
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

import httpx

from scripts.eval_fixtures.coding_host_test_receipt import (
    HMAC_ENVIRONMENT_NAME as CODING_HOST_TEST_HMAC_ENVIRONMENT_NAME,
)
from scripts.eval_fixtures.coding_host_test_receipt import (
    SCENARIO_ID as CODING_SCENARIO_ID,
)
from scripts.eval_fixtures.coding_host_test_receipt import (
    CodingHostReceiptError,
)
from scripts.eval_fixtures.coding_host_test_receipt import (
    verify_receipt as verify_coding_host_test_receipt,
)

SCENARIO_SCHEMA = "real-agent-scenarios/v1"
OBSERVATION_SCHEMA = "real-agent-observations/v1"
RAW_SSE_SCHEMA = "real-agent-raw-sse/v1"
VALIDATION_SCHEMA = "real-agent-golden-validation/v1"
MERGED_SCHEMA = "real-agent-validated-receipts/v1"
VALIDATOR_VERSION = "real-domain-golden/v1"

TERMINAL_EVENTS = frozenset({"run_finished", "run_error", "run_cancelled", "run_blocked"})
TEXT_EVENTS = frozenset({"text_delta", "text_message_content"})
TOOL_START_EVENTS = frozenset({"tool_call_start", "tool_call_started"})
TOOL_RESULT_EVENTS = frozenset({"tool_call_result", "tool_call_completed", "tool_result"})
TOOL_END_EVENTS = frozenset({"tool_call_end"})
MIN_PARALLEL_OVERLAP_MS = 25.0
SIDE_EFFECT_FREE_PARENT_TOOLS = frozenset({"spawn_subagent"})
SUPPORTED_ASSERTIONS = frozenset(
    {
        "json_equals",
        "json_number",
        "json_set_equals",
        "json_contains_all",
        "json_excludes_all",
        "json_keys_equals",
        "json_nonempty_string",
        "text_contains_all",
        "text_excludes_all",
    }
)
_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}$")
_PLUGIN_COMPONENT_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
_SHA_RE = re.compile(r"^[a-f0-9]{64}$")
_SELF_ASSESSMENT_KEYS = frozenset(
    {
        "score",
        "scores",
        "passed",
        "pass",
        "candidate_scores",
        "hard_gates",
        "hard_violations",
        "judge_score",
        "evaluation_score",
        "suite_score",
        "golden_passed",
    }
)
_SELF_ASSESSMENT_TEXT_RE = re.compile(
    r"(?i)(?:judge|evaluation|eval|自评|评分)\s*(?:score|得分|分数)?\s*[:=]\s*\d{1,3}"
)
_SECRET_PATTERNS = (
    re.compile(r"\bsk-[A-Za-z0-9_-]{12,}\b"),
    re.compile(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{12,}"),
    re.compile(
        r"(?i)\b(?:api[_ -]?key|access[_ -]?token|password|secret)\s*[:=]\s*"
        r"['\"]?[A-Za-z0-9._~+/=-]{8,}"
    ),
)


class ScenarioContractError(ValueError):
    """A scenario, observation, or validation artifact is malformed."""


def _strict_json_loads(text: str, *, label: str) -> Any:
    def pairs_hook(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate object key {key!r}")
            result[key] = value
        return result

    def reject_constant(value: str) -> None:
        raise ValueError(f"non-finite numeric constant {value}")

    try:
        return json.loads(text, object_pairs_hook=pairs_hook, parse_constant=reject_constant)
    except (json.JSONDecodeError, ValueError) as exc:
        raise ScenarioContractError(f"{label} is not strict JSON: {exc}") from exc


def _load_json(path: Path, *, label: str) -> Any:
    try:
        return _strict_json_loads(path.read_text(encoding="utf-8"), label=label)
    except OSError as exc:
        raise ScenarioContractError(f"unable to read {label}: {exc}") from exc


def _canonical_bytes(value: Any) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")


def _sha256(value: Any) -> str:
    return hashlib.sha256(_canonical_bytes(value)).hexdigest()


def _sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def _strict_keys(
    value: Mapping[str, Any],
    *,
    required: set[str],
    optional: set[str] | None = None,
    label: str,
) -> None:
    optional = optional or set()
    missing = required - set(value)
    unknown = set(value) - required - optional
    if missing:
        raise ScenarioContractError(f"{label} missing fields: {', '.join(sorted(missing))}")
    if unknown:
        raise ScenarioContractError(f"{label} unsupported fields: {', '.join(sorted(unknown))}")


def _identifier(value: Any, *, label: str) -> str:
    if not isinstance(value, str) or not _ID_RE.fullmatch(value.strip()):
        raise ScenarioContractError(f"{label} must be a valid identifier")
    return value.strip()


def _bounded_text(value: Any, *, label: str, maximum: int) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ScenarioContractError(f"{label} must be a non-empty string")
    resolved = value.strip()
    if len(resolved) > maximum:
        raise ScenarioContractError(f"{label} exceeds {maximum} characters")
    return resolved


def _integer(value: Any, *, label: str, minimum: int, maximum: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not minimum <= value <= maximum:
        raise ScenarioContractError(f"{label} must be an integer in [{minimum}, {maximum}]")
    return value


def _redact_text(value: str) -> str:
    redacted = value
    for pattern in _SECRET_PATTERNS:
        redacted = pattern.sub("[REDACTED]", redacted)
    return redacted


def _redact(value: Any) -> Any:
    if isinstance(value, str):
        return _redact_text(value)
    if isinstance(value, list):
        return [_redact(item) for item in value]
    if isinstance(value, dict):
        return {str(key): _redact(item) for key, item in value.items()}
    return value


def _validate_assertion(value: Any, *, scenario_id: str, index: int) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ScenarioContractError(f"{scenario_id} assertion {index} must be an object")
    kind = value.get("kind")
    if kind not in SUPPORTED_ASSERTIONS:
        raise ScenarioContractError(f"{scenario_id} assertion {index} has unsupported kind")
    required = {"assertion_id", "kind", "expected"}
    optional = {"description"}
    if kind.startswith("json_"):
        required.add("path")
    if kind == "json_number":
        optional.update({"absolute_tolerance", "relative_tolerance"})
    _strict_keys(value, required=required, optional=optional, label=f"{scenario_id} assertion")
    assertion_id = _identifier(value["assertion_id"], label="assertion_id")
    if "description" in value:
        _bounded_text(value["description"], label="assertion description", maximum=1000)
    if kind.startswith("json_"):
        path = value["path"]
        if not isinstance(path, str) or (path and not path.startswith("/")) or len(path) > 512:
            raise ScenarioContractError(f"{assertion_id} path must be an RFC 6901 JSON pointer")
    expected = value["expected"]
    if kind in {
        "json_set_equals",
        "json_contains_all",
        "json_excludes_all",
        "json_keys_equals",
        "text_contains_all",
        "text_excludes_all",
    }:
        if not isinstance(expected, list) or not expected or len(expected) > 64:
            raise ScenarioContractError(f"{assertion_id} expected must be a non-empty list")
        if kind.startswith("text_") and not all(
            isinstance(item, str) and item for item in expected
        ):
            raise ScenarioContractError(f"{assertion_id} text expected values must be strings")
        if kind == "json_keys_equals" and not all(
            isinstance(item, str) and item for item in expected
        ):
            raise ScenarioContractError(f"{assertion_id} expected object keys must be strings")
    if kind == "json_nonempty_string" and (
        isinstance(expected, bool) or not isinstance(expected, int) or not 1 <= expected <= 10_000
    ):
        raise ScenarioContractError(
            f"{assertion_id} expected must be a minimum string length in [1, 10000]"
        )
    if kind == "json_number":
        if isinstance(expected, bool) or not isinstance(expected, int | float):
            raise ScenarioContractError(f"{assertion_id} expected must be numeric")
        for key in ("absolute_tolerance", "relative_tolerance"):
            tolerance = value.get(key, 0)
            if (
                isinstance(tolerance, bool)
                or not isinstance(tolerance, int | float)
                or not math.isfinite(float(tolerance))
                or tolerance < 0
            ):
                raise ScenarioContractError(f"{assertion_id} {key} must be finite and non-negative")
    return dict(value)


def load_scenarios(path: Path) -> dict[str, Any]:
    value = _load_json(path, label="scenario contract")
    if not isinstance(value, dict):
        raise ScenarioContractError("scenario contract must be an object")
    _strict_keys(
        value,
        required={"schema_version", "suite_id", "scenarios"},
        label="scenario contract",
    )
    if value["schema_version"] != SCENARIO_SCHEMA:
        raise ScenarioContractError("unsupported scenario schema_version")
    _identifier(value["suite_id"], label="suite_id")
    scenarios = value["scenarios"]
    if not isinstance(scenarios, list) or not scenarios or len(scenarios) > 64:
        raise ScenarioContractError("scenarios must contain between 1 and 64 entries")
    seen: set[str] = set()
    for raw in scenarios:
        if not isinstance(raw, dict):
            raise ScenarioContractError("each scenario must be an object")
        _strict_keys(
            raw,
            required={
                "scenario_id",
                "domain",
                "prompt",
                "repetitions",
                "required_agent_ids",
                "require_parallel",
                "expected_assertions",
            },
            optional={
                "answer_locator",
                "model_id",
                "max_tokens",
                "execution_profile",
                "source_artifacts",
                "delegation_task_requirements",
                "canonical_delegation",
                "output_conformance_hints",
            },
            label="scenario",
        )
        scenario_id = _identifier(raw["scenario_id"], label="scenario_id")
        if scenario_id in seen:
            raise ScenarioContractError(f"duplicate scenario_id {scenario_id}")
        seen.add(scenario_id)
        _bounded_text(raw["domain"], label=f"{scenario_id} domain", maximum=120)
        _bounded_text(raw["prompt"], label=f"{scenario_id} prompt", maximum=40_000)
        output_hints = raw.get("output_conformance_hints", [])
        if (
            not isinstance(output_hints, list)
            or len(output_hints) > 32
            or any(
                not isinstance(hint, str) or not hint.strip() or len(hint) > 800
                for hint in output_hints
            )
            or len(output_hints) != len(set(output_hints))
        ):
            raise ScenarioContractError(
                f"{scenario_id} output_conformance_hints must be unique bounded strings"
            )
        _integer(raw["repetitions"], label=f"{scenario_id} repetitions", minimum=1, maximum=5)
        agent_ids = raw["required_agent_ids"]
        if not isinstance(agent_ids, list) or len(agent_ids) > 5:
            raise ScenarioContractError(f"{scenario_id} required_agent_ids must be a list")
        normalized_agents = [_identifier(item, label="required agent id") for item in agent_ids]
        if len(set(normalized_agents)) != len(normalized_agents):
            raise ScenarioContractError(f"{scenario_id} contains duplicate required agent ids")
        task_requirements = raw.get("delegation_task_requirements")
        canonical_delegation = raw.get("canonical_delegation")
        if normalized_agents:
            if not isinstance(task_requirements, list):
                raise ScenarioContractError(
                    f"{scenario_id} delegation_task_requirements is required"
                )
            if len(task_requirements) != len(normalized_agents):
                raise ScenarioContractError(
                    f"{scenario_id} needs exactly one delegation requirement per agent"
                )
            requirement_agents: list[str] = []
            for requirement in task_requirements:
                if not isinstance(requirement, dict):
                    raise ScenarioContractError(
                        f"{scenario_id} delegation task requirement must be an object"
                    )
                _strict_keys(
                    requirement,
                    required={
                        "agent_id",
                        "prompt_contains_all",
                        "prompt_excludes_all",
                        "min_prompt_chars",
                    },
                    label=f"{scenario_id} delegation task requirement",
                )
                agent_id = _identifier(requirement["agent_id"], label="requirement agent_id")
                requirement_agents.append(agent_id)
                for field in ("prompt_contains_all", "prompt_excludes_all"):
                    tokens = requirement[field]
                    if (
                        not isinstance(tokens, list)
                        or not tokens
                        or len(tokens) > 32
                        or not all(
                            isinstance(token, str) and token.strip() and len(token) <= 240
                            for token in tokens
                        )
                        or len(set(tokens)) != len(tokens)
                    ):
                        raise ScenarioContractError(
                            f"{scenario_id} {field} must contain unique bounded strings"
                        )
                _integer(
                    requirement["min_prompt_chars"],
                    label=f"{scenario_id} min_prompt_chars",
                    minimum=160,
                    maximum=20_000,
                )
            if sorted(requirement_agents) != sorted(normalized_agents):
                raise ScenarioContractError(
                    f"{scenario_id} delegation requirements must exactly match required agents"
                )
            if not isinstance(canonical_delegation, dict):
                raise ScenarioContractError(f"{scenario_id} canonical_delegation is required")
            _strict_keys(
                canonical_delegation,
                required={"max_concurrency", "tasks", "canonical_sha256"},
                label=f"{scenario_id} canonical delegation",
            )
            expected_concurrency = len(normalized_agents) if raw["require_parallel"] else 1
            if canonical_delegation["max_concurrency"] != expected_concurrency:
                raise ScenarioContractError(
                    f"{scenario_id} canonical delegation concurrency must be exact"
                )
            canonical_tasks = canonical_delegation["tasks"]
            if not isinstance(canonical_tasks, list) or len(canonical_tasks) != len(
                normalized_agents
            ):
                raise ScenarioContractError(
                    f"{scenario_id} canonical delegation needs exactly one task per agent"
                )
            canonical_identities: list[str] = []
            for index, task in enumerate(canonical_tasks):
                if not isinstance(task, dict):
                    raise ScenarioContractError(
                        f"{scenario_id} canonical delegation task must be an object"
                    )
                _strict_keys(
                    task,
                    required={"prompt", "description"},
                    optional={"agent_id", "agent_type"},
                    label=f"{scenario_id} canonical delegation task",
                )
                has_agent_id = isinstance(task.get("agent_id"), str) and bool(task["agent_id"])
                has_agent_type = isinstance(task.get("agent_type"), str) and bool(
                    task["agent_type"]
                )
                if has_agent_id == has_agent_type:
                    raise ScenarioContractError(
                        f"{scenario_id} canonical task must name exactly one agent identity"
                    )
                identity = (
                    _identifier(task["agent_id"], label="canonical agent_id")
                    if has_agent_id
                    else f"builtin:{_identifier(task['agent_type'], label='canonical agent_type')}"
                )
                canonical_identities.append(identity)
                _bounded_text(
                    task["description"],
                    label=f"{scenario_id} canonical task description",
                    maximum=200,
                )
                prompt = _bounded_text(
                    task["prompt"],
                    label=f"{scenario_id} canonical task prompt",
                    maximum=20_000,
                )
                requirement = next(
                    item for item in task_requirements if item["agent_id"] == identity
                )
                if (
                    len(prompt.strip()) < requirement["min_prompt_chars"]
                    or not all(token in prompt for token in requirement["prompt_contains_all"])
                    or not all(
                        token.casefold() not in prompt.casefold()
                        for token in requirement["prompt_excludes_all"]
                    )
                ):
                    raise ScenarioContractError(
                        f"{scenario_id} canonical task {index} violates its task requirement"
                    )
            if canonical_identities != normalized_agents:
                raise ScenarioContractError(
                    f"{scenario_id} canonical delegation task order must match required agents"
                )
            canonical_arguments = {
                "tasks": canonical_tasks,
                "max_concurrency": canonical_delegation["max_concurrency"],
            }
            declared_digest = canonical_delegation["canonical_sha256"]
            if not isinstance(declared_digest, str) or not _SHA_RE.fullmatch(declared_digest):
                raise ScenarioContractError(
                    f"{scenario_id} canonical delegation digest must be SHA-256"
                )
            if not hmac.compare_digest(declared_digest, _sha256(canonical_arguments)):
                raise ScenarioContractError(
                    f"{scenario_id} canonical delegation digest does not match tasks"
                )
        elif task_requirements not in (None, []):
            raise ScenarioContractError(
                f"{scenario_id} must not declare delegation requirements without agents"
            )
        elif canonical_delegation not in (None, {}):
            raise ScenarioContractError(
                f"{scenario_id} must not declare canonical delegation without agents"
            )
        if not isinstance(raw["require_parallel"], bool):
            raise ScenarioContractError(f"{scenario_id} require_parallel must be boolean")
        if raw["require_parallel"] and len(agent_ids) < 2:
            raise ScenarioContractError(f"{scenario_id} parallel execution needs two agents")
        locator = raw.get("answer_locator", "final_json_tag")
        if locator not in {"final_json_tag", "whole_output_json"}:
            raise ScenarioContractError(f"{scenario_id} has unsupported answer_locator")
        if "model_id" in raw:
            _bounded_text(raw["model_id"], label=f"{scenario_id} model_id", maximum=120)
        if "max_tokens" in raw:
            _integer(
                raw["max_tokens"], label=f"{scenario_id} max_tokens", minimum=256, maximum=8192
            )
        if raw.get("execution_profile", "safe") not in {"safe", "balanced", "powerful"}:
            raise ScenarioContractError(f"{scenario_id} execution_profile is unsupported")
        assertions = raw["expected_assertions"]
        if not isinstance(assertions, list) or not assertions or len(assertions) > 64:
            raise ScenarioContractError(f"{scenario_id} expected_assertions must be non-empty")
        normalized = [
            _validate_assertion(item, scenario_id=scenario_id, index=index)
            for index, item in enumerate(assertions)
        ]
        assertion_ids = [item["assertion_id"] for item in normalized]
        if len(set(assertion_ids)) != len(assertion_ids):
            raise ScenarioContractError(f"{scenario_id} contains duplicate assertion ids")
        artifacts = raw.get("source_artifacts", [])
        if not isinstance(artifacts, list) or len(artifacts) > 32:
            raise ScenarioContractError(f"{scenario_id} source_artifacts must be a list")
        artifact_ids: set[str] = set()
        for artifact in artifacts:
            if not isinstance(artifact, dict):
                raise ScenarioContractError(f"{scenario_id} source artifact must be an object")
            _strict_keys(
                artifact,
                required={"artifact_id", "path", "sha256"},
                optional={"description"},
                label=f"{scenario_id} source artifact",
            )
            artifact_id = _identifier(artifact["artifact_id"], label="artifact_id")
            if artifact_id in artifact_ids:
                raise ScenarioContractError(f"{scenario_id} has duplicate artifact_id")
            artifact_ids.add(artifact_id)
            artifact_path = artifact["path"]
            if (
                not isinstance(artifact_path, str)
                or not artifact_path
                or Path(artifact_path).is_absolute()
                or ".." in Path(artifact_path).parts
                or len(artifact_path) > 512
            ):
                raise ScenarioContractError(
                    f"{scenario_id} artifact path must be a contained relative path"
                )
            digest = artifact["sha256"]
            if not isinstance(digest, str) or not _SHA_RE.fullmatch(digest):
                raise ScenarioContractError(f"{scenario_id} artifact sha256 is malformed")
            if "description" in artifact:
                _bounded_text(artifact["description"], label="artifact description", maximum=1000)
    return value


def verify_source_artifacts(
    scenarios: Mapping[str, Any], *, scenario_directory: Path
) -> list[dict[str, Any]]:
    """Re-read scenario-owned sources and return immutable digest receipts."""

    root = scenario_directory.resolve()
    receipts: list[dict[str, Any]] = []
    for scenario in scenarios["scenarios"]:
        artifacts = scenario.get("source_artifacts", [])
        if not artifacts:
            raise ScenarioContractError(
                f"{scenario['scenario_id']} has no source_artifacts; real-domain evidence is required"
            )
        for artifact in artifacts:
            unresolved = scenario_directory / artifact["path"]
            if unresolved.is_symlink():
                raise ScenarioContractError("source artifact symlinks are not allowed")
            path = unresolved.resolve(strict=True)
            if not path.is_relative_to(root) or not path.is_file():
                raise ScenarioContractError("source artifact escapes the scenario directory")
            if path.stat().st_size > 2_000_000:
                raise ScenarioContractError("source artifact exceeds 2 MB")
            observed = hashlib.sha256(path.read_bytes()).hexdigest()
            if not hmac.compare_digest(observed, artifact["sha256"]):
                raise ScenarioContractError(
                    f"source artifact digest mismatch: {scenario['scenario_id']}:{artifact['artifact_id']}"
                )
            receipts.append(
                {
                    "scenario_id": scenario["scenario_id"],
                    "artifact_id": artifact["artifact_id"],
                    "relative_path": artifact["path"],
                    "content_sha256": observed,
                    "size_bytes": path.stat().st_size,
                }
            )
    return receipts


def verify_plugin_definitions(scenarios: Mapping[str, Any]) -> list[dict[str, Any]]:
    """Bind every requested plugin specialist to its local immutable definition."""

    repository_root = Path(__file__).resolve().parents[1]
    plugin_root = (repository_root / "agent-plugins").resolve(strict=True)
    required_ids = sorted(
        {
            agent_id
            for scenario in scenarios["scenarios"]
            for agent_id in scenario["required_agent_ids"]
            if not agent_id.startswith("builtin:")
        }
    )
    receipts: list[dict[str, Any]] = []
    for qualified_id in required_ids:
        if qualified_id.count(":") != 1:
            raise ScenarioContractError(f"plugin agent id is not qualified: {qualified_id}")
        plugin_id, agent_id = qualified_id.split(":", 1)
        if (
            plugin_id in {".", ".."}
            or agent_id in {".", ".."}
            or not _PLUGIN_COMPONENT_RE.fullmatch(plugin_id)
            or not _PLUGIN_COMPONENT_RE.fullmatch(agent_id)
        ):
            raise ScenarioContractError(f"plugin agent id is malformed: {qualified_id}")
        plugin_dir = (plugin_root / plugin_id).resolve(strict=False)
        if not plugin_dir.is_relative_to(plugin_root):
            raise ScenarioContractError(f"plugin definition path is unsafe: {qualified_id}")
        manifest = plugin_dir / "plugin.json"
        definition = plugin_dir / "agents" / f"{agent_id}.md"
        for path in (manifest, definition):
            if path.is_symlink() or not path.resolve().is_relative_to(plugin_dir.resolve()):
                raise ScenarioContractError(f"plugin definition path is unsafe: {qualified_id}")
            if not path.is_file() or path.stat().st_size > 100_000:
                raise ScenarioContractError(f"plugin definition is unavailable: {qualified_id}")
        receipts.append(
            {
                "qualified_agent_id": qualified_id,
                "plugin_manifest_sha256": hashlib.sha256(manifest.read_bytes()).hexdigest(),
                "definition_sha256": hashlib.sha256(definition.read_bytes()).hexdigest(),
            }
        )
    return receipts


def _attestation_key(override: str | None, *, environment_name: str, label: str) -> str:
    value = override or os.environ.get(environment_name, "")
    if not value or len(value.encode("utf-8")) < 32:
        raise ScenarioContractError(f"{label} requires at least 32 bytes from {environment_name}")
    return value


def _collector_attestation(payload: Mapping[str, Any], *, key: str) -> dict[str, Any]:
    return {
        "algorithm": "hmac-sha256",
        "key_id": hashlib.sha256(key.encode("utf-8")).hexdigest()[:24],
        "digest": hmac.new(
            key.encode("utf-8"), _canonical_bytes(payload), hashlib.sha256
        ).hexdigest(),
    }


def _verify_collector_attestation(document: Mapping[str, Any], *, key: str) -> None:
    attestation = document.get("collector_attestation")
    if not isinstance(attestation, dict) or attestation.get("algorithm") != "hmac-sha256":
        raise ScenarioContractError("collector HMAC attestation is missing")
    expected_key_id = hashlib.sha256(key.encode("utf-8")).hexdigest()[:24]
    if not hmac.compare_digest(str(attestation.get("key_id") or ""), expected_key_id):
        raise ScenarioContractError("collector attestation key id does not match")
    unsigned = dict(document)
    unsigned.pop("collector_attestation", None)
    expected = hmac.new(key.encode("utf-8"), _canonical_bytes(unsigned), hashlib.sha256).hexdigest()
    if not hmac.compare_digest(str(attestation.get("digest") or ""), expected):
        raise ScenarioContractError("collector HMAC attestation does not match")


def _safe_write_json(path: Path, value: Any) -> None:
    if path.is_symlink():
        raise ScenarioContractError("refusing to write receipt through a symlink")
    path = path.resolve(strict=False)
    path.parent.mkdir(parents=True, exist_ok=True)
    flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(path, flags, 0o600)
    try:
        os.fchmod(descriptor, stat.S_IRUSR | stat.S_IWUSR)
        with os.fdopen(descriptor, "w", encoding="utf-8", closefd=False) as handle:
            json.dump(value, handle, ensure_ascii=False, indent=2, sort_keys=True, allow_nan=False)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
    finally:
        os.close(descriptor)


def _event_text(data: Any) -> str:
    if isinstance(data, str):
        return data
    if not isinstance(data, dict):
        return ""
    for key in ("content", "message", "delta"):
        if isinstance(data.get(key), str):
            return str(data[key])
    return ""


def _whitelist(data: Any, fields: Sequence[str]) -> dict[str, Any]:
    if not isinstance(data, dict):
        return {}
    return _redact({field: data[field] for field in fields if field in data})


def summarize_sse_events(
    events: Sequence[Mapping[str, Any]], *, stream_sha256: str
) -> dict[str, Any]:
    """Return lifecycle observations only; no semantic pass/fail fields are produced."""

    starts: list[dict[str, Any]] = []
    finishes: list[dict[str, Any]] = []
    tools_started: list[dict[str, Any]] = []
    tools_finished: list[dict[str, Any]] = []
    terminals: list[dict[str, Any]] = []
    event_counts: dict[str, int] = {}
    text_parts: list[str] = []
    attempt_ids: set[str] = set()
    text_events: list[dict[str, Any]] = []
    event_sequence: list[dict[str, Any]] = []
    for ordinal, event in enumerate(events):
        event_type = str(event.get("event_type") or "")
        event_counts[event_type] = event_counts.get(event_type, 0) + 1
        data = event.get("data")
        if event_type in TEXT_EVENTS:
            text_value = _event_text(data)
            text_parts.append(text_value)
            text_events.append(
                {
                    "ordinal": ordinal,
                    "event_type": event_type,
                    "content_sha256": _sha256_text(text_value),
                    "content_chars": len(text_value),
                }
            )
        if isinstance(data, dict) and data.get("attempt_id"):
            attempt_ids.add(str(data["attempt_id"]))
        if event_type == "subagent_started":
            started = _whitelist(
                data,
                (
                    "agent_id",
                    "agent_type",
                    "profile_id",
                    "source_plugin",
                    "definition_sha256",
                    "dispatch_index",
                    "attempt_id",
                    "started_monotonic_ms",
                ),
            )
            started["ordinal"] = ordinal
            starts.append(started)
        elif event_type == "subagent_finished":
            finished = _whitelist(
                data,
                (
                    "agent_id",
                    "agent_type",
                    "profile_id",
                    "source_plugin",
                    "definition_sha256",
                    "dispatch_index",
                    "attempt_id",
                    "status",
                    "started_monotonic_ms",
                    "finished_monotonic_ms",
                    "duration_ms",
                    "tool_calls",
                    "result_summary",
                    "error",
                ),
            )
            finished["ordinal"] = ordinal
            finishes.append(finished)
        elif event_type in TOOL_START_EVENTS:
            tool_started = _whitelist(
                data,
                (
                    "tool_call_id",
                    "call_id",
                    "tool_id",
                    "name",
                    "tool_name",
                    "attempt_id",
                    "arguments",
                ),
            )
            tool_started["event_type"] = event_type
            tool_started["ordinal"] = ordinal
            tools_started.append(tool_started)
        elif event_type in TOOL_RESULT_EVENTS:
            tool_finished = _whitelist(
                data,
                (
                    "tool_call_id",
                    "call_id",
                    "tool_id",
                    "name",
                    "tool_name",
                    "attempt_id",
                    "success",
                    "status",
                    "error",
                    "error_code",
                    "side_effect_state",
                    "result",
                    "result_preview",
                ),
            )
            tool_finished["event_type"] = event_type
            tool_finished["ordinal"] = ordinal
            tools_finished.append(tool_finished)
        elif event_type in TERMINAL_EVENTS:
            terminal = _whitelist(
                data,
                ("attempt_id", "run_id", "status", "error", "error_code", "duration_ms"),
            )
            metadata = data.get("metadata") if isinstance(data, dict) else None
            envelope = metadata.get("terminal_envelope") if isinstance(metadata, dict) else None
            if isinstance(envelope, dict):
                terminal["terminal_envelope"] = _whitelist(
                    envelope, ("attempt_id", "run_id", "tenant_id", "status")
                )
            terminal["event_type"] = event_type
            terminal["ordinal"] = ordinal
            terminals.append(terminal)
        if event_type in (
            TEXT_EVENTS
            | TOOL_START_EVENTS
            | TOOL_RESULT_EVENTS
            | TERMINAL_EVENTS
            | {"subagent_started", "subagent_finished"}
        ):
            event_sequence.append({"ordinal": ordinal, "event_type": event_type})

    start_by_agent = {
        str(item.get("agent_id") or ""): item for item in starts if item.get("agent_id")
    }
    for item in finishes:
        started = start_by_agent.get(str(item.get("agent_id") or ""), {})
        for field in ("agent_type", "profile_id", "source_plugin", "definition_sha256"):
            if field not in item and field in started:
                item[field] = started[field]
    by_index: dict[int, dict[str, Any]] = {}
    for item in finishes:
        index = item.get("dispatch_index")
        if isinstance(index, int) and not isinstance(index, bool):
            by_index[index] = item
    overlaps: list[dict[str, Any]] = []
    for left_index in sorted(by_index):
        for right_index in sorted(index for index in by_index if index > left_index):
            left = by_index[left_index]
            right = by_index[right_index]
            values = (
                left.get("started_monotonic_ms"),
                left.get("finished_monotonic_ms"),
                right.get("started_monotonic_ms"),
                right.get("finished_monotonic_ms"),
            )
            observed = False
            overlap_ms = 0.0
            if all(isinstance(item, int | float) and not isinstance(item, bool) for item in values):
                left_start, left_finish, right_start, right_finish = map(float, values)
                overlap_ms = max(
                    0.0,
                    min(left_finish, right_finish) - max(left_start, right_start),
                )
                observed = overlap_ms > 0.0
            overlaps.append(
                {
                    "left_dispatch_index": left_index,
                    "right_dispatch_index": right_index,
                    "observed": observed,
                    "overlap_ms": overlap_ms,
                }
            )
    candidate_output = _redact_text("".join(text_parts).strip())[:100_000]
    return {
        "event_counts": dict(sorted(event_counts.items())),
        "stream_sha256": stream_sha256,
        "attempt_ids": sorted(attempt_ids),
        "candidate_output": candidate_output,
        "candidate_output_sha256": _sha256_text(candidate_output),
        "text_events": text_events,
        "event_sequence": event_sequence,
        "subagent_starts": starts,
        "subagent_finishes": finishes,
        "parallel_overlaps": overlaps,
        "tool_starts": tools_started,
        "tool_results": tools_finished,
        "terminal_events": terminals,
    }


def _required_env(*names: str) -> str:
    for name in names:
        value = os.environ.get(name, "").strip()
        if value:
            return value
    raise ScenarioContractError(f"missing required environment variable: {' or '.join(names)}")


def _external_suite_nonce() -> str:
    nonce = _required_env("GENERAL_AGENT_SUITE_NONCE").lower()
    if not re.fullmatch(r"[a-f0-9]{64}", nonce):
        raise ScenarioContractError("GENERAL_AGENT_SUITE_NONCE must be exactly 64 hex characters")
    return nonce


def _verified_runtime_attestation() -> str:
    attestation = _required_env("GENERAL_AGENT_RUNTIME_ATTESTATION")
    expected = _required_env("GENERAL_AGENT_EXPECTED_RUNTIME_ATTESTATION_SHA256").lower()
    if not _SHA_RE.fullmatch(expected):
        raise ScenarioContractError(
            "GENERAL_AGENT_EXPECTED_RUNTIME_ATTESTATION_SHA256 must be a SHA-256 digest"
        )
    if not hmac.compare_digest(_sha256_text(attestation), expected):
        raise ScenarioContractError("runtime attestation does not match independent expectation")
    return attestation


def _canonical_delegation_arguments(scenario: Mapping[str, Any]) -> dict[str, Any] | None:
    delegation = scenario.get("canonical_delegation")
    if not isinstance(delegation, dict):
        return None
    value = {
        "tasks": delegation["tasks"],
        "max_concurrency": delegation["max_concurrency"],
    }
    copied = _strict_json_loads(
        _canonical_bytes(value).decode("utf-8"), label="canonical delegation arguments"
    )
    assert isinstance(copied, dict)
    return copied


def _normalized_delegation_arguments(value: Any) -> dict[str, Any] | None:
    """Normalize model/tool adapters without weakening the exact task contract."""

    if not isinstance(value, dict):
        return None
    # Some provider adapters add a display-only batch description that is not
    # part of the task or authority contract. No other top-level additions are
    # accepted.
    if set(value) - {"tasks", "max_concurrency", "description"}:
        return None
    normalized = {
        "tasks": value.get("tasks"),
        "max_concurrency": value.get("max_concurrency"),
    }
    copied = _strict_json_loads(
        _canonical_bytes(normalized).decode("utf-8"),
        label="normalized delegation arguments",
    )
    return copied if isinstance(copied, dict) else None


def _candidate_prompt(scenario: Mapping[str, Any]) -> str:
    """Return the eval-only prompt including the host-owned exact delegation object."""

    base_prompt = str(scenario["prompt"])
    canonical_arguments = _canonical_delegation_arguments(scenario)
    sections = [base_prompt]
    if canonical_arguments is not None:
        canonical_json = _canonical_bytes(canonical_arguments).decode("utf-8")
        sections.append(
            "EVAL-ONLY HOST DELEGATION OBJECT\n"
            "For this acceptance run, call spawn_subagent exactly once using the following "
            "JSON object as its entire arguments object. Preserve every task prompt, description, "
            "identity, array order, and max_concurrency exactly; do not add fields or instructions.\n"
            f"{canonical_json}"
        )
    sections.append(
        "HOST OUTPUT CONFORMANCE\n"
        "Follow the task's OUTPUT CONTRACT literally. Emit exactly one <FINAL_JSON> object and "
        "never wrap it in a Markdown fence. If the task explicitly requests a memo before the "
        "block, keep it concise; otherwise emit no prose outside the block. Keep literal enum "
        "tokens, IDs, scalar types, and exact arrays as specified; do not replace them with "
        "explanatory objects or synonyms. Do not call parent tools other than the one host "
        "delegation call above."
    )
    hints = scenario.get("output_conformance_hints")
    if isinstance(hints, list) and hints:
        sections.append(
            "HOST-SPECIFIED OUTPUT LITERALS\n" + "\n".join(f"- {hint}" for hint in hints)
        )
    return "\n\n".join(sections)


def _login(client: httpx.Client, *, api_prefix: str, email: str, password: str) -> str:
    response = client.post(f"{api_prefix}/auth/login", json={"email": email, "password": password})
    response.raise_for_status()
    payload = response.json()
    token = payload.get("access_token") if isinstance(payload, dict) else None
    if not isinstance(token, str) or not token:
        raise ScenarioContractError("login did not return an access token")
    return token


def _collect_trial(
    client: httpx.Client,
    *,
    api_prefix: str,
    token: str,
    scenario: Mapping[str, Any],
    trial_number: int,
    default_model: str,
    suite_nonce: str,
) -> tuple[dict[str, Any], dict[str, Any]]:
    session_id = str(uuid.uuid4())
    collector_challenge = secrets.token_hex(32)
    events: list[dict[str, Any]] = []
    raw_payloads: list[str] = []
    stream_hasher = hashlib.sha256()
    candidate_prompt = _candidate_prompt(scenario)
    with client.stream(
        "POST",
        f"{api_prefix}/assistant/chat/stream",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "session_id": session_id,
            "message": candidate_prompt,
            "model_id": scenario.get("model_id", default_model),
            "temperature": 0.0,
            "max_tokens": scenario.get("max_tokens", 3000),
            "kb_mode": "off",
            "web_search_enabled": False,
            "execution_profile": scenario.get("execution_profile", "safe"),
            "memory_mode": "off",
            "os_agent_enabled": False,
        },
    ) as response:
        response.raise_for_status()
        for line in response.iter_lines():
            if not line or not line.startswith("data: "):
                continue
            raw = line[6:].strip()
            if not raw:
                continue
            raw_payloads.append(raw)
            stream_hasher.update(raw.encode("utf-8"))
            stream_hasher.update(b"\n")
            try:
                event = _strict_json_loads(raw, label="Gateway SSE event")
            except ScenarioContractError:
                continue
            if not isinstance(event, dict):
                continue
            events.append(event)
            if event.get("event_type") in TERMINAL_EVENTS:
                break
    observation = summarize_sse_events(events, stream_sha256=stream_hasher.hexdigest())
    observation.update(
        {
            "scenario_id": scenario["scenario_id"],
            "trial": trial_number,
            "session_id": session_id,
            "suite_nonce": suite_nonce,
            "collector_challenge": collector_challenge,
            "prompt_sha256": _sha256_text(candidate_prompt),
        }
    )
    observation["observation_sha256"] = _sha256(observation)
    raw_trial = {
        "scenario_id": scenario["scenario_id"],
        "trial": trial_number,
        "session_id": session_id,
        "suite_nonce": suite_nonce,
        "collector_challenge": collector_challenge,
        "raw_sse_payloads": raw_payloads,
        "stream_sha256": stream_hasher.hexdigest(),
    }
    raw_trial["raw_trial_sha256"] = _sha256(raw_trial)
    return observation, raw_trial


def _runtime_binding(
    client: httpx.Client,
    *,
    base_url: str,
    api_prefix: str,
    token: str,
    operator_attestation: str,
) -> dict[str, Any]:
    health = client.get(f"{base_url}/health")
    health.raise_for_status()
    tools_response = client.get(
        f"{api_prefix}/assistant/tools", headers={"Authorization": f"Bearer {token}"}
    )
    tools_response.raise_for_status()
    try:
        health_payload = health.json()
        tools_payload = tools_response.json()
    except ValueError as exc:
        raise ScenarioContractError("runtime binding endpoints did not return JSON") from exc
    return {
        "gateway_health_sha256": _sha256(_redact(health_payload)),
        "authenticated_tool_catalog_sha256": _sha256(_redact(tools_payload)),
        "operator_container_runtime_attestation_sha256": _sha256_text(operator_attestation),
    }


def collect(
    scenario_path: Path,
    output_path: Path,
    *,
    raw_output_path: Path | None = None,
    collector_key: str | None = None,
    runtime_attestation: str | None = None,
) -> dict[str, Any]:
    scenarios = load_scenarios(scenario_path)
    source_artifacts = verify_source_artifacts(
        scenarios, scenario_directory=scenario_path.resolve().parent
    )
    plugin_definitions = verify_plugin_definitions(scenarios)
    resolved_collector_key = _attestation_key(
        collector_key,
        environment_name="GENERAL_AGENT_COLLECTOR_HMAC_KEY",
        label="standalone collection",
    )
    if runtime_attestation is not None:
        raise ScenarioContractError("runtime attestation must come from the verified environment")
    resolved_runtime_attestation = _verified_runtime_attestation()
    suite_nonce = _external_suite_nonce()
    raw_output = raw_output_path or output_path.with_name(f"{output_path.stem}.raw-sse.json")
    if raw_output.resolve().parent != output_path.resolve().parent:
        raise ScenarioContractError("raw SSE artifact must share the observation directory")
    base_url = os.environ.get("GENERAL_AGENT_GATEWAY_BASE_URL", "http://127.0.0.1:8080").rstrip("/")
    email = _required_env("GENERAL_AGENT_GATEWAY_EMAIL", "ASSISTANT_ISOLATION_EMAIL")
    password = _required_env("GENERAL_AGENT_GATEWAY_PASSWORD", "ASSISTANT_ISOLATION_PASSWORD")
    default_model = os.environ.get("GENERAL_AGENT_CANDIDATE_MODEL", "qwen3.7-plus").strip()
    timeout = float(os.environ.get("GENERAL_AGENT_GATEWAY_TIMEOUT_S", "300"))
    if not default_model or not 5 <= timeout <= 900:
        raise ScenarioContractError("invalid candidate model or Gateway timeout")
    trials: list[dict[str, Any]] = []
    raw_trials: list[dict[str, Any]] = []
    with httpx.Client(timeout=timeout, trust_env=False) as client:
        token = _login(client, api_prefix=f"{base_url}/api/v1", email=email, password=password)
        runtime_binding = _runtime_binding(
            client,
            base_url=base_url,
            api_prefix=f"{base_url}/api/v1",
            token=token,
            operator_attestation=resolved_runtime_attestation,
        )
        for scenario in scenarios["scenarios"]:
            for trial_number in range(1, scenario["repetitions"] + 1):
                observation, raw_trial = _collect_trial(
                    client,
                    api_prefix=f"{base_url}/api/v1",
                    token=token,
                    scenario=scenario,
                    trial_number=trial_number,
                    default_model=default_model,
                    suite_nonce=suite_nonce,
                )
                trials.append(observation)
                raw_trials.append(raw_trial)
                print(
                    f"scenario={scenario['scenario_id']} trial={trial_number} "
                    f"terminal_events={len(observation['terminal_events'])} "
                    f"subagent_finishes={len(observation['subagent_finishes'])}"
                )
    raw_document: dict[str, Any] = {
        "schema_version": RAW_SSE_SCHEMA,
        "suite_id": scenarios["suite_id"],
        "suite_nonce": suite_nonce,
        "trials": raw_trials,
    }
    raw_document["raw_sse_sha256"] = _sha256(raw_document)
    raw_document["collector_attestation"] = _collector_attestation(
        raw_document, key=resolved_collector_key
    )
    _safe_write_json(raw_output, raw_document)
    raw_artifact_sha256 = hashlib.sha256(raw_output.read_bytes()).hexdigest()
    document = {
        "schema_version": OBSERVATION_SCHEMA,
        "suite_id": scenarios["suite_id"],
        "scenario_contract_sha256": _sha256(scenarios),
        "suite_nonce": suite_nonce,
        "collector": {
            "transport": "gateway-sse",
            "candidate_model_default": default_model,
            "semantic_verdicts_emitted": False,
        },
        "runtime_binding": runtime_binding,
        "source_artifacts": source_artifacts,
        "plugin_definitions": plugin_definitions,
        "raw_sse_artifact": {
            "file_name": raw_output.name,
            "content_sha256": raw_artifact_sha256,
            "mode": "0600",
        },
        "trials": trials,
    }
    document["observations_sha256"] = _sha256(document)
    document["collector_attestation"] = _collector_attestation(document, key=resolved_collector_key)
    _safe_write_json(output_path, document)
    return document


def _json_pointer(document: Any, pointer: str) -> tuple[bool, Any]:
    if pointer == "":
        return True, document
    current = document
    for raw_part in pointer[1:].split("/"):
        part = raw_part.replace("~1", "/").replace("~0", "~")
        if isinstance(current, dict) and part in current:
            current = current[part]
        elif isinstance(current, list) and part.isdigit() and int(part) < len(current):
            current = current[int(part)]
        else:
            return False, None
    return True, current


def _answer_json(candidate_output: str, locator: str) -> Any:
    if locator == "whole_output_json":
        return _strict_json_loads(candidate_output, label="candidate answer")
    matches = re.findall(r"<FINAL_JSON>\s*(.*?)\s*</FINAL_JSON>", candidate_output, re.DOTALL)
    if len(matches) != 1:
        raise ScenarioContractError("candidate must contain exactly one FINAL_JSON block")
    return _strict_json_loads(matches[0], label="candidate FINAL_JSON")


def _set_key(value: Any) -> bytes:
    return _canonical_bytes(value)


def _evaluate_assertion(
    assertion: Mapping[str, Any], *, answer: Any, candidate_output: str
) -> tuple[bool, str, Any]:
    kind = assertion["kind"]
    expected = assertion["expected"]
    if kind.startswith("text_"):
        actual = candidate_output
        if kind == "text_contains_all":
            passed = all(item in actual for item in expected)
        else:
            passed = all(item not in actual for item in expected)
        return passed, "matched" if passed else "text_mismatch", actual
    found, actual = _json_pointer(answer, str(assertion["path"]))
    if not found:
        return False, "json_path_missing", None
    if kind == "json_equals":
        passed = actual == expected and type(actual) is type(expected)  # noqa: E721
    elif kind == "json_number":
        if isinstance(actual, bool) or not isinstance(actual, int | float):
            passed = False
        else:
            expected_number = float(expected)
            actual_number = float(actual)
            absolute = float(assertion.get("absolute_tolerance", 0))
            relative = float(assertion.get("relative_tolerance", 0))
            passed = math.isfinite(actual_number) and math.isclose(
                actual_number,
                expected_number,
                rel_tol=relative,
                abs_tol=absolute,
            )
    elif kind == "json_set_equals":
        passed = isinstance(actual, list) and {_set_key(item) for item in actual} == {
            _set_key(item) for item in expected
        }
    elif kind == "json_contains_all":
        if isinstance(actual, list):
            actual_values = {_set_key(item) for item in actual}
            passed = all(_set_key(item) in actual_values for item in expected)
        elif isinstance(actual, str):
            passed = all(isinstance(item, str) and item in actual for item in expected)
        else:
            passed = False
    elif kind == "json_excludes_all":
        if isinstance(actual, list):
            actual_values = {_set_key(item) for item in actual}
            passed = all(_set_key(item) not in actual_values for item in expected)
        elif isinstance(actual, str):
            passed = all(isinstance(item, str) and item not in actual for item in expected)
        else:
            passed = False
    elif kind == "json_keys_equals":
        passed = isinstance(actual, dict) and set(actual) == set(expected)
    elif kind == "json_nonempty_string":
        passed = isinstance(actual, str) and len(actual.strip()) >= int(expected)
    else:  # pragma: no cover - guarded by scenario validation
        raise ScenarioContractError(f"unsupported assertion kind {kind}")
    return passed, "matched" if passed else "value_mismatch", actual


def _self_assessment_detected(answer: Any, candidate_output: str) -> bool:
    def has_reserved_key(value: Any) -> bool:
        if isinstance(value, dict):
            if any(str(key).casefold() in _SELF_ASSESSMENT_KEYS for key in value):
                return True
            return any(has_reserved_key(item) for item in value.values())
        if isinstance(value, list):
            return any(has_reserved_key(item) for item in value)
        return False

    return has_reserved_key(answer) or bool(_SELF_ASSESSMENT_TEXT_RE.search(candidate_output))


def _execution_checks(
    scenario: Mapping[str, Any], observation: Mapping[str, Any]
) -> list[dict[str, Any]]:
    required_profiles = list(scenario["required_agent_ids"])

    def records(field: str) -> list[Mapping[str, Any]]:
        value = observation.get(field)
        if not isinstance(value, list):
            return []
        return [item for item in value if isinstance(item, dict)]

    starts = records("subagent_starts")
    finishes = records("subagent_finishes")
    terminals = records("terminal_events")
    overlaps = records("parallel_overlaps")
    tool_starts = records("tool_starts")
    tool_results = records("tool_results")
    text_events = records("text_events")
    attempt_ids_value = observation.get("attempt_ids")
    attempt_ids = (
        [str(item) for item in attempt_ids_value if isinstance(item, str) and item]
        if isinstance(attempt_ids_value, list)
        else []
    )
    current_attempt = attempt_ids[0] if len(attempt_ids) == 1 else ""

    def agent_identity(item: Mapping[str, Any]) -> str:
        profile_id = str(item.get("profile_id") or "")
        if profile_id:
            return profile_id
        agent_type = str(item.get("agent_type") or "")
        return f"builtin:{agent_type}" if agent_type else ""

    def valid_ordinal(value: Any) -> bool:
        return isinstance(value, int) and not isinstance(value, bool) and value >= 0

    def indexed(receipts: Sequence[Mapping[str, Any]]) -> tuple[dict[int, Mapping[str, Any]], bool]:
        result: dict[int, Mapping[str, Any]] = {}
        valid = len(receipts) == len(required_profiles)
        for receipt in receipts:
            dispatch_index = receipt.get("dispatch_index")
            if (
                not isinstance(dispatch_index, int)
                or isinstance(dispatch_index, bool)
                or dispatch_index in result
            ):
                valid = False
                continue
            result[dispatch_index] = receipt
        return result, valid

    starts_by_index, starts_indexed = indexed(starts)
    finishes_by_index, finishes_indexed = indexed(finishes)
    expected_indexes = set(range(len(required_profiles)))
    profile_receipts_valid = bool(
        current_attempt
        and starts_indexed
        and finishes_indexed
        and set(starts_by_index) == expected_indexes
        and set(finishes_by_index) == expected_indexes
    )
    if profile_receipts_valid:
        for dispatch_index, expected_profile in enumerate(required_profiles):
            started = starts_by_index[dispatch_index]
            finished = finishes_by_index[dispatch_index]
            started_identity = agent_identity(started)
            finished_identity = agent_identity(finished)
            started_at = started.get("started_monotonic_ms")
            finished_started_at = finished.get("started_monotonic_ms")
            finished_at = finished.get("finished_monotonic_ms")
            plugin_receipt_valid = bool(
                (
                    expected_profile.startswith("builtin:")
                    and not started.get("definition_sha256")
                    and not finished.get("definition_sha256")
                )
                or (
                    not expected_profile.startswith("builtin:")
                    and isinstance(started.get("definition_sha256"), str)
                    and _SHA_RE.fullmatch(str(started["definition_sha256"]))
                    and started.get("definition_sha256") == finished.get("definition_sha256")
                    and bool(started.get("source_plugin"))
                    and started.get("source_plugin") == finished.get("source_plugin")
                )
            )
            timing_valid = bool(
                isinstance(started_at, int | float)
                and not isinstance(started_at, bool)
                and isinstance(finished_started_at, int | float)
                and not isinstance(finished_started_at, bool)
                and isinstance(finished_at, int | float)
                and not isinstance(finished_at, bool)
                and float(started_at) == float(finished_started_at)
                and float(finished_at) >= float(started_at)
                and valid_ordinal(started.get("ordinal"))
                and valid_ordinal(finished.get("ordinal"))
                and int(started["ordinal"]) < int(finished["ordinal"])
            )
            if not (
                started_identity == expected_profile
                and finished_identity == expected_profile
                and str(started.get("agent_id") or "")
                and started.get("agent_id") == finished.get("agent_id")
                and started.get("attempt_id") == current_attempt
                and finished.get("attempt_id") == current_attempt
                and finished.get("status") == "completed"
                and not finished.get("error")
                and plugin_receipt_valid
                and timing_valid
            ):
                profile_receipts_valid = False
                break

    terminal_valid = bool(
        len(terminals) == 1
        and terminals[0].get("event_type") == "run_finished"
        and terminals[0].get("attempt_id") == current_attempt
        and terminals[0].get("status") not in {"error", "failed", "cancelled", "blocked"}
        and not terminals[0].get("error")
    )

    expected_pairs = {
        (left, right)
        for left in range(len(required_profiles))
        for right in range(left + 1, len(required_profiles))
    }
    observed_pairs: dict[tuple[int, int], Mapping[str, Any]] = {}
    parallel_valid = True
    for overlap in overlaps:
        left = overlap.get("left_dispatch_index")
        right = overlap.get("right_dispatch_index")
        if (
            not isinstance(left, int)
            or isinstance(left, bool)
            or not isinstance(right, int)
            or isinstance(right, bool)
            or left >= right
            or (left, right) in observed_pairs
        ):
            parallel_valid = False
            continue
        observed_pairs[(left, right)] = overlap
    if scenario["require_parallel"]:
        parallel_valid = bool(
            parallel_valid
            and set(observed_pairs) == expected_pairs
            and all(
                item.get("observed") is True
                and isinstance(item.get("overlap_ms"), int | float)
                and not isinstance(item.get("overlap_ms"), bool)
                and float(item["overlap_ms"]) >= MIN_PARALLEL_OVERLAP_MS
                for item in observed_pairs.values()
            )
        )
    prompt_valid = observation.get("prompt_sha256") == _sha256_text(_candidate_prompt(scenario))

    def call_id(item: Mapping[str, Any]) -> str:
        return str(item.get("tool_call_id") or item.get("call_id") or item.get("tool_id") or "")

    def tool_name(item: Mapping[str, Any]) -> str:
        return str(item.get("name") or item.get("tool_name") or "")

    def aliases_are_consistent(item: Mapping[str, Any], fields: Sequence[str]) -> bool:
        values = [str(item[field]) for field in fields if item.get(field) not in (None, "")]
        return bool(values) and len(set(values)) == 1

    normalized_starts: dict[str, dict[str, Any]] = {}
    normalized_arguments: dict[str, Any] = {}
    malformed_tool_receipt = False
    for item in tool_starts:
        if not aliases_are_consistent(item, ("tool_call_id", "call_id", "tool_id")) or not (
            aliases_are_consistent(item, ("name", "tool_name"))
        ):
            malformed_tool_receipt = True
        identifier = call_id(item)
        if not identifier:
            malformed_tool_receipt = True
            continue
        merged = normalized_starts.setdefault(identifier, {})
        previous_name = tool_name(merged)
        current_name = tool_name(item)
        if previous_name and current_name and previous_name != current_name:
            malformed_tool_receipt = True
        for key, value in item.items():
            if value in (None, ""):
                continue
            if key == "ordinal" and valid_ordinal(value):
                existing = merged.get("ordinal")
                merged[key] = min(int(existing), value) if valid_ordinal(existing) else value
            elif key == "arguments":
                parsed_arguments = value
                if isinstance(value, str):
                    try:
                        parsed_arguments = _strict_json_loads(value, label="tool start arguments")
                    except ScenarioContractError:
                        malformed_tool_receipt = True
                        continue
                if identifier in normalized_arguments and _canonical_bytes(
                    normalized_arguments[identifier]
                ) != _canonical_bytes(parsed_arguments):
                    malformed_tool_receipt = True
                else:
                    normalized_arguments[identifier] = parsed_arguments
                    merged[key] = parsed_arguments
            else:
                merged[key] = value
    normalized_results: dict[str, list[Mapping[str, Any]]] = {}
    for item in tool_results:
        if not aliases_are_consistent(item, ("tool_call_id", "call_id", "tool_id")) or not (
            aliases_are_consistent(item, ("name", "tool_name"))
        ):
            malformed_tool_receipt = True
        identifier = call_id(item)
        if not identifier:
            malformed_tool_receipt = True
            continue
        start = normalized_starts.get(identifier)
        if start is None or not tool_name(item) or tool_name(item) != tool_name(start):
            malformed_tool_receipt = True
        normalized_results.setdefault(identifier, []).append(item)

    start_ids = set(normalized_starts)
    result_ids = set(normalized_results)
    tool_pairs_valid = bool(not malformed_tool_receipt and start_ids == result_ids)
    spawn_call_ids = [
        identifier
        for identifier, item in normalized_starts.items()
        if tool_name(item) == "spawn_subagent"
    ]
    spawn_call_id = spawn_call_ids[0] if len(spawn_call_ids) == 1 else ""
    spawn_call = normalized_starts.get(spawn_call_id, {})
    arguments: Any = spawn_call.get("arguments")
    if isinstance(arguments, str):
        try:
            arguments = _strict_json_loads(arguments, label="spawn_subagent arguments")
        except ScenarioContractError:
            arguments = None
    normalized_delegation = _normalized_delegation_arguments(arguments)
    tasks = (
        normalized_delegation.get("tasks")
        if isinstance(normalized_delegation, dict)
        else None
    )
    canonical_arguments = _canonical_delegation_arguments(scenario)
    canonical_delegation_valid = bool(
        (not required_profiles and canonical_arguments is None and arguments is None)
        or (
            required_profiles
            and isinstance(normalized_delegation, dict)
            and isinstance(canonical_arguments, dict)
            and _canonical_bytes(normalized_delegation) == _canonical_bytes(canonical_arguments)
            and _sha256(normalized_delegation)
            == str(scenario["canonical_delegation"]["canonical_sha256"])
        )
    )

    def task_identity(task: Any) -> str:
        if not isinstance(task, dict):
            return ""
        if task.get("agent_id"):
            return str(task["agent_id"])
        if task.get("agent_type"):
            return f"builtin:{task['agent_type']}"
        return ""

    task_identities = [task_identity(task) for task in tasks] if isinstance(tasks, list) else []
    expected_concurrency = len(required_profiles) if scenario["require_parallel"] else 1
    basic_delegation_shape_valid = bool(
        (not required_profiles and not spawn_call_ids)
        or (
            required_profiles
            and not malformed_tool_receipt
            and len(spawn_call_ids) == 1
            and isinstance(normalized_delegation, dict)
            and isinstance(tasks, list)
            and task_identities == required_profiles
            and len(task_identities) == len(required_profiles)
            and isinstance(normalized_delegation.get("max_concurrency"), int)
            and not isinstance(normalized_delegation.get("max_concurrency"), bool)
            and normalized_delegation["max_concurrency"] == expected_concurrency
        )
    )
    delegation_tool_valid = basic_delegation_shape_valid and canonical_delegation_valid

    requirements = scenario.get("delegation_task_requirements")
    requirement_by_agent = (
        {str(item.get("agent_id") or ""): item for item in requirements if isinstance(item, dict)}
        if isinstance(requirements, list)
        else {}
    )
    task_prompts_valid = not required_profiles
    if basic_delegation_shape_valid and required_profiles and isinstance(tasks, list):
        task_prompts_valid = True
        for expected_agent, task in zip(required_profiles, tasks, strict=True):
            requirement = requirement_by_agent.get(expected_agent)
            prompt = task.get("prompt") if isinstance(task, dict) else None
            if not isinstance(requirement, dict) or not isinstance(prompt, str):
                task_prompts_valid = False
                break
            normalized_prompt = prompt.strip()
            contains = requirement.get("prompt_contains_all")
            excludes = requirement.get("prompt_excludes_all")
            if not (
                len(normalized_prompt) >= int(requirement.get("min_prompt_chars", 0))
                and isinstance(contains, list)
                and all(isinstance(token, str) and token in normalized_prompt for token in contains)
                and isinstance(excludes, list)
                and all(
                    isinstance(token, str) and token.casefold() not in normalized_prompt.casefold()
                    for token in excludes
                )
            ):
                task_prompts_valid = False
                break

    def no_error(item: Mapping[str, Any]) -> bool:
        return item.get("error") in (None, "", False, [], {}) and item.get("error_code") in (
            None,
            "",
        )

    spawn_results = normalized_results.get(spawn_call_id, []) if spawn_call_id else []
    completion_records = [
        item
        for item in spawn_results
        if item.get("event_type") in TOOL_RESULT_EVENTS
        and tool_name(item) == "spawn_subagent"
        and item.get("success") is True
        and item.get("status") in (None, "", "completed", "succeeded", "success")
        and no_error(item)
        and str(item.get("side_effect_state") or "").casefold() in {"none", "read_only"}
        and valid_ordinal(item.get("ordinal"))
    ]
    spawn_aggregate_valid = not required_profiles
    if required_profiles:
        spawn_aggregate_valid = bool(
            delegation_tool_valid
            and not malformed_tool_receipt
            and len(completion_records) == 1
            and all(
                item.get("success") is not False
                and str(item.get("status") or "").casefold()
                not in {"error", "failed", "cancelled", "blocked", "unknown"}
                and no_error(item)
                and str(item.get("side_effect_state") or "").casefold() in {"none", "read_only"}
                for item in spawn_results
            )
        )

    parent_tool_names = {tool_name(item) for item in normalized_starts.values()}
    no_extra_parent_tools = bool(
        not malformed_tool_receipt
        and parent_tool_names <= SIDE_EFFECT_FREE_PARENT_TOOLS
        and (
            (required_profiles and start_ids == {spawn_call_id})
            or (not required_profiles and not start_ids)
        )
    )

    synthesis_order_valid = False
    terminal_ordinal = terminals[0].get("ordinal") if len(terminals) == 1 else None
    candidate_text_ordinals = [
        int(item["ordinal"])
        for item in text_events
        if valid_ordinal(item.get("ordinal"))
        and isinstance(item.get("content_chars"), int)
        and not isinstance(item.get("content_chars"), bool)
        and int(item["content_chars"]) > 0
    ]
    if terminal_valid and valid_ordinal(terminal_ordinal) and candidate_text_ordinals:
        final_text_ordinal = max(candidate_text_ordinals)
        if required_profiles and profile_receipts_valid and spawn_aggregate_valid:
            child_terminal_ordinals = [int(item["ordinal"]) for item in finishes]
            aggregate_ordinal = int(completion_records[0]["ordinal"])
            synthesis_order_valid = bool(
                max(child_terminal_ordinals)
                < aggregate_ordinal
                < final_text_ordinal
                < int(terminal_ordinal)
            )
        elif not required_profiles:
            synthesis_order_valid = final_text_ordinal < int(terminal_ordinal)

    # The prompt hash binds the public task while this separate gate binds the
    # actual child prompts emitted by the candidate's spawn_subagent call.
    values = {
        "terminal.current-attempt-success": terminal_valid,
        "prompt.digest-bound": prompt_valid,
        "delegation.exact-profile-lifecycle": profile_receipts_valid,
        "delegation.parallel-overlap": parallel_valid,
        "delegation.canonical-task-object": canonical_delegation_valid,
        "delegation.task-prompts": task_prompts_valid,
        "tools.start-result-paired": tool_pairs_valid,
        "tools.delegation-call-observed": delegation_tool_valid,
        "tools.spawn-aggregate-success": spawn_aggregate_valid,
        "tools.no-extra-parent-side-effects": no_extra_parent_tools,
        "lifecycle.delegation-synthesis-order": synthesis_order_valid,
    }
    return [
        {
            "check_id": check_id,
            "passed": passed,
            "observed_sha256": _sha256(
                {
                    "scenario_id": scenario["scenario_id"],
                    "observation_sha256": observation.get("observation_sha256"),
                    "check_id": check_id,
                    "passed": passed,
                }
            ),
        }
        for check_id, passed in values.items()
    ]


def _validated_trial(scenario: Mapping[str, Any], observation: Mapping[str, Any]) -> dict[str, Any]:
    candidate_output = str(observation.get("candidate_output") or "")
    parse_error: str | None = None
    try:
        answer = _answer_json(candidate_output, scenario.get("answer_locator", "final_json_tag"))
    except ScenarioContractError as exc:
        answer = None
        parse_error = str(exc)
    self_assessment = _self_assessment_detected(answer, candidate_output)
    results: list[dict[str, Any]] = []
    for assertion in scenario["expected_assertions"]:
        if parse_error and not assertion["kind"].startswith("text_"):
            passed, reason, actual = False, "answer_json_invalid", None
        else:
            passed, reason, actual = _evaluate_assertion(
                assertion,
                answer=answer,
                candidate_output=candidate_output,
            )
        result = {
            "assertion_id": assertion["assertion_id"],
            "kind": assertion["kind"],
            "passed": passed,
            "reason": reason,
            "expected_sha256": _sha256(assertion["expected"]),
            "actual_sha256": _sha256(actual),
        }
        results.append(result)
    execution_checks = _execution_checks(scenario, observation)
    golden_passed = all(item["passed"] for item in results)
    execution_passed = all(item["passed"] for item in execution_checks)
    return {
        "scenario_id": scenario["scenario_id"],
        "trial": observation["trial"],
        "observation_sha256": observation["observation_sha256"],
        "answer_parse_error": parse_error,
        "candidate_self_assessment_detected": self_assessment,
        "assertions": results,
        "execution_checks": execution_checks,
        "golden_passed": golden_passed,
        "execution_checks_passed": execution_passed,
        "trial_accepted": golden_passed and execution_passed and not self_assessment,
    }


def _domain_macro(
    scenarios: Mapping[str, Any], results: Sequence[Mapping[str, Any]]
) -> dict[str, Any]:
    scenario_domain = {item["scenario_id"]: item["domain"] for item in scenarios["scenarios"]}
    domains: dict[str, dict[str, Any]] = {}
    for result in results:
        domain = scenario_domain[str(result["scenario_id"])]
        aggregate = domains.setdefault(domain, {"trials": 0, "accepted_trials": 0})
        aggregate["trials"] += 1
        aggregate["accepted_trials"] += int(result["trial_accepted"] is True)
    for aggregate in domains.values():
        aggregate["accepted_trial_rate"] = aggregate["accepted_trials"] / aggregate["trials"]
        aggregate["all_trials_accepted"] = aggregate["accepted_trials"] == aggregate["trials"]
    rates = [float(item["accepted_trial_rate"]) for item in domains.values()]
    return {
        "domains": dict(sorted(domains.items())),
        "macro_accepted_trial_rate": sum(rates) / len(rates),
        "all_domains_full_pass": all(item["all_trials_accepted"] for item in domains.values()),
        "three_run_policy_met": all(
            scenario["repetitions"] == 3 for scenario in scenarios["scenarios"]
        ),
    }


def _verify_observations(
    scenarios: Mapping[str, Any],
    observations: Mapping[str, Any],
    *,
    source_artifacts: Sequence[Mapping[str, Any]],
    plugin_definitions: Sequence[Mapping[str, Any]],
    observation_path: Path,
    collector_key: str,
    runtime_attestation: str,
    expected_suite_nonce: str,
) -> dict[tuple[str, int], Mapping[str, Any]]:
    _verify_collector_attestation(observations, key=collector_key)
    if observations.get("schema_version") != OBSERVATION_SCHEMA:
        raise ScenarioContractError("unsupported observation schema_version")
    if observations.get("suite_id") != scenarios["suite_id"]:
        raise ScenarioContractError("observation suite_id does not match scenario contract")
    if observations.get("scenario_contract_sha256") != _sha256(scenarios):
        raise ScenarioContractError("observation scenario contract digest does not match")
    if observations.get("source_artifacts") != list(source_artifacts):
        raise ScenarioContractError("observation source artifact receipts do not match")
    if observations.get("plugin_definitions") != list(plugin_definitions):
        raise ScenarioContractError("observation plugin definition receipts do not match")
    runtime_binding = observations.get("runtime_binding")
    if not isinstance(runtime_binding, dict) or runtime_binding.get(
        "operator_container_runtime_attestation_sha256"
    ) != _sha256_text(runtime_attestation):
        raise ScenarioContractError("observation runtime/container binding does not match")
    stored_document_digest = observations.get("observations_sha256")
    unsigned = dict(observations)
    unsigned.pop("collector_attestation", None)
    unsigned.pop("observations_sha256", None)
    if stored_document_digest != _sha256(unsigned):
        raise ScenarioContractError("observation document digest does not match")
    suite_nonce = observations.get("suite_nonce")
    if not isinstance(suite_nonce, str) or not re.fullmatch(r"[a-f0-9]{64}", suite_nonce):
        raise ScenarioContractError("observation suite_nonce is malformed")
    if not hmac.compare_digest(suite_nonce, expected_suite_nonce):
        raise ScenarioContractError("observation suite_nonce does not match operator challenge")
    by_key: dict[tuple[str, int], Mapping[str, Any]] = {}
    for trial in observations.get("trials", []):
        if not isinstance(trial, dict):
            raise ScenarioContractError("observation trial must be an object")
        digest = trial.get("observation_sha256")
        trial_unsigned = dict(trial)
        trial_unsigned.pop("observation_sha256", None)
        if digest != _sha256(trial_unsigned):
            raise ScenarioContractError("trial observation digest does not match")
        key = (str(trial.get("scenario_id") or ""), trial.get("trial"))
        if key in by_key:
            raise ScenarioContractError(f"duplicate observation trial {key}")
        by_key[key] = trial
    expected = {
        (scenario["scenario_id"], trial)
        for scenario in scenarios["scenarios"]
        for trial in range(1, scenario["repetitions"] + 1)
    }
    if set(by_key) != expected:
        missing = sorted(expected - set(by_key))
        unknown = sorted(set(by_key) - expected)
        raise ScenarioContractError(
            f"observation trial set mismatch: missing={missing}, unknown={unknown}"
        )
    for scenario in scenarios["scenarios"]:
        scenario_trials = [
            by_key[(scenario["scenario_id"], trial)]
            for trial in range(1, scenario["repetitions"] + 1)
        ]
        session_ids = [str(item.get("session_id") or "") for item in scenario_trials]
        attempt_sets = [tuple(item.get("attempt_ids") or ()) for item in scenario_trials]
        stream_digests = [str(item.get("stream_sha256") or "") for item in scenario_trials]
        if any(not value for value in session_ids) or len(set(session_ids)) != len(session_ids):
            raise ScenarioContractError(
                f"{scenario['scenario_id']} trials do not have distinct session receipts"
            )
        if any(len(value) != 1 for value in attempt_sets) or len(set(attempt_sets)) != len(
            attempt_sets
        ):
            raise ScenarioContractError(
                f"{scenario['scenario_id']} trials do not have one distinct attempt receipt each"
            )
        if any(not _SHA_RE.fullmatch(value) for value in stream_digests) or len(
            set(stream_digests)
        ) != len(stream_digests):
            raise ScenarioContractError(
                f"{scenario['scenario_id']} trials contain duplicate or invalid SSE trajectories"
            )
    challenges = [str(item.get("collector_challenge") or "") for item in by_key.values()]
    if any(not re.fullmatch(r"[a-f0-9]{64}", value) for value in challenges) or len(
        set(challenges)
    ) != len(challenges):
        raise ScenarioContractError("collector challenges are missing or cloned")
    if any(item.get("suite_nonce") != suite_nonce for item in by_key.values()):
        raise ScenarioContractError("trial suite_nonce does not match observation document")

    plugin_hashes = {
        str(item["qualified_agent_id"]): str(item["definition_sha256"])
        for item in plugin_definitions
    }
    for trial in by_key.values():
        for receipt in (*trial.get("subagent_starts", []), *trial.get("subagent_finishes", [])):
            profile_id = str(receipt.get("profile_id") or "")
            if not profile_id:
                continue
            expected_hash = plugin_hashes.get(profile_id)
            if expected_hash is None or not hmac.compare_digest(
                str(receipt.get("definition_sha256") or ""), expected_hash
            ):
                raise ScenarioContractError(
                    "runtime plugin definition receipt does not match source"
                )

    _verify_raw_sse_artifact(
        observations,
        by_key=by_key,
        observation_path=observation_path,
        collector_key=collector_key,
    )
    return by_key


def _verify_raw_sse_artifact(
    observations: Mapping[str, Any],
    *,
    by_key: Mapping[tuple[str, int], Mapping[str, Any]],
    observation_path: Path,
    collector_key: str,
) -> None:
    receipt = observations.get("raw_sse_artifact")
    if not isinstance(receipt, dict) or set(receipt) != {"file_name", "content_sha256", "mode"}:
        raise ScenarioContractError("raw SSE artifact receipt is malformed")
    file_name = receipt.get("file_name")
    if (
        not isinstance(file_name, str)
        or Path(file_name).name != file_name
        or not file_name.endswith(".json")
    ):
        raise ScenarioContractError("raw SSE artifact filename is unsafe")
    raw_path = observation_path.resolve().parent / file_name
    if raw_path.is_symlink() or not raw_path.is_file():
        raise ScenarioContractError("raw SSE artifact is missing or symlinked")
    if stat.S_IMODE(raw_path.stat().st_mode) != 0o600 or receipt.get("mode") != "0600":
        raise ScenarioContractError("raw SSE artifact permissions are not 0600")
    observed_file_sha = hashlib.sha256(raw_path.read_bytes()).hexdigest()
    if not hmac.compare_digest(str(receipt.get("content_sha256") or ""), observed_file_sha):
        raise ScenarioContractError("raw SSE artifact file digest does not match")
    raw_document = _load_json(raw_path, label="raw SSE artifact")
    if not isinstance(raw_document, dict):
        raise ScenarioContractError("raw SSE artifact must be an object")
    _verify_collector_attestation(raw_document, key=collector_key)
    if (
        raw_document.get("schema_version") != RAW_SSE_SCHEMA
        or raw_document.get("suite_id") != observations.get("suite_id")
        or raw_document.get("suite_nonce") != observations.get("suite_nonce")
    ):
        raise ScenarioContractError("raw SSE artifact identity does not match")
    raw_digest = raw_document.get("raw_sse_sha256")
    raw_unsigned = dict(raw_document)
    raw_unsigned.pop("collector_attestation", None)
    raw_unsigned.pop("raw_sse_sha256", None)
    if raw_digest != _sha256(raw_unsigned):
        raise ScenarioContractError("raw SSE artifact internal digest does not match")
    raw_by_key: dict[tuple[str, int], Mapping[str, Any]] = {}
    for raw_trial in raw_document.get("trials", []):
        if not isinstance(raw_trial, dict):
            raise ScenarioContractError("raw SSE trial must be an object")
        digest = raw_trial.get("raw_trial_sha256")
        raw_trial_unsigned = dict(raw_trial)
        raw_trial_unsigned.pop("raw_trial_sha256", None)
        if digest != _sha256(raw_trial_unsigned):
            raise ScenarioContractError("raw SSE trial digest does not match")
        key = (str(raw_trial.get("scenario_id") or ""), raw_trial.get("trial"))
        if key in raw_by_key:
            raise ScenarioContractError("raw SSE artifact contains duplicate trials")
        raw_by_key[key] = raw_trial
    if set(raw_by_key) != set(by_key):
        raise ScenarioContractError("raw SSE and observation trial sets differ")
    for key, observation in by_key.items():
        raw_trial = raw_by_key[key]
        for field in ("session_id", "suite_nonce", "collector_challenge", "stream_sha256"):
            if raw_trial.get(field) != observation.get(field):
                raise ScenarioContractError(f"raw SSE trial {key} {field} does not match")
        payloads = raw_trial.get("raw_sse_payloads")
        if not isinstance(payloads, list) or not all(isinstance(item, str) for item in payloads):
            raise ScenarioContractError("raw SSE payload list is malformed")
        stream_hasher = hashlib.sha256()
        parsed_events: list[dict[str, Any]] = []
        for payload in payloads:
            stream_hasher.update(payload.encode("utf-8"))
            stream_hasher.update(b"\n")
            event = _strict_json_loads(payload, label="raw SSE payload")
            if not isinstance(event, dict):
                raise ScenarioContractError("raw SSE payload is not an event object")
            parsed_events.append(event)
        stream_sha = stream_hasher.hexdigest()
        if stream_sha != observation.get("stream_sha256"):
            raise ScenarioContractError("raw SSE payload digest does not match observation")
        reconstructed = summarize_sse_events(parsed_events, stream_sha256=stream_sha)
        for field in (
            "event_counts",
            "candidate_output",
            "candidate_output_sha256",
            "text_events",
            "event_sequence",
            "subagent_starts",
            "subagent_finishes",
            "parallel_overlaps",
            "tool_starts",
            "tool_results",
            "terminal_events",
        ):
            if reconstructed[field] != observation.get(field):
                raise ScenarioContractError(
                    f"raw SSE reconstruction differs from observation field {field}"
                )


def _seal(payload: Mapping[str, Any], *, hmac_key: str | None) -> dict[str, Any]:
    digest = _sha256(payload)
    seal = {"algorithm": "sha256", "digest": digest}
    if hmac_key:
        seal["hmac_algorithm"] = "hmac-sha256"
        seal["hmac_digest"] = hmac.new(
            hmac_key.encode("utf-8"), _canonical_bytes(payload), hashlib.sha256
        ).hexdigest()
    return seal


def _independent_validation_keys() -> tuple[str, str]:
    """Load the two release attestations and reject a shared trust root."""

    collector_key = _attestation_key(
        None,
        environment_name="GENERAL_AGENT_COLLECTOR_HMAC_KEY",
        label="validation",
    )
    golden_key = _attestation_key(
        None,
        environment_name="GENERAL_AGENT_GOLDEN_HMAC_KEY",
        label="golden validation",
    )
    if hmac.compare_digest(collector_key, golden_key):
        raise ScenarioContractError("collector and golden HMAC keys must be different")
    return collector_key, golden_key


def _merged_collector_binding(
    *,
    suite_id: str,
    scenario_contract_sha256: str,
    observations_sha256: str,
    runtime_binding_sha256: str,
    raw_sse_artifact_sha256: str,
    provider_observer_sha256: str,
    suite_nonce_sha256: str,
    coding_host_test_evidence: Mapping[str, Any] | None,
    merged_trials: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    return {
        "schema_version": MERGED_SCHEMA,
        "suite_id": suite_id,
        "scenario_contract_sha256": scenario_contract_sha256,
        "observations_sha256": observations_sha256,
        "runtime_binding_sha256": runtime_binding_sha256,
        "raw_sse_artifact_sha256": raw_sse_artifact_sha256,
        "provider_observer_sha256": provider_observer_sha256,
        "suite_nonce_sha256": suite_nonce_sha256,
        "coding_host_test_evidence": coding_host_test_evidence,
        "trials": [
            {
                "scenario_id": item["observation"]["scenario_id"],
                "trial": item["observation"]["trial"],
                "observation_sha256": item["observation"]["observation_sha256"],
            }
            for item in merged_trials
        ],
    }


def _merged_golden_binding(
    *,
    suite_id: str,
    scenario_contract_sha256: str,
    observations_sha256: str,
    runtime_binding_sha256: str,
    raw_sse_artifact_sha256: str,
    provider_observer_sha256: str,
    suite_nonce_sha256: str,
    validation_sha256: str,
    coding_host_test_evidence: Mapping[str, Any] | None,
    merged_trials: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    return {
        "schema_version": MERGED_SCHEMA,
        "suite_id": suite_id,
        "scenario_contract_sha256": scenario_contract_sha256,
        "observations_sha256": observations_sha256,
        "runtime_binding_sha256": runtime_binding_sha256,
        "raw_sse_artifact_sha256": raw_sse_artifact_sha256,
        "provider_observer_sha256": provider_observer_sha256,
        "suite_nonce_sha256": suite_nonce_sha256,
        "validation_sha256": validation_sha256,
        "coding_host_test_evidence": coding_host_test_evidence,
        "trials": [
            {
                "scenario_id": item["golden_validation"]["scenario_id"],
                "trial": item["golden_validation"]["trial"],
                "observation_sha256": item["golden_validation"]["observation_sha256"],
                "golden_validation_sha256": _sha256(item["golden_validation"]),
            }
            for item in merged_trials
        ],
    }


def validate(
    scenario_path: Path,
    observation_path: Path,
    output_path: Path,
    *,
    require_hmac: bool = True,
) -> dict[str, Any]:
    del require_hmac  # Kept only for source compatibility; release validation is always HMAC-only.
    scenarios = load_scenarios(scenario_path)
    source_artifacts = verify_source_artifacts(
        scenarios, scenario_directory=scenario_path.resolve().parent
    )
    plugin_definitions = verify_plugin_definitions(scenarios)
    collector_key, hmac_key = _independent_validation_keys()
    runtime_attestation = _verified_runtime_attestation()
    expected_suite_nonce = _external_suite_nonce()
    observations = _load_json(observation_path, label="observations")
    if not isinstance(observations, dict):
        raise ScenarioContractError("observations must be an object")
    by_key = _verify_observations(
        scenarios,
        observations,
        source_artifacts=source_artifacts,
        plugin_definitions=plugin_definitions,
        observation_path=observation_path,
        collector_key=collector_key,
        runtime_attestation=runtime_attestation,
        expected_suite_nonce=expected_suite_nonce,
    )
    scenario_by_id = {item["scenario_id"]: item for item in scenarios["scenarios"]}
    results = [
        _validated_trial(scenario_by_id[scenario_id], by_key[(scenario_id, trial)])
        for scenario_id, trial in sorted(by_key)
    ]
    payload = {
        "schema_version": VALIDATION_SCHEMA,
        "suite_id": scenarios["suite_id"],
        "scenario_contract_sha256": _sha256(scenarios),
        "observations_sha256": observations["observations_sha256"],
        "validator": {
            "version": VALIDATOR_VERSION,
            "executable_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
            "semantic_model_used": False,
        },
        "source_artifacts": source_artifacts,
        "plugin_definitions": plugin_definitions,
        "collector_attestation_key_id": hashlib.sha256(collector_key.encode("utf-8")).hexdigest()[
            :24
        ],
        "domain_macro": _domain_macro(scenarios, results),
        "trials": results,
    }
    document = {**payload, "seal": _seal(payload, hmac_key=hmac_key)}
    _safe_write_json(output_path, document)
    return document


def _verify_seal(
    validation: Mapping[str, Any],
    *,
    hmac_key: str | None = None,
    require_hmac: bool = True,
) -> tuple[dict[str, Any], str]:
    del require_hmac  # Kept for source compatibility; validation is always HMAC-only.
    if hmac_key is None:
        hmac_key = _attestation_key(
            None,
            environment_name="GENERAL_AGENT_GOLDEN_HMAC_KEY",
            label="golden validation",
        )
    seal = validation.get("seal")
    if not isinstance(seal, dict):
        raise ScenarioContractError("validation seal is missing")
    payload = dict(validation)
    payload.pop("seal", None)
    digest = seal.get("digest")
    if seal.get("algorithm") != "sha256" or not isinstance(digest, str):
        raise ScenarioContractError("validation SHA-256 seal is malformed")
    if not hmac.compare_digest(digest, _sha256(payload)):
        raise ScenarioContractError("validation SHA-256 seal does not match")
    hmac_digest = seal.get("hmac_digest")
    if hmac_digest is None:
        raise ScenarioContractError("validation is hash-only but HMAC is required")
    if seal.get("hmac_algorithm") != "hmac-sha256" or not isinstance(hmac_digest, str):
        raise ScenarioContractError("validation HMAC seal is malformed")
    expected = hmac.new(
        hmac_key.encode("utf-8"), _canonical_bytes(payload), hashlib.sha256
    ).hexdigest()
    if not hmac.compare_digest(hmac_digest, expected):
        raise ScenarioContractError("validation HMAC seal does not match")
    return payload, "hmac-sha256"


def merge(
    scenario_path: Path,
    observation_path: Path,
    validation_path: Path,
    output_path: Path,
    *,
    require_hmac: bool = True,
    coding_host_test_receipt_path: Path | None = None,
) -> dict[str, Any]:
    del require_hmac  # Kept only for source compatibility; release merge is always HMAC-only.
    scenarios = load_scenarios(scenario_path)
    source_artifacts = verify_source_artifacts(
        scenarios, scenario_directory=scenario_path.resolve().parent
    )
    plugin_definitions = verify_plugin_definitions(scenarios)
    collector_key, golden_key = _independent_validation_keys()
    runtime_attestation = _verified_runtime_attestation()
    expected_suite_nonce = _external_suite_nonce()
    observations = _load_json(observation_path, label="observations")
    validation = _load_json(validation_path, label="golden validation")
    if not isinstance(observations, dict) or not isinstance(validation, dict):
        raise ScenarioContractError("observations and validation must be objects")
    by_key = _verify_observations(
        scenarios,
        observations,
        source_artifacts=source_artifacts,
        plugin_definitions=plugin_definitions,
        observation_path=observation_path,
        collector_key=collector_key,
        runtime_attestation=runtime_attestation,
        expected_suite_nonce=expected_suite_nonce,
    )
    validation_payload, seal_strength = _verify_seal(validation, hmac_key=golden_key)
    if validation_payload.get("schema_version") != VALIDATION_SCHEMA:
        raise ScenarioContractError("unsupported validation schema_version")
    if validation_payload.get("suite_id") != scenarios["suite_id"]:
        raise ScenarioContractError("validation suite_id does not match")
    if validation_payload.get("scenario_contract_sha256") != _sha256(scenarios):
        raise ScenarioContractError("validation scenario digest does not match")
    if validation_payload.get("observations_sha256") != observations["observations_sha256"]:
        raise ScenarioContractError("validation is bound to different observations")
    if validation_payload.get("source_artifacts") != source_artifacts:
        raise ScenarioContractError("validation source artifact receipts do not match")
    if validation_payload.get("plugin_definitions") != plugin_definitions:
        raise ScenarioContractError("validation plugin definition receipts do not match")
    expected_collector_key_id = hashlib.sha256(collector_key.encode("utf-8")).hexdigest()[:24]
    if validation_payload.get("collector_attestation_key_id") != expected_collector_key_id:
        raise ScenarioContractError("validation collector attestation identity does not match")
    validation_by_key: dict[tuple[str, int], Mapping[str, Any]] = {}
    for result in validation_payload.get("trials", []):
        if not isinstance(result, dict):
            raise ScenarioContractError("validation trial must be an object")
        key = (str(result.get("scenario_id") or ""), result.get("trial"))
        if key in validation_by_key:
            raise ScenarioContractError(f"duplicate validation trial {key}")
        validation_by_key[key] = result
    if set(validation_by_key) != set(by_key):
        raise ScenarioContractError("validation and observation trial sets do not match")
    scenario_by_id = {item["scenario_id"]: item for item in scenarios["scenarios"]}
    recomputed_results = [
        _validated_trial(scenario_by_id[scenario_id], by_key[(scenario_id, trial)])
        for scenario_id, trial in sorted(by_key)
    ]
    if validation_payload.get("trials") != recomputed_results:
        raise ScenarioContractError("validation trials differ from host recomputation")
    if validation_payload.get("domain_macro") != _domain_macro(scenarios, recomputed_results):
        raise ScenarioContractError("validation domain macro differs from host recomputation")
    merged_trials = []
    for key in sorted(by_key):
        result = validation_by_key[key]
        if result.get("observation_sha256") != by_key[key]["observation_sha256"]:
            raise ScenarioContractError(f"validation trial {key} is bound to another observation")
        merged_trials.append({"observation": by_key[key], "golden_validation": result})
    coding_scenarios = [
        scenario
        for scenario in scenarios["scenarios"]
        if scenario["scenario_id"] == CODING_SCENARIO_ID
    ]
    coding_host_test_evidence: dict[str, Any] | None = None
    if coding_scenarios:
        if len(coding_scenarios) != 1 or coding_host_test_receipt_path is None:
            raise ScenarioContractError("coding release merge requires --coding-host-test-receipt")
        coding_key = _attestation_key(
            None,
            environment_name=CODING_HOST_TEST_HMAC_ENVIRONMENT_NAME,
            label="coding host-test",
        )
        if hmac.compare_digest(coding_key, collector_key) or hmac.compare_digest(
            coding_key, golden_key
        ):
            raise ScenarioContractError(
                "coding host-test, collector, and golden HMAC keys must be different"
            )
        coding_by_trial = {
            int(trial): {
                "observation_sha256": by_key[(CODING_SCENARIO_ID, trial)]["observation_sha256"],
                "execution_checks": _execution_checks(
                    coding_scenarios[0], by_key[(CODING_SCENARIO_ID, trial)]
                ),
            }
            for trial in range(1, 4)
        }
        try:
            coding_host_test_evidence = verify_coding_host_test_receipt(
                coding_host_test_receipt_path,
                hmac_key=coding_key,
                expected_scenario_contract_sha256=_sha256(scenarios),
                expected_observations_sha256=observations["observations_sha256"],
                expected_collector_attestation_key_id=expected_collector_key_id,
                expected_trials=coding_by_trial,
                expected_source_artifacts=source_artifacts,
                expected_plugin_definitions=plugin_definitions,
                expected_runtime_binding=observations["runtime_binding"],
                expected_raw_sse_artifact=observations["raw_sse_artifact"],
            )
        except CodingHostReceiptError as exc:
            raise ScenarioContractError(str(exc)) from exc
    scenario_digest = _sha256(scenarios)
    observations_digest = observations["observations_sha256"]
    validation_digest = validation["seal"]["digest"]
    runtime_binding_digest = _sha256(observations["runtime_binding"])
    raw_sse_artifact_digest = str(observations["raw_sse_artifact"]["content_sha256"])
    provider_observer_digest = _sha256(
        {
            "collector": observations["collector"],
            "runtime_binding": observations["runtime_binding"],
            "source_artifacts": observations["source_artifacts"],
            "plugin_definitions": observations["plugin_definitions"],
            "raw_sse_artifact": observations["raw_sse_artifact"],
            "trials": [
                {
                    "scenario_id": item["scenario_id"],
                    "trial": item["trial"],
                    "session_id": item["session_id"],
                    "stream_sha256": item["stream_sha256"],
                    "observation_sha256": item["observation_sha256"],
                }
                for item in observations["trials"]
            ],
        }
    )
    suite_nonce_digest = _sha256_text(expected_suite_nonce)
    collector_attestation = _collector_attestation(
        _merged_collector_binding(
            suite_id=scenarios["suite_id"],
            scenario_contract_sha256=scenario_digest,
            observations_sha256=observations_digest,
            runtime_binding_sha256=runtime_binding_digest,
            raw_sse_artifact_sha256=raw_sse_artifact_digest,
            provider_observer_sha256=provider_observer_digest,
            suite_nonce_sha256=suite_nonce_digest,
            coding_host_test_evidence=coding_host_test_evidence,
            merged_trials=merged_trials,
        ),
        key=collector_key,
    )
    golden_attestation = _collector_attestation(
        _merged_golden_binding(
            suite_id=scenarios["suite_id"],
            scenario_contract_sha256=scenario_digest,
            observations_sha256=observations_digest,
            runtime_binding_sha256=runtime_binding_digest,
            raw_sse_artifact_sha256=raw_sse_artifact_digest,
            provider_observer_sha256=provider_observer_digest,
            suite_nonce_sha256=suite_nonce_digest,
            validation_sha256=validation_digest,
            coding_host_test_evidence=coding_host_test_evidence,
            merged_trials=merged_trials,
        ),
        key=golden_key,
    )
    document = {
        "schema_version": MERGED_SCHEMA,
        "suite_id": scenarios["suite_id"],
        "provenance": {
            "scenario_contract_sha256": scenario_digest,
            "observations_sha256": observations_digest,
            "runtime_binding_sha256": runtime_binding_digest,
            "raw_sse_artifact_sha256": raw_sse_artifact_digest,
            "provider_observer_sha256": provider_observer_digest,
            "suite_nonce_sha256": suite_nonce_digest,
            "validation_sha256": validation_digest,
            "validation_seal_strength": seal_strength,
            "coding_host_test_evidence": coding_host_test_evidence,
            "collector_attestation": collector_attestation,
            "golden_attestation": golden_attestation,
        },
        "trials": merged_trials,
    }
    document["merged_receipt_sha256"] = _sha256(document)
    _safe_write_json(output_path, document)
    return document


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    collect_parser = subparsers.add_parser("collect", help="collect factual Gateway SSE receipts")
    collect_parser.add_argument("--scenarios", type=Path, required=True)
    collect_parser.add_argument("--output", type=Path, required=True)
    validate_parser = subparsers.add_parser("validate", help="run independent golden assertions")
    validate_parser.add_argument("--scenarios", type=Path, required=True)
    validate_parser.add_argument("--observations", type=Path, required=True)
    validate_parser.add_argument("--output", type=Path, required=True)
    merge_parser = subparsers.add_parser("merge", help="verify and merge observations + goldens")
    merge_parser.add_argument("--scenarios", type=Path, required=True)
    merge_parser.add_argument("--observations", type=Path, required=True)
    merge_parser.add_argument("--validation", type=Path, required=True)
    merge_parser.add_argument("--output", type=Path, required=True)
    merge_parser.add_argument("--coding-host-test-receipt", type=Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        if args.command == "collect":
            collect(args.scenarios, args.output)
        elif args.command == "validate":
            validate(args.scenarios, args.observations, args.output)
        else:
            merge(
                args.scenarios,
                args.observations,
                args.validation,
                args.output,
                coding_host_test_receipt_path=args.coding_host_test_receipt,
            )
    except (ScenarioContractError, httpx.HTTPError, OSError, ValueError) as exc:
        print(f"real scenario runner failed closed: {type(exc).__name__}: {exc}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
