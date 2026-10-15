"""
Regression tests for ``src.api.schemas.assistant`` image-generation
``reference_image`` size bounding.

Bug context: before commit 4372eff we had no explicit upper bound on
``reference_image``. Over-sized base64 payloads reached the proxy and
got a generic 413 from nginx; the frontend had no way to distinguish
"this image is too big" from a transient network failure. The fix caps
``reference_image`` at ``_REFERENCE_IMAGE_MAX_CHARS`` so Pydantic
returns a crisp 422 *before* nginx does anything.

This test locks that cap in place. If anyone later bumps it (valid —
but must be matched in nginx ``client_max_body_size`` to preserve the
"Pydantic 422 before nginx 413" ordering), the test fails, they
re-read the comment, and they update both sides together.
"""

from __future__ import annotations

import pytest
from pydantic import ValidationError

from src.api.schemas.assistant import (
    AsyncImageGenerationRequest,
    ImageGenerationRequest,
    _REFERENCE_IMAGE_MAX_CHARS,
)


def test_cap_constant_matches_nginx_expectation():
    """If the cap drifts out of sync with the documented nginx limit,
    the "Pydantic 422 before nginx 413" ordering silently breaks.
    Pinning both values here forces a reviewer to re-read the comment
    and update nginx in the same commit."""
    assert _REFERENCE_IMAGE_MAX_CHARS == 6_000_000


@pytest.mark.parametrize(
    "model_cls",
    [ImageGenerationRequest, AsyncImageGenerationRequest],
)
def test_reference_image_at_cap_accepted(model_cls):
    """Exactly at the cap is fine — border is inclusive (Pydantic
    ``max_length`` is ``len(value) <= max_length``)."""
    payload = "a" * _REFERENCE_IMAGE_MAX_CHARS
    req = model_cls(
        prompt="edit this",
        model_id="gemini-3.1-flash-image-preview",
        reference_image=payload,
    )
    assert req.reference_image is not None
    assert len(req.reference_image) == _REFERENCE_IMAGE_MAX_CHARS


@pytest.mark.parametrize(
    "model_cls",
    [ImageGenerationRequest, AsyncImageGenerationRequest],
)
def test_reference_image_over_cap_rejected(model_cls):
    """One byte past the cap must trip a clear Pydantic ValidationError
    so FastAPI returns 422, not nginx's generic 413."""
    payload = "a" * (_REFERENCE_IMAGE_MAX_CHARS + 1)
    with pytest.raises(ValidationError) as exc:
        model_cls(
            prompt="edit this",
            model_id="gemini-3.1-flash-image-preview",
            reference_image=payload,
        )
    # Confirm the error is specifically about reference_image length
    errs = exc.value.errors()
    assert any(
        e["loc"] == ("reference_image",) and "too_long" in e["type"]
        for e in errs
    ), errs


@pytest.mark.parametrize(
    "model_cls",
    [ImageGenerationRequest, AsyncImageGenerationRequest],
)
def test_reference_image_none_allowed(model_cls):
    """Omitting reference_image is the common case (fresh generation,
    not edit). Must keep working."""
    req = model_cls(
        prompt="a cat",
        model_id="gemini-3.1-flash-image-preview",
    )
    assert req.reference_image is None
