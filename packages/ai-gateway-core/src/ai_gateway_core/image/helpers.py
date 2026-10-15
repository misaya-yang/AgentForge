"""Shared helpers for image generation endpoints.

Centralizes logic that was previously duplicated between the sync endpoint
(``generate_image``), async task (``_run_image_generation_task``), and the
tool executor (``ImageGeneratorExecutor``).
"""

from __future__ import annotations

from typing import Any

# ---------------------------------------------------------------------------
# Size / aspect ratio parsing
# ---------------------------------------------------------------------------

_ASPECT_CANDIDATES = {
    "1:1": 1.0,
    "16:9": 16 / 9,
    "9:16": 9 / 16,
    "4:3": 4 / 3,
    "3:4": 3 / 4,
}


def parse_image_size(size: str | None, default: tuple[int, int] = (1024, 1024)) -> tuple[int, int, str]:
    """Parse a ``"WIDTH*HEIGHT"`` string into (width, height, aspect_ratio).

    Returns the default dimensions on any parse failure. Aspect ratio is
    snapped to the closest supported value among 1:1, 16:9, 9:16, 4:3, 3:4.
    """
    width, height = default
    if size:
        try:
            parts = size.split("*")
            if len(parts) == 2:
                width, height = int(parts[0]), int(parts[1])
        except ValueError:
            pass

    try:
        ratio = float(width) / float(height) if height else 1.0
    except Exception:
        ratio = 1.0

    aspect = min(_ASPECT_CANDIDATES.keys(), key=lambda k: abs(ratio - _ASPECT_CANDIDATES[k]))
    return width, height, aspect


# ---------------------------------------------------------------------------
# Style mapping — shared across sync/async endpoints
# ---------------------------------------------------------------------------

STYLE_MAP: dict[str, str] = {
    "default": "<auto>",
    "auto": "<auto>",
    "photography": "<photography>",
    "portrait": "<portrait>",
    "3d": "<3d cartoon>",
    "anime": "<anime>",
    "oil": "<oil painting>",
    "watercolor": "<watercolor>",
    "sketch": "<sketch>",
    "flat": "<flat illustration>",
}


def resolve_style(style: str | None) -> str:
    """Map a friendly style name to the DashScope style tag."""
    return STYLE_MAP.get(style or "default", "<auto>")


# ---------------------------------------------------------------------------
# Provider routing — detect which image backend to prefer from model_id
# ---------------------------------------------------------------------------


def resolve_image_routing(
    model_id: str | None,
    selected_provider: str | None = None,
) -> tuple[bool, bool, str | None]:
    """Determine (prefer_gemini, prefer_doubao, dashscope_model_override).

    Rules:
    - ``selected_provider == "google"`` or ``"gemini"`` in id → Gemini
    - ``"doubao"`` or ``"seedream"`` in id → Doubao
    - ``"qwen-image"`` or ``"qwen_image"`` in id → DashScope with model override

    Detection is case-insensitive and runs a single ``.lower()`` pass.
    """
    if not model_id:
        return False, False, None

    mid = model_id.lower()
    prefer_gemini = selected_provider == "google" or "gemini" in mid
    prefer_doubao = "doubao" in mid or "seedream" in mid
    dashscope_model: str | None = None
    if "qwen-image" in mid or "qwen_image" in mid:
        dashscope_model = model_id  # pass original id as-is, e.g. "qwen-image-2.0"

    return prefer_gemini, prefer_doubao, dashscope_model


# ---------------------------------------------------------------------------
# Gemini multi-turn contents builder
# ---------------------------------------------------------------------------
#
# Storage architecture for multi-turn editing:
#
# We DON'T use Gemini's Files API. Two reasons:
#   1. Vertex Express endpoint cannot read URIs uploaded to AI Studio
#      (cross-host 403). Pinning everything to AI Studio breaks the free-tier
#      Vertex routing for chat.
#   2. Vendor-locks our scale to Google's storage product, with a 48 h URI
#      expiry that breaks "user comes back tomorrow to keep editing".
#
# Instead Gateway stores generated images in our own S3/MinIO
# (via ``ArtifactStorage``). Session metadata holds a tiny pointer per turn:
# ``artifact_id`` (~36 bytes) + ``thought_signature`` (~few KB). On the next
# turn the route fetches the bytes from S3, base64-encodes them, and passes
# them to Gemini as ``inlineData``. Server-to-server only — the public API
# never returns or accepts base64.
#
# Backward compat: histories written by the previous Files-API design carry
# ``file_uri`` and/or ``image_base64`` fields. Replay still honors them so a
# user mid-session at deploy time doesn't lose context, but we stop writing
# them going forward.


def build_gemini_contents_from_history(
    image_history: list[dict[str, Any]],
    new_user_text: str,
) -> list[dict[str, Any]]:
    """Build Gemini ``contents`` array from stored image chat history.

    The route layer is expected to inflate each model turn's ``artifact_id``
    into ``image_base64`` + ``mime_type`` BEFORE calling this function — see
    ``inflate_history_with_bytes``. This stays a pure function so it's trivial
    to unit-test.

    Recognized model-turn shapes (in order of preference):
    - ``image_base64`` (inflated from artifact_id, or legacy fallback) →
      ``{"inlineData": {"data": ..., "mimeType": ...}}``
    - ``file_uri`` (legacy from old Files-API design) →
      ``{"fileData": {"fileUri": ..., "mimeType": ...}}``
    - neither → text-only model turn (visual anchor lost, replay still safe)

    ``thought_signature`` (when present) is attached to the image part. Gemini
    3.x requires the signature to be replayed verbatim or it 400s / silently
    degrades edit coherence. Signatures are opaque blobs of a few KB.
    """
    contents: list[dict[str, Any]] = []
    for turn in image_history:
        role = turn.get("role", "user")
        parts: list[dict[str, Any]] = []
        if turn.get("text"):
            parts.append({"text": turn["text"]})

        inline_b64 = turn.get("image_base64")
        file_uri = turn.get("file_uri")
        mime = turn.get("mime_type", "image/jpeg")
        img_part: dict[str, Any] | None = None
        if inline_b64:
            img_part = {"inlineData": {"mimeType": mime, "data": inline_b64}}
        elif file_uri:
            img_part = {"fileData": {"fileUri": file_uri, "mimeType": mime}}

        if img_part is not None:
            if turn.get("thought_signature"):
                img_part["thoughtSignature"] = turn["thought_signature"]
            parts.append(img_part)

        if parts:
            contents.append({"role": role, "parts": parts})

    contents.append({"role": "user", "parts": [{"text": new_user_text}]})
    return contents


async def inflate_history_with_bytes(
    image_history: list[dict[str, Any]],
    download: Any,  # Callable[[str], Awaitable[bytes | None]]
) -> list[dict[str, Any]]:
    """Return a copy of ``image_history`` with image bytes attached as
    ``image_base64`` on every model turn that has an ``artifact_id``.

    ``download(artifact_id) -> bytes | None`` is awaited concurrently for all
    artifact ids. Turns with ``image_base64`` already set (or legacy
    ``file_uri``) are passed through untouched. Turns whose download returns
    ``None`` keep their pointer fields so replay stays compat-safe — they just
    won't carry a visual anchor that turn.

    This function is the boundary between "tiny history pointers" (what we
    store) and "fat inline-data history" (what Gemini's API needs to see).
    """
    import asyncio
    import base64 as _b64

    artifact_ids: list[str] = []
    for turn in image_history:
        if (
            turn.get("role") == "model"
            and turn.get("artifact_id")
            and not turn.get("image_base64")
        ):
            artifact_ids.append(turn["artifact_id"])

    if not artifact_ids:
        return [dict(t) for t in image_history]

    # Parallel fetch — multi-turn replay shouldn't be N×latency.
    fetched = await asyncio.gather(
        *[download(aid) for aid in artifact_ids], return_exceptions=True,
    )
    bytes_by_id: dict[str, bytes] = {}
    for aid, res in zip(artifact_ids, fetched, strict=True):
        if isinstance(res, BaseException):
            continue
        if res:
            bytes_by_id[aid] = res

    inflated: list[dict[str, Any]] = []
    for turn in image_history:
        t = dict(turn)
        aid = t.get("artifact_id")
        if (
            t.get("role") == "model"
            and aid
            and aid in bytes_by_id
            and not t.get("image_base64")
        ):
            t["image_base64"] = _b64.b64encode(bytes_by_id[aid]).decode()
        inflated.append(t)
    return inflated


def append_image_turns(
    image_history: list[dict[str, Any]],
    user_text: str,
    result_image: dict[str, Any] | None,
    result_text: str | None,
    *,
    artifact_id: str | None = None,
) -> None:
    """Append one user turn and (on success) one model turn to ``image_history``.

    Storage discipline:
    - The model turn stores ONLY a pointer (``artifact_id``) plus tiny metadata
      (mime type, optional text reply, optional thought signature).
    - Image bytes stay in our S3/MinIO via ArtifactStorage; ``artifact_id`` is
      the lookup key.
    - We never inline base64 here — a single 1024×1024 image is ~1 MB
      base64-encoded; two such turns blow the 1 MB session metadata cap and
      brick writes for the whole session.

    Without an ``artifact_id`` the model turn degrades gracefully to text-only
    (no visual anchor for the next turn, but the session keeps writing). This
    happens only on artifact-storage failures — log and continue.

    Mutates the list in place. Call only when Gemini responded successfully;
    skip entirely on failure to avoid a dangling unanswered prompt in the next
    turn's context.
    """
    if result_image is None:
        return
    image_history.append({"role": "user", "text": user_text})
    model_turn: dict[str, Any] = {
        "role": "model",
        "mime_type": result_image.get("mime_type", "image/jpeg"),
    }
    if artifact_id:
        model_turn["artifact_id"] = artifact_id
    if result_text:
        model_turn["text"] = result_text
    sig = result_image.get("thought_signature")
    if sig:
        model_turn["thought_signature"] = sig
    image_history.append(model_turn)
