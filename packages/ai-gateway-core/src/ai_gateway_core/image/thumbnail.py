"""Thumbnail generator — produces a small (max 256 px, longest edge) PNG.

PIL is already a runtime dependency for the watermarking pipeline. The
``make_thumbnail`` function is intentionally tolerant: any exception while
opening / resizing returns ``None`` so the caller skips persisting the
thumbnail variant rather than failing the whole request.
"""

from __future__ import annotations

import io
import logging
from typing import Final

from PIL import Image

logger = logging.getLogger(__name__)

_MAX_EDGE: Final[int] = 256
_PNG_COMPRESS_LEVEL: Final[int] = 6


def make_thumbnail(raw_bytes: bytes, max_edge: int = _MAX_EDGE) -> bytes | None:
    """Resize ``raw_bytes`` so the longest edge is ≤ ``max_edge``. Output PNG."""
    if not raw_bytes:
        return None
    try:
        with Image.open(io.BytesIO(raw_bytes)) as im:
            im = im.convert("RGB") if im.mode not in ("RGB", "RGBA") else im
            w, h = im.size
            if w == 0 or h == 0:
                return None
            scale = min(1.0, max_edge / max(w, h))
            if scale >= 1.0:
                # Image is already small enough — re-encode to PNG for consistency
                buf = io.BytesIO()
                im.save(buf, format="PNG", compress_level=_PNG_COMPRESS_LEVEL)
                return buf.getvalue()
            new_size = (max(1, int(w * scale)), max(1, int(h * scale)))
            resized = im.resize(new_size, Image.LANCZOS)
            buf = io.BytesIO()
            resized.save(buf, format="PNG", compress_level=_PNG_COMPRESS_LEVEL)
            return buf.getvalue()
    except Exception as exc:
        logger.warning("[Thumbnail] make_thumbnail failed: %s", exc)
        return None


__all__ = ["make_thumbnail"]
