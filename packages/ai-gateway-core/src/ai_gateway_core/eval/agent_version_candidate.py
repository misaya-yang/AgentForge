"""Closed, provider-free Agent release candidate and gate contracts.

The browser can request an evaluation, but it cannot submit trusted Agent
configuration, runtime fingerprints, profile selection, or a passing result.
Those values are derived from a saved Draft and the Gateway-owned runtime
Snapshot before this module receives them.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
from datetime import date, datetime, timezone
from typing import Any, Final

from ai_gateway_contracts.agent_runtime import runtime_sha256

AGENT_RELEASE_CANDIDATE_SCHEMA_VERSION: Final = "agent-release-candidate/v1"
AGENT_RELEASE_GATE_SCHEMA_VERSION: Final = "agent-release-gate/v1"
AGENT_RELEASE_DIFF_SCHEMA_VERSION: Final = "agent-release-diff/v1"
OFFLINE_RELEASE_PROFILE_ID: Final = "offline_v1"
OFFLINE_RELEASE_PROFILE_VERSION: Final = "2026-07-19"

_REQUIRED_RUNTIME_FINGERPRINT_KEYS: Final = (
    "spec_hash",
    "model_id",
    "provider_id",
    "model_authorization_hash",
    "prompt_hash",
    "tool_schema_hash",
    "skill_manifest_hash",
    "knowledge_revision",
    "eval_dataset_manifest_hash",
    "runtime_version",
    "snapshot_hash",
    "channel_policy_hash",
)


class AgentReleaseCandidateError(ValueError):
    """The server-resolved candidate is incomplete or internally inconsistent."""


class AgentReleaseProfileUnavailableError(RuntimeError):
    """The selected server-side profile cannot truthfully execute."""


def _canonical_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def _plain_sha256(value: Any) -> str:
    return hashlib.sha256(_canonical_json(value).encode("utf-8")).hexdigest()


def _hash_value(value: Any) -> str:
    return runtime_sha256(value)


def _without_prefix(value: Any) -> str:
    raw = str(value or "")
    return raw.removeprefix("sha256:")


def _stable_scalar(value: Any) -> Any:
    if isinstance(value, datetime | date):
        return value.isoformat()
    return value


def build_model_authorization_evidence(
    *,
    source: str,
    model_id: str,
    provider_id: str,
    access_level: str,
    model_enabled: bool,
    provider_enabled: bool,
    runtime_provider_configured: bool,
    model_updated_at: Any = None,
    provider_updated_at: Any = None,
) -> dict[str, Any]:
    """Create the prompt/Secret-free model authorization token used by release gates."""

    catalog_state = {
        "model_id": str(model_id),
        "provider_id": str(provider_id),
        "access_level": str(access_level or "public"),
        "model_enabled": bool(model_enabled),
        "provider_enabled": bool(provider_enabled),
        "runtime_provider_configured": bool(runtime_provider_configured),
        "model_updated_at": _stable_scalar(model_updated_at),
        "provider_updated_at": _stable_scalar(provider_updated_at),
    }
    return {
        "source": str(source),
        "model_id": str(model_id),
        "provider_id": str(provider_id),
        "access_level": str(access_level or "public"),
        "catalog_state_hash": _plain_sha256(catalog_state),
        "runtime_provider_configured": bool(runtime_provider_configured),
    }


def server_release_profile() -> dict[str, Any]:
    """Return the server-owned release profile; clients never choose it.

    The open-source default is deterministic and provider-free. A production
    profile deliberately fails closed until the deployment supplies an
    approved dataset, thresholds, and an execution adapter in AS-08.
    """

    configured = os.getenv("AGENT_RELEASE_PROFILE", OFFLINE_RELEASE_PROFILE_ID).strip()
    if configured in {"", OFFLINE_RELEASE_PROFILE_ID}:
        return {
            "profile_id": OFFLINE_RELEASE_PROFILE_ID,
            "profile_version": OFFLINE_RELEASE_PROFILE_VERSION,
            "available": True,
            "execution_scope": "provider_free_release_integrity",
            "model_quality_evaluated": False,
            "blocking_evaluators": [
                "candidate_integrity",
                "resource_readiness",
                "secret_safety",
            ],
            "critical_pass_rate": 1.0,
        }
    return {
        "profile_id": configured,
        "profile_version": "unconfigured",
        "available": False,
        "execution_scope": "production_unavailable",
        "model_quality_evaluated": False,
        "blocking_evaluators": [],
        "critical_pass_rate": 1.0,
    }


def require_available_release_profile() -> dict[str, Any]:
    profile = server_release_profile()
    if not profile["available"]:
        raise AgentReleaseProfileUnavailableError("AGENT_RELEASE_PROFILE_UNAVAILABLE")
    return profile


def _normalized_policy(policy: Any) -> dict[str, Any]:
    raw = policy if isinstance(policy, dict) else {}
    origins = sorted(
        {
            str(origin).strip()
            for origin in raw.get("allowed_origins", [])
            if isinstance(origin, str) and str(origin).strip()
        }
    )
    normalized = {
        "attachments": bool(raw.get("attachments", False)),
        "high_risk_tools": bool(raw.get("high_risk_tools", False)),
        "allowed_origins": origins,
    }
    expiry = raw.get("expires_at")
    if expiry is not None:
        try:
            parsed = expiry if isinstance(expiry, datetime) else datetime.fromisoformat(
                str(expiry).replace("Z", "+00:00")
            )
        except ValueError as exc:
            raise AgentReleaseCandidateError("AGENT_RELEASE_PUBLIC_EXPIRY_INVALID") from exc
        if parsed.tzinfo is None or parsed.utcoffset() is None:
            raise AgentReleaseCandidateError("AGENT_RELEASE_PUBLIC_EXPIRY_INVALID")
        normalized["expires_at"] = parsed.astimezone(timezone.utc).isoformat()
    return normalized


def _normalized_capability_key(raw: Any) -> tuple[str, str, str, str | None] | None:
    if not isinstance(raw, dict):
        return None
    raw_type = str(raw.get("capability_type") or raw.get("type") or "")
    runtime_type = "platform" if raw_type in {"native", "model_native"} else raw_type
    resource_id = str(raw.get("resource_id") or raw.get("id") or "")
    if runtime_type not in {"platform", "mcp", "skill", "connector"} or not resource_id:
        return None
    version = str(raw.get("resource_version") or raw.get("version") or "")
    schema_hash = str(raw.get("schema_hash") or "") or None
    if schema_hash and not schema_hash.startswith("sha256:"):
        schema_hash = f"sha256:{schema_hash}"
    return runtime_type, resource_id, version, schema_hash


def _expected_runtime_capabilities(
    resolution: dict[str, Any],
    policy: dict[str, Any],
) -> set[tuple[str, str, str, str | None]]:
    expected: set[tuple[str, str, str, str | None]] = set()
    for raw in resolution.get("capabilities") or []:
        if not isinstance(raw, dict):
            continue
        raw_type = str(raw.get("capability_type") or raw.get("type") or "")
        if raw_type == "knowledge":
            continue
        config = raw.get("config") if isinstance(raw.get("config"), dict) else {}
        runtime_type = "platform" if raw_type in {"native", "model_native"} else raw_type
        risk = str(
            raw.get("risk")
            or config.get("risk")
            or ("low" if runtime_type == "platform" else "high")
        )
        if risk not in {"low", "medium", "high", "critical"}:
            raise AgentReleaseCandidateError("AGENT_RELEASE_CAPABILITY_RISK_INVALID")
        if risk in {"high", "critical"} and not policy["high_risk_tools"]:
            continue
        key = _normalized_capability_key(raw)
        if key is None:
            raise AgentReleaseCandidateError("AGENT_RELEASE_CAPABILITY_IDENTITY_INVALID")
        expected.add(key)
    return expected


def build_agent_version_candidate(
    *,
    resolution: dict[str, Any],
    runtime_snapshot: dict[str, Any],
    channel: str,
    auth_mode: str,
    channel_policy: dict[str, Any] | None,
    dataset_id: str | None,
    dataset_snapshot: dict[str, Any] | None = None,
    model_authorization: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Build a prompt-free release identity from one exact saved Draft."""

    agent = resolution.get("agent") if isinstance(resolution.get("agent"), dict) else {}
    draft = resolution.get("draft") if isinstance(resolution.get("draft"), dict) else {}
    fingerprints = (
        runtime_snapshot.get("fingerprints")
        if isinstance(runtime_snapshot.get("fingerprints"), dict)
        else {}
    )
    model = (
        runtime_snapshot.get("model")
        if isinstance(runtime_snapshot.get("model"), dict)
        else {}
    )
    instructions = (
        runtime_snapshot.get("instructions")
        if isinstance(runtime_snapshot.get("instructions"), dict)
        else {}
    )
    policy = _normalized_policy(channel_policy)
    draft_spec_hash = _without_prefix(draft.get("spec_hash"))
    snapshot_spec_hash = _without_prefix(fingerprints.get("spec"))
    if not draft.get("draft_id") or not draft.get("revision") or not draft_spec_hash:
        raise AgentReleaseCandidateError("AGENT_RELEASE_DRAFT_IDENTITY_INCOMPLETE")
    if snapshot_spec_hash != draft_spec_hash:
        raise AgentReleaseCandidateError("AGENT_RELEASE_SPEC_HASH_MISMATCH")
    if str(runtime_snapshot.get("agent_id") or "") != str(agent.get("agent_id") or ""):
        raise AgentReleaseCandidateError("AGENT_RELEASE_AGENT_ID_MISMATCH")
    if str(runtime_snapshot.get("tenant_id") or "") != str(agent.get("tenant_id") or ""):
        raise AgentReleaseCandidateError("AGENT_RELEASE_TENANT_ID_MISMATCH")
    if str(runtime_snapshot.get("schema_version") or "") != "agent-runtime/v1":
        raise AgentReleaseCandidateError("AGENT_RELEASE_RUNTIME_VERSION_UNSUPPORTED")
    snapshot_publication = (
        runtime_snapshot.get("publication")
        if isinstance(runtime_snapshot.get("publication"), dict)
        else {}
    )
    if str(snapshot_publication.get("channel") or "") != channel:
        raise AgentReleaseCandidateError("AGENT_RELEASE_CHANNEL_MISMATCH")
    if str(snapshot_publication.get("auth_mode") or "") != auth_mode:
        raise AgentReleaseCandidateError("AGENT_RELEASE_AUTH_MODE_MISMATCH")
    snapshot_policy = _normalized_policy(runtime_snapshot.get("channel_policy"))
    if snapshot_policy != policy:
        raise AgentReleaseCandidateError("AGENT_RELEASE_CHANNEL_POLICY_MISMATCH")
    expected_capabilities = _expected_runtime_capabilities(resolution, policy)
    actual_capabilities = {
        key
        for raw in runtime_snapshot.get("capabilities") or []
        if (key := _normalized_capability_key(raw)) is not None
    }
    if actual_capabilities != expected_capabilities:
        raise AgentReleaseCandidateError("AGENT_RELEASE_CAPABILITY_SET_MISMATCH")
    expected_datasets = {
        str(item.get("dataset_id") or "")
        for item in resolution.get("knowledge") or []
        if isinstance(item, dict) and item.get("dataset_id")
    }
    snapshot_knowledge = (
        runtime_snapshot.get("knowledge")
        if isinstance(runtime_snapshot.get("knowledge"), dict)
        else {}
    )
    actual_datasets = {
        str(dataset_id)
        for dataset_id in snapshot_knowledge.get("datasets") or []
        if str(dataset_id)
    }
    if actual_datasets != expected_datasets:
        raise AgentReleaseCandidateError("AGENT_RELEASE_KNOWLEDGE_SET_MISMATCH")

    selected_dataset_id = str(dataset_id or "") or None
    if selected_dataset_id:
        selected_dataset = dataset_snapshot if isinstance(dataset_snapshot, dict) else {}
        if str(selected_dataset.get("dataset_id") or "") != selected_dataset_id:
            raise AgentReleaseCandidateError("AGENT_RELEASE_EVAL_DATASET_ID_MISMATCH")
        if str(selected_dataset.get("tenant_id") or "") != str(agent.get("tenant_id") or ""):
            raise AgentReleaseCandidateError("AGENT_RELEASE_EVAL_DATASET_TENANT_MISMATCH")
        dataset_version = str(selected_dataset.get("version") or "").strip()
        dataset_manifest_hash = _without_prefix(selected_dataset.get("manifest_hash"))
        if not dataset_version or not re.fullmatch(r"[0-9a-f]{64}", dataset_manifest_hash):
            raise AgentReleaseCandidateError("AGENT_RELEASE_EVAL_DATASET_SNAPSHOT_INVALID")
    else:
        if dataset_snapshot:
            raise AgentReleaseCandidateError("AGENT_RELEASE_EVAL_DATASET_UNEXPECTED")
        dataset_version = None
        dataset_manifest_hash = None

    raw_model_authorization = (
        model_authorization if isinstance(model_authorization, dict) else {}
    )
    normalized_model_authorization = {
        "source": str(raw_model_authorization.get("source") or "runtime_snapshot"),
        "model_id": str(raw_model_authorization.get("model_id") or model.get("id") or ""),
        "provider_id": str(
            raw_model_authorization.get("provider_id") or model.get("provider") or ""
        ),
        "access_level": str(raw_model_authorization.get("access_level") or "public"),
        "catalog_state_hash": _without_prefix(
            raw_model_authorization.get("catalog_state_hash")
            or _plain_sha256(
                {
                    "model_id": str(model.get("id") or ""),
                    "provider_id": str(model.get("provider") or ""),
                }
            )
        ),
        "runtime_provider_configured": bool(
            raw_model_authorization.get("runtime_provider_configured", True)
        ),
    }
    if (
        normalized_model_authorization["model_id"] != str(model.get("id") or "")
        or normalized_model_authorization["provider_id"]
        != str(model.get("provider") or "")
        or not re.fullmatch(
            r"[0-9a-f]{64}", normalized_model_authorization["catalog_state_hash"]
        )
    ):
        raise AgentReleaseCandidateError("AGENT_RELEASE_MODEL_AUTHORIZATION_INVALID")
    model_authorization_hash = _plain_sha256(normalized_model_authorization)
    eval_dataset_fingerprint = dataset_manifest_hash or _plain_sha256(
        {"dataset_id": None}
    )

    runtime_fingerprint = {
        "spec_hash": draft_spec_hash,
        "model_id": str(model.get("id") or ""),
        "provider_id": str(model.get("provider") or ""),
        "model_authorization_hash": model_authorization_hash,
        "prompt_hash": str(instructions.get("prompt_hash") or ""),
        "tool_schema_hash": str(fingerprints.get("tool_schema") or ""),
        "skill_manifest_hash": str(fingerprints.get("skills") or ""),
        "knowledge_revision": str(fingerprints.get("knowledge_revision") or ""),
        "eval_dataset_manifest_hash": eval_dataset_fingerprint,
        "runtime_version": str(runtime_snapshot.get("schema_version") or ""),
        "snapshot_hash": _hash_value(runtime_snapshot),
        "channel_policy_hash": _hash_value(policy),
    }
    missing = [key for key in _REQUIRED_RUNTIME_FINGERPRINT_KEYS if not runtime_fingerprint[key]]
    if missing:
        raise AgentReleaseCandidateError(
            "AGENT_RELEASE_FINGERPRINT_INCOMPLETE:" + ",".join(sorted(missing))
        )

    release_identity = {
        "agent_id": str(agent.get("agent_id") or ""),
        "draft_id": str(draft["draft_id"]),
        "draft_revision": int(draft["revision"]),
        "spec_hash": draft_spec_hash,
        "model_id": runtime_fingerprint["model_id"],
        "provider_id": runtime_fingerprint["provider_id"],
        "model_authorization_hash": runtime_fingerprint["model_authorization_hash"],
        "prompt_hash": runtime_fingerprint["prompt_hash"],
        "tool_schema_hash": runtime_fingerprint["tool_schema_hash"],
        "skill_manifest_hash": runtime_fingerprint["skill_manifest_hash"],
        "knowledge_revision": runtime_fingerprint["knowledge_revision"],
        "eval_dataset_id": selected_dataset_id,
        "eval_dataset_version": dataset_version,
        "eval_dataset_manifest_hash": dataset_manifest_hash,
        "runtime_version": runtime_fingerprint["runtime_version"],
    }
    evaluation_identity = {
        **release_identity,
        "channel": channel,
        "auth_mode": auth_mode,
        "channel_policy_hash": _plain_sha256(policy),
        "runtime_fingerprint_hash": _plain_sha256(runtime_fingerprint),
    }
    return {
        "schema_version": AGENT_RELEASE_CANDIDATE_SCHEMA_VERSION,
        "tenant_id": str(agent.get("tenant_id") or ""),
        "agent_id": str(agent.get("agent_id") or ""),
        "draft_id": str(draft["draft_id"]),
        "draft_revision": int(draft["revision"]),
        "spec_hash": draft_spec_hash,
        "channel": channel,
        "auth_mode": auth_mode,
        "channel_policy": policy,
        "channel_policy_hash": _plain_sha256(policy),
        "dataset_id": selected_dataset_id,
        "dataset_version": dataset_version,
        "dataset_manifest_hash": dataset_manifest_hash,
        "model_authorization": normalized_model_authorization,
        "runtime_fingerprint": runtime_fingerprint,
        "runtime_fingerprint_hash": _plain_sha256(runtime_fingerprint),
        "release_identity_hash": _plain_sha256(release_identity),
        "evaluation_identity_hash": _plain_sha256(evaluation_identity),
    }


def evaluate_agent_version_candidate(
    candidate: dict[str, Any],
    *,
    profile: dict[str, Any] | None = None,
    validation_duration_ms: float = 0.0,
) -> dict[str, Any]:
    """Apply the deterministic release-integrity gate to a closed candidate."""

    selected = profile or require_available_release_profile()
    if not selected.get("available"):
        raise AgentReleaseProfileUnavailableError("AGENT_RELEASE_PROFILE_UNAVAILABLE")
    blocking: list[dict[str, str]] = []
    fingerprint = (
        candidate.get("runtime_fingerprint")
        if isinstance(candidate.get("runtime_fingerprint"), dict)
        else {}
    )
    for key in _REQUIRED_RUNTIME_FINGERPRINT_KEYS:
        if not fingerprint.get(key):
            blocking.append(
                {
                    "code": "AGENT_EVAL_FINGERPRINT_INCOMPLETE",
                    "field": f"runtime_fingerprint.{key}",
                    "message": "A required server runtime fingerprint is missing",
                }
            )
    if candidate.get("runtime_fingerprint_hash") != _plain_sha256(fingerprint):
        blocking.append(
            {
                "code": "AGENT_EVAL_FINGERPRINT_HASH_MISMATCH",
                "field": "runtime_fingerprint_hash",
                "message": "Runtime fingerprint evidence is inconsistent",
            }
        )
    if candidate.get("channel_policy_hash") != _plain_sha256(
        candidate.get("channel_policy") or {}
    ):
        blocking.append(
            {
                "code": "AGENT_EVAL_CHANNEL_POLICY_MISMATCH",
                "field": "channel_policy_hash",
                "message": "Channel policy evidence is inconsistent",
            }
        )
    model_authorization = (
        candidate.get("model_authorization")
        if isinstance(candidate.get("model_authorization"), dict)
        else {}
    )
    if fingerprint.get("model_authorization_hash") != _plain_sha256(
        model_authorization
    ):
        blocking.append(
            {
                "code": "AGENT_EVAL_MODEL_AUTHORIZATION_MISMATCH",
                "field": "runtime_fingerprint.model_authorization_hash",
                "message": "Model authorization evidence is inconsistent",
            }
        )
    if candidate.get("dataset_id"):
        manifest_hash = _without_prefix(candidate.get("dataset_manifest_hash"))
        if (
            not candidate.get("dataset_version")
            or not re.fullmatch(r"[0-9a-f]{64}", manifest_hash)
            or fingerprint.get("eval_dataset_manifest_hash") != manifest_hash
        ):
            blocking.append(
                {
                    "code": "AGENT_EVAL_DATASET_FINGERPRINT_MISMATCH",
                    "field": "dataset_manifest_hash",
                    "message": "Selected Eval Dataset evidence is inconsistent",
                }
            )
    if candidate.get("channel") not in {"hosted", "embed", "api"}:
        blocking.append(
            {
                "code": "AGENT_EVAL_CHANNEL_INVALID",
                "field": "channel",
                "message": "Release channel is invalid",
            }
        )

    non_blocking = [
        {
            "code": "AGENT_EVAL_PROVIDER_FREE_SCOPE",
            "field": "profile",
            "message": (
                "This open-source gate verifies release integrity and resource readiness; "
                "it does not claim live-model quality."
            ),
        }
    ]
    if not candidate.get("dataset_id"):
        non_blocking.append(
            {
                "code": "AGENT_EVAL_DATASET_NOT_SELECTED",
                "field": "dataset_id",
                "message": "No tenant Eval Dataset is attached to this provider-free run",
            }
        )
    status = "failed" if blocking else "passed"
    evaluator_results = [
        {
            "evaluator": evaluator,
            "blocking": True,
            "status": "failed" if blocking else "passed",
        }
        for evaluator in selected.get("blocking_evaluators", [])
    ]
    return {
        "schema_version": AGENT_RELEASE_GATE_SCHEMA_VERSION,
        "status": status,
        "profile_id": str(selected["profile_id"]),
        "profile_version": str(selected["profile_version"]),
        "execution_scope": str(selected["execution_scope"]),
        "model_quality_evaluated": bool(selected.get("model_quality_evaluated", False)),
        "blocking_findings": blocking,
        "non_blocking_findings": non_blocking,
        "metrics": {
            "critical_pass_rate": 0.0 if blocking else 1.0,
            "configured_critical_pass_rate": float(selected["critical_pass_rate"]),
            "validation_duration_ms": max(0.0, round(float(validation_duration_ms), 3)),
            "provider_cost_cents": 0.0,
            "evaluator_results": evaluator_results,
        },
    }


def _binding_summary(value: Any, *, kind: str) -> list[dict[str, Any]]:
    rows = value if isinstance(value, list) else []
    summary: list[dict[str, Any]] = []
    for raw in rows:
        if not isinstance(raw, dict):
            continue
        if kind == "knowledge":
            resource_id = str(raw.get("dataset_id") or "")
            resource_type = "knowledge"
            version = None
            schema_hash = None
            config = raw.get("retrieval_config") or {}
        else:
            resource_id = str(raw.get("resource_id") or "")
            resource_type = str(raw.get("type") or "")
            version = raw.get("resource_version")
            schema_hash = raw.get("schema_hash")
            config = raw.get("config") or {}
        if not resource_id:
            continue
        summary.append(
            {
                "type": resource_type,
                "resource_id": resource_id,
                "resource_version": version,
                "schema_hash": schema_hash,
                "config_hash": _hash_value(config),
            }
        )
    return sorted(summary, key=lambda item: (item["type"], item["resource_id"]))


def _section(before: Any, after: Any, *, changed_paths: list[str]) -> dict[str, Any]:
    return {
        "changed": before != after,
        "before_hash": _hash_value(before),
        "after_hash": _hash_value(after),
        "changed_paths": changed_paths if before != after else [],
    }


def structured_agent_release_diff(
    before_spec: dict[str, Any] | None,
    after_spec: dict[str, Any],
) -> dict[str, Any]:
    """Return a deterministic, prompt-free structured Version diff."""

    before = before_spec if isinstance(before_spec, dict) else {}
    after = after_spec if isinstance(after_spec, dict) else {}
    before_identity = before.get("identity") if isinstance(before.get("identity"), dict) else {}
    after_identity = after.get("identity") if isinstance(after.get("identity"), dict) else {}
    before_outcome = before.get("outcome") if isinstance(before.get("outcome"), dict) else {}
    after_outcome = after.get("outcome") if isinstance(after.get("outcome"), dict) else {}
    before_model = before.get("model") if isinstance(before.get("model"), dict) else {}
    after_model = after.get("model") if isinstance(after.get("model"), dict) else {}
    before_capabilities = _binding_summary(before.get("capabilities"), kind="capability")
    after_capabilities = _binding_summary(after.get("capabilities"), kind="capability")
    before_knowledge = _binding_summary(before.get("knowledge"), kind="knowledge")
    after_knowledge = _binding_summary(after.get("knowledge"), kind="knowledge")
    before_memory = before.get("memory") if isinstance(before.get("memory"), dict) else {}
    after_memory = after.get("memory") if isinstance(after.get("memory"), dict) else {}
    sections = {
        "identity": _section(
            before_identity,
            after_identity,
            changed_paths=[
                f"identity.{key}"
                for key in sorted(set(before_identity) | set(after_identity))
                if before_identity.get(key) != after_identity.get(key)
            ],
        ),
        "outcome": _section(before_outcome, after_outcome, changed_paths=["outcome"]),
        "prompt": {
            **_section(
                str(before.get("instructions") or ""),
                str(after.get("instructions") or ""),
                changed_paths=["instructions"],
            ),
            "before_length": len(str(before.get("instructions") or "")),
            "after_length": len(str(after.get("instructions") or "")),
        },
        "model": {
            **_section(before_model, after_model, changed_paths=["model"]),
            "before": {
                "model_id": before_model.get("model_id"),
                "provider_id": before_model.get("provider_id"),
            },
            "after": {
                "model_id": after_model.get("model_id"),
                "provider_id": after_model.get("provider_id"),
            },
        },
        "capabilities": {
            **_section(
                before_capabilities,
                after_capabilities,
                changed_paths=["capabilities"],
            ),
            "before": before_capabilities,
            "after": after_capabilities,
        },
        "skills": {
            **_section(
                [item for item in before_capabilities if item["type"] == "skill"],
                [item for item in after_capabilities if item["type"] == "skill"],
                changed_paths=["capabilities.skills"],
            ),
        },
        "knowledge": {
            **_section(before_knowledge, after_knowledge, changed_paths=["knowledge"]),
            "before": before_knowledge,
            "after": after_knowledge,
        },
        "memory": _section(before_memory, after_memory, changed_paths=["memory"]),
    }
    return {
        "schema_version": AGENT_RELEASE_DIFF_SCHEMA_VERSION,
        "changed_sections": [name for name, value in sections.items() if value["changed"]],
        "sections": sections,
    }


__all__ = [
    "AGENT_RELEASE_CANDIDATE_SCHEMA_VERSION",
    "AGENT_RELEASE_DIFF_SCHEMA_VERSION",
    "AGENT_RELEASE_GATE_SCHEMA_VERSION",
    "AgentReleaseCandidateError",
    "AgentReleaseProfileUnavailableError",
    "build_model_authorization_evidence",
    "build_agent_version_candidate",
    "evaluate_agent_version_candidate",
    "require_available_release_profile",
    "server_release_profile",
    "structured_agent_release_diff",
]
