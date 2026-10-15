"""Shared image-generation helpers.

Gateway routes import these pure utilities for size parsing, routing, history,
watermarking, and callback handling.
"""

from .callback import send_image_callback
from .helpers import (
    STYLE_MAP,
    append_image_turns,
    build_gemini_contents_from_history,
    inflate_history_with_bytes,
    parse_image_size,
    resolve_image_routing,
    resolve_style,
)
from .image_state import (
    advance_latest_artifact_cas,
    compute_owner_scope,
    compute_request_hash,
    count_active_image_tasks,
    create_image_blob,
    create_image_task,
    get_image_blob,
    get_image_session,
    get_image_task,
    get_turn,
    get_turn_by_task,
    insert_turn,
    list_turns,
    lookup_idempotent,
    new_turn_id,
    record_idempotent,
    set_locked_style,
    update_image_blob_status,
    update_image_task,
    update_turn_status,
    upsert_image_session,
)


def apply_watermark_b64(*args, **kwargs):
    """Lazy wrapper so importing ``ai_gateway_core.image`` does not import PIL."""
    from .watermark import apply_watermark_b64 as _apply_watermark_b64

    return _apply_watermark_b64(*args, **kwargs)


def make_thumbnail(*args, **kwargs):
    """Lazy wrapper so non-image services do not block on Pillow startup."""
    from .thumbnail import make_thumbnail as _make_thumbnail

    return _make_thumbnail(*args, **kwargs)


__all__ = [
    "STYLE_MAP",
    "advance_latest_artifact_cas",
    "append_image_turns",
    "apply_watermark_b64",
    "build_gemini_contents_from_history",
    "count_active_image_tasks",
    "compute_owner_scope",
    "compute_request_hash",
    "create_image_blob",
    "create_image_task",
    "get_image_blob",
    "get_image_session",
    "get_image_task",
    "get_turn",
    "get_turn_by_task",
    "inflate_history_with_bytes",
    "insert_turn",
    "list_turns",
    "lookup_idempotent",
    "make_thumbnail",
    "new_turn_id",
    "parse_image_size",
    "record_idempotent",
    "resolve_image_routing",
    "resolve_style",
    "send_image_callback",
    "set_locked_style",
    "update_image_blob_status",
    "update_image_task",
    "update_turn_status",
    "upsert_image_session",
]
