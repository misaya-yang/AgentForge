from __future__ import annotations

from typing import get_args, get_origin

from ai_gateway_contracts.event_envelope import _PAYLOAD_MODELS, EventEnvelope
from ai_gateway_core.events.registry import STREAM_NAMES
from pydantic.fields import FieldInfo


def _is_optional(field: FieldInfo) -> bool:
    annotation = field.annotation
    return get_origin(annotation) is type(None) or type(None) in get_args(annotation)


def test_event_envelope_newer_context_fields_are_optional() -> None:
    required_baseline = {
        "event_id",
        "event_type",
        "schema_version",
        "occurred_at",
        "producer",
        "tenant_id",
        "request_id",
        "payload",
    }
    for name, field in EventEnvelope.model_fields.items():
        if name in required_baseline:
            continue
        assert not field.is_required() or _is_optional(field), name


def test_registered_event_payloads_have_stream_routes() -> None:
    assert _PAYLOAD_MODELS
    assert set(_PAYLOAD_MODELS) <= set(STREAM_NAMES)


def test_event_schema_versions_match_event_type_suffix() -> None:
    for event_type, model in _PAYLOAD_MODELS.items():
        suffix = event_type.rsplit(".v", 1)[-1]
        assert suffix.isdigit()
        instance_fields = model.model_fields
        if "schema_version" in instance_fields:
            default = instance_fields["schema_version"].default
            assert default in (int(suffix), None)
