"""Multi-turn image editing — artifact-backed history + thoughtSignature.

Architecture under test (see ``ai_gateway_core/image/helpers.py`` docstrings):

- ``append_image_turns``: appends user + model turns; model turn carries
  ``artifact_id`` (pointer to S3) plus ``thought_signature`` (Gemini 3.x
  reasoning blob). NEVER stores image bytes inline.
- ``inflate_history_with_bytes``: takes a tiny pointer-only history, fetches
  bytes from S3 in parallel, returns a copy with ``image_base64`` attached
  to each model turn — ready for ``build_gemini_contents_from_history``.
- ``build_gemini_contents_from_history``: pure function — emits
  ``inlineData`` parts using the inflated bytes; emits ``fileData`` only
  for legacy turns written by the previous Files-API design (back-compat).

These tests lock the round-trip: signature persists across the artifact
write/read boundary, S3 bytes inflate correctly, and replay attaches the
signature to the right Gemini part.
"""

from __future__ import annotations

import base64

import pytest

from ai_gateway_core.image import (
    append_image_turns,
    build_gemini_contents_from_history,
    inflate_history_with_bytes,
)


# ---------------------------------------------------------------------------
# 1. append_image_turns — pointer-only persistence
# ---------------------------------------------------------------------------


def test_signature_and_artifact_id_persisted_on_model_turn():
    history: list[dict] = []
    result_image = {
        "content_base64": "fake_b64_data",
        "mime_type": "image/png",
        "thought_signature": "sig_abc_123",
    }
    append_image_turns(
        history, "draw a cat", result_image, "here you go",
        artifact_id="art_001",
    )

    assert len(history) == 2
    assert history[0]["role"] == "user"
    assert history[0]["text"] == "draw a cat"
    assert history[1]["role"] == "model"
    assert history[1]["artifact_id"] == "art_001"
    assert history[1]["thought_signature"] == "sig_abc_123"
    assert history[1]["mime_type"] == "image/png"
    assert history[1]["text"] == "here you go"


def test_no_image_bytes_ever_persisted_in_history():
    """Storage discipline lock: even if upstream gives us 1.5 MB of base64,
    none of it lands in history. Pointer-only or nothing."""
    history: list[dict] = []
    result_image = {
        "content_base64": "X" * 1_500_000,  # ~1.5 MB
        "mime_type": "image/png",
        "thought_signature": "sig_abc",
    }
    append_image_turns(
        history, "draw a fish", result_image, "result text",
        artifact_id="art_002",
    )

    model_turn = history[1]
    assert "image_base64" not in model_turn
    assert "file_uri" not in model_turn
    assert model_turn["artifact_id"] == "art_002"


def test_no_artifact_id_means_text_only_model_turn():
    """If artifact storage is unavailable, the model turn degrades to
    text+sig only — replay still safe (just no visual anchor)."""
    history: list[dict] = []
    result_image = {
        "content_base64": "fake",
        "mime_type": "image/png",
        "thought_signature": "sig_xyz",
    }
    append_image_turns(history, "draw a dog", result_image, "got it")

    model_turn = history[1]
    assert "artifact_id" not in model_turn
    assert "image_base64" not in model_turn
    assert "file_uri" not in model_turn
    assert model_turn["thought_signature"] == "sig_xyz"
    assert model_turn["text"] == "got it"


def test_no_signature_when_absent_in_result():
    history: list[dict] = []
    result_image = {"content_base64": "fake", "mime_type": "image/png"}
    append_image_turns(
        history, "draw a fish", result_image, None, artifact_id="art_003",
    )
    assert "thought_signature" not in history[1]


# ---------------------------------------------------------------------------
# 2. inflate_history_with_bytes — pointers → bytes
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_inflate_attaches_bytes_to_model_turns_with_artifact_id():
    history = [
        {"role": "user", "text": "draw a cat"},
        {
            "role": "model",
            "mime_type": "image/png",
            "artifact_id": "art_001",
            "thought_signature": "sig_first",
        },
    ]
    fake_bytes = b"binary_image_bytes_001"

    async def fake_download(aid: str) -> bytes | None:
        assert aid == "art_001"
        return fake_bytes

    inflated = await inflate_history_with_bytes(history, fake_download)

    assert len(inflated) == 2
    assert inflated[0] == history[0]  # user turn unchanged
    model_turn = inflated[1]
    # Original pointer kept...
    assert model_turn["artifact_id"] == "art_001"
    # ...with bytes added.
    assert model_turn["image_base64"] == base64.b64encode(fake_bytes).decode()
    assert model_turn["thought_signature"] == "sig_first"


@pytest.mark.asyncio
async def test_inflate_does_not_mutate_input_history():
    """Pure-ish function — replay must not silently grow the on-disk
    history with megabytes of base64."""
    history = [
        {"role": "user", "text": "draw"},
        {"role": "model", "artifact_id": "art_001"},
    ]

    async def fake_download(aid: str) -> bytes | None:
        return b"big_bytes"

    await inflate_history_with_bytes(history, fake_download)

    assert "image_base64" not in history[1]


@pytest.mark.asyncio
async def test_inflate_skips_turn_when_download_returns_none():
    """One missing artifact must not abort the whole replay."""
    history = [
        {"role": "model", "artifact_id": "art_001"},
        {"role": "model", "artifact_id": "art_missing"},
        {"role": "model", "artifact_id": "art_002"},
    ]

    async def fake_download(aid: str) -> bytes | None:
        if aid == "art_missing":
            return None
        return f"bytes_for_{aid}".encode()

    inflated = await inflate_history_with_bytes(history, fake_download)

    assert "image_base64" in inflated[0]
    assert "image_base64" not in inflated[1]  # graceful degrade
    assert "image_base64" in inflated[2]


@pytest.mark.asyncio
async def test_inflate_runs_downloads_in_parallel():
    """Multi-turn replay can have many model turns; latency must not
    multiply."""
    import asyncio

    history = [
        {"role": "model", "artifact_id": f"art_{i}"} for i in range(5)
    ]
    call_log: list[str] = []

    async def slow_download(aid: str) -> bytes | None:
        await asyncio.sleep(0.05)
        call_log.append(aid)
        return f"bytes_for_{aid}".encode()

    import time as _time
    start = _time.monotonic()
    inflated = await inflate_history_with_bytes(history, slow_download)
    elapsed = _time.monotonic() - start

    # Sequential would be 5 × 0.05 = 0.25s; parallel should be ~0.05s.
    # Generous bound to avoid CI flakiness.
    assert elapsed < 0.20, f"downloads ran sequentially ({elapsed}s)"
    assert all("image_base64" in t for t in inflated)


# ---------------------------------------------------------------------------
# 3. Replay — signature attached to inlineData (the new path)
# ---------------------------------------------------------------------------


def test_signature_replayed_on_inlineData_when_history_has_image_base64():
    history = [
        {"role": "user", "text": "draw a cat"},
        {
            "role": "model",
            "mime_type": "image/png",
            "image_base64": "Zm9v",  # b'foo' base64
            "thought_signature": "sig_abc_123",
        },
    ]
    contents = build_gemini_contents_from_history(history, "make it orange")

    assert len(contents) == 3
    img_part = next(p for p in contents[1]["parts"] if "inlineData" in p)
    assert img_part["thoughtSignature"] == "sig_abc_123"
    assert img_part["inlineData"]["data"] == "Zm9v"
    assert img_part["inlineData"]["mimeType"] == "image/png"


def test_legacy_file_uri_replay_still_works_for_back_compat():
    """Sessions written by the old Files-API design carry ``file_uri``.
    Replay must still emit ``fileData`` for them so a user mid-session at
    deploy time doesn't lose context."""
    history = [
        {
            "role": "model",
            "mime_type": "image/png",
            "file_uri": "files/legacy_abc",
            "thought_signature": "sig_legacy",
        },
    ]
    contents = build_gemini_contents_from_history(history, "edit it")

    img_part = next(p for p in contents[0]["parts"] if "fileData" in p)
    assert img_part["fileData"]["fileUri"] == "files/legacy_abc"
    assert img_part["thoughtSignature"] == "sig_legacy"


def test_image_base64_takes_precedence_over_file_uri_when_both_present():
    """If both fields are set (after inflate), inlineData wins — that's
    the live data we just downloaded."""
    history = [
        {
            "role": "model",
            "mime_type": "image/png",
            "image_base64": "Zm9v",
            "file_uri": "files/stale",
            "thought_signature": "sig",
        },
    ]
    contents = build_gemini_contents_from_history(history, "edit")

    parts = contents[0]["parts"]
    assert any("inlineData" in p for p in parts)
    assert not any("fileData" in p for p in parts)


def test_text_only_turn_replays_without_image_part():
    history = [
        {
            "role": "model",
            "mime_type": "image/png",
            "thought_signature": "sig_no_anchor",
            "text": "ok",
            # neither image_base64 nor file_uri — anchor missing
        },
    ]
    contents = build_gemini_contents_from_history(history, "next")

    text_parts = [p for p in contents[0]["parts"] if "text" in p]
    image_parts = [p for p in contents[0]["parts"] if "inlineData" in p or "fileData" in p]
    assert text_parts == [{"text": "ok"}]
    assert image_parts == []


# ---------------------------------------------------------------------------
# 4. End-to-end — append → store → inflate → replay
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_full_artifact_backed_two_turn_roundtrip():
    """Simulates two turns. After turn 1 the history has just pointers; on
    turn 2 we inflate via fake S3 then replay — both signatures and the
    turn-1 image data must be present in the contents sent to Gemini."""
    history: list[dict] = []
    fake_s3: dict[str, bytes] = {}

    # ---- Turn 1 ----
    result_t1 = {
        "content_base64": base64.b64encode(b"image_t1_bytes").decode(),
        "mime_type": "image/png",
        "thought_signature": "sig_turn1",
    }
    fake_s3["art_t1"] = b"image_t1_bytes"  # what create_artifact would store
    append_image_turns(
        history, "draw sunset", result_t1, "done", artifact_id="art_t1",
    )

    # History is tiny (no bytes).
    import json as _json
    serialized = _json.dumps(history)
    assert len(serialized) < 1000, "history grew too large"

    # ---- Turn 2 prep — inflate + replay ----
    async def fake_download(aid: str) -> bytes | None:
        return fake_s3.get(aid)

    resolved = await inflate_history_with_bytes(history, fake_download)
    contents = build_gemini_contents_from_history(resolved, "add birds")

    # Three contents entries: prior user, prior model (with bytes+sig), new user.
    assert len(contents) == 3
    model_part = next(
        p for p in contents[1]["parts"] if "inlineData" in p
    )
    assert model_part["inlineData"]["data"] == base64.b64encode(b"image_t1_bytes").decode()
    assert model_part["thoughtSignature"] == "sig_turn1"

    # ---- Turn 2 result ----
    result_t2 = {
        "content_base64": base64.b64encode(b"image_t2_bytes").decode(),
        "mime_type": "image/png",
        "thought_signature": "sig_turn2",
    }
    fake_s3["art_t2"] = b"image_t2_bytes"
    append_image_turns(
        history, "add birds", result_t2, None, artifact_id="art_t2",
    )

    # Turn 3 replay — both prior turns must carry their signatures.
    resolved3 = await inflate_history_with_bytes(history, fake_download)
    contents3 = build_gemini_contents_from_history(resolved3, "make sky pink")

    model_turns = [c for c in contents3 if c["role"] == "model"]
    assert len(model_turns) == 2

    sigs = []
    for mt in model_turns:
        img_part = next(p for p in mt["parts"] if "inlineData" in p)
        sigs.append(img_part["thoughtSignature"])
    assert sigs == ["sig_turn1", "sig_turn2"]
