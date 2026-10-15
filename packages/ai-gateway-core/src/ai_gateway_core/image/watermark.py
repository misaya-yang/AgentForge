"""Image watermark overlay service.

Level 3 adaptive watermarking:
- Analyzes 4 corners to pick the smoothest region (lowest texture variance)
- Detects background brightness at that location
- Auto-selects contrasting color (white on dark, dark on light)
- Adds a subtle outline in the opposite color for cross-background readability
"""

import base64
import io
from pathlib import Path

import numpy as np
from PIL import Image, ImageFilter

from ai_gateway_core.logging import get_logger

logger = get_logger(__name__)

# ---------------------------------------------------------------------------
# Tunable constants
# ---------------------------------------------------------------------------
_WATERMARK_SCALE = 0.22              # watermark width as fraction of image
_WATERMARK_MARGIN_PX = 24            # edge margin in pixels
_WATERMARK_FILL_OPACITY = 0.75       # main watermark ink opacity
_WATERMARK_OUTLINE_OPACITY = 0.85    # outline opacity
_WATERMARK_DILATION_SIZE = 5         # MaxFilter kernel — 2px on each side
_WATERMARK_BLUR_SIGMA = 0.8          # Gaussian blur for outline smoothing
_DARK_BG_THRESHOLD = 140             # mean grayscale threshold (0-255)
_DARK_INK_RGB = (20, 20, 20)         # near-black fill for light backgrounds
_LIGHT_INK_RGB = (255, 255, 255)     # white fill for dark backgrounds
_CORNER_ANALYSIS_PAD_PX = 6          # extra context for corner analysis
_FALLBACK_BRIGHTNESS = 128.0         # if corner analysis fails
_PNG_COMPRESS_LEVEL = 1              # 0-9; 1 = fast encode (PIL default 6)

# Ship watermark alongside the module so pip-install (site-packages) and
# local dev (repo checkout) both resolve the same file.
_WATERMARK_PATH = Path(__file__).parent / "assets" / "watermark.png"
_watermark_cache: Image.Image | None = None


def _load_watermark() -> Image.Image | None:
    global _watermark_cache
    if _watermark_cache is not None:
        return _watermark_cache
    if not _WATERMARK_PATH.exists():
        logger.warning("[Watermark] watermark.png not found at %s", _WATERMARK_PATH)
        return None
    try:
        img = Image.open(_WATERMARK_PATH).convert("RGBA")
        img.load()  # Force full decode so concurrent reads are safe
        _watermark_cache = img
        logger.info("[Watermark] Loaded watermark: %dx%d", img.width, img.height)
        return _watermark_cache
    except Exception as e:
        logger.error("[Watermark] Failed to load: %s", e)
        return None


def _tint(img: Image.Image, rgb: tuple[int, int, int]) -> Image.Image:
    """Fill the RGB channels with a solid color, keep alpha."""
    _, _, _, a = img.split()
    rr = Image.new("L", img.size, rgb[0])
    gg = Image.new("L", img.size, rgb[1])
    bb = Image.new("L", img.size, rgb[2])
    return Image.merge("RGBA", (rr, gg, bb, a))


def _analyze_region(base: Image.Image, box: tuple[int, int, int, int]) -> tuple[float, float]:
    """Analyze a region: return (mean_brightness, variance).

    Lower variance = smoother region = better for watermark.
    """
    region = base.crop(box).convert("L")
    arr = np.asarray(region, dtype=np.float32)
    return float(arr.mean()), float(arr.std())


def _pick_best_corner(base: Image.Image, wm_w: int, wm_h: int, margin: int) -> tuple[int, int, float]:
    """Evaluate 4 corners, return (x, y, mean_brightness) of the smoothest one."""
    W, H = base.width, base.height
    pad = _CORNER_ANALYSIS_PAD_PX

    corners = {
        "bl": (margin - pad, H - wm_h - margin - pad, margin + wm_w + pad, H - margin + pad),
        "br": (W - wm_w - margin - pad, H - wm_h - margin - pad, W - margin + pad, H - margin + pad),
        "tl": (margin - pad, margin - pad, margin + wm_w + pad, margin + wm_h + pad),
        "tr": (W - wm_w - margin - pad, margin - pad, W - margin + pad, margin + wm_h + pad),
    }

    results = {}
    for name, box in corners.items():
        x0, y0 = max(0, box[0]), max(0, box[1])
        x1, y1 = min(W, box[2]), min(H, box[3])
        if x1 <= x0 or y1 <= y0:
            continue
        results[name] = _analyze_region(base, (x0, y0, x1, y1))

    if not results:
        return W - wm_w - margin, H - wm_h - margin, _FALLBACK_BRIGHTNESS

    best_name = min(results, key=lambda k: results[k][1])
    best_mean, best_std = results[best_name]

    positions = {
        "bl": (margin, H - wm_h - margin),
        "br": (W - wm_w - margin, H - wm_h - margin),
        "tl": (margin, margin),
        "tr": (W - wm_w - margin, margin),
    }
    x, y = positions[best_name]
    logger.debug(
        "[Watermark] Best corner: %s (brightness=%.0f, variance=%.1f)",
        best_name, best_mean, best_std,
    )
    return x, y, best_mean


def apply_watermark(image_bytes: bytes) -> bytes:
    """Apply adaptive watermark to image bytes. Returns PNG bytes.

    CPU-bound; callers running inside an async event loop should wrap this
    in ``asyncio.to_thread(apply_watermark, image_bytes)`` to avoid blocking.
    """
    wm = _load_watermark()
    if wm is None:
        raise RuntimeError(
            f"watermark.png not found at {_WATERMARK_PATH} — "
            "required when add_watermark=True"
        )

    try:
        base = Image.open(io.BytesIO(image_bytes)).convert("RGBA")
    except Exception as e:
        raise RuntimeError(f"Cannot open image for watermarking: {e}") from e

    target_width = int(base.width * _WATERMARK_SCALE)
    scale = target_width / wm.width
    wm_w = max(1, int(wm.width * scale))
    wm_h = max(1, int(wm.height * scale))
    wm_scaled = wm.resize((wm_w, wm_h), Image.LANCZOS)

    x, y, brightness = _pick_best_corner(base, wm_w, wm_h, _WATERMARK_MARGIN_PX)

    if brightness > _DARK_BG_THRESHOLD:
        fill_color = _DARK_INK_RGB
        outline_color = _LIGHT_INK_RGB
    else:
        fill_color = _LIGHT_INK_RGB
        outline_color = (0, 0, 0)

    wm_fill = _tint(wm_scaled, fill_color)
    fill_alpha = wm_fill.getchannel("A").point(lambda p: int(p * _WATERMARK_FILL_OPACITY))
    wm_fill.putalpha(fill_alpha)

    # Outline: dilate alpha channel to form a ring around the text
    alpha = wm_scaled.getchannel("A")
    dilated = alpha.filter(ImageFilter.MaxFilter(_WATERMARK_DILATION_SIZE))
    dilated = dilated.filter(ImageFilter.GaussianBlur(_WATERMARK_BLUR_SIGMA))

    dilated_arr = np.asarray(dilated, dtype=np.int16)
    orig_arr = np.asarray(alpha, dtype=np.int16)
    ring_arr = np.clip(dilated_arr - orig_arr // 2, 0, 255).astype(np.uint8)
    ring_alpha = Image.fromarray(ring_arr, mode="L")
    ring_alpha = ring_alpha.point(lambda p: int(p * _WATERMARK_OUTLINE_OPACITY))

    outline_rgb = Image.new("RGBA", wm_scaled.size, (*outline_color, 0))
    outline_rgb.putalpha(ring_alpha)

    overlay = Image.new("RGBA", base.size, (0, 0, 0, 0))
    overlay.paste(outline_rgb, (x, y), outline_rgb)
    overlay.paste(wm_fill, (x, y), wm_fill)

    result = Image.alpha_composite(base, overlay)

    buf = io.BytesIO()
    result.convert("RGB").save(buf, format="PNG", compress_level=_PNG_COMPRESS_LEVEL)
    return buf.getvalue()


def apply_watermark_b64(b64_data: str) -> tuple[str, str]:
    """Apply watermark to a base64-encoded image. Returns (new_b64, 'image/png').

    Raises on any failure (missing watermark asset, invalid base64, etc.).
    Callers that set ``add_watermark=True`` must handle the exception as a
    hard failure — never silently return an un-watermarked image.
    """
    raw = base64.b64decode(b64_data)
    watermarked = apply_watermark(raw)
    return base64.b64encode(watermarked).decode("utf-8"), "image/png"
