"""Bounded private stdin envelope. Never include request/key contents in errors."""
import json

# Includes the complete request, private provider key, envelope syntax and final LF.
# Independent of the smaller public request body and per-field admission limits.
MAX_LAUNCH_FRAME = 512 * 1024


class LaunchFrameTooLarge(ValueError):
    def __init__(self):
        super().__init__("sdk_launch_frame_too_large")


def _invalid_constant(_value):
    raise ValueError("sdk_launch_frame_invalid")


def encode_launch_frame(request, provider_key):
    if not isinstance(request, dict) or not isinstance(provider_key, str):
        raise ValueError("sdk_launch_frame_invalid")
    frame = bytearray()
    try:
        encoder = json.JSONEncoder(ensure_ascii=False, separators=(",", ":"), allow_nan=False)
        for part in encoder.iterencode({"request": request, "provider_key": provider_key}):
            encoded = part.encode("utf-8")
            if len(frame) + len(encoded) + 1 > MAX_LAUNCH_FRAME:
                raise LaunchFrameTooLarge()
            frame.extend(encoded)
    except LaunchFrameTooLarge:
        raise
    except (TypeError, ValueError, OverflowError, RecursionError):
        raise ValueError("sdk_launch_frame_invalid") from None
    frame.append(10)
    return bytes(frame)


def read_launch_frame(stream):
    # Parent closes stdin after one frame. EOF distinguishes a complete launch
    # from a truncated prefix; max+1 bounds oversized/trailing input detection.
    raw = stream.read(MAX_LAUNCH_FRAME + 1)
    if len(raw) > MAX_LAUNCH_FRAME:
        raise LaunchFrameTooLarge()
    if not raw.endswith(b"\n") or b"\n" in raw[:-1]:
        raise ValueError("sdk_launch_frame_invalid")
    try:
        value = json.loads(raw[:-1].decode("utf-8"), parse_constant=_invalid_constant)
    except (ValueError, RecursionError):
        raise ValueError("sdk_launch_frame_invalid") from None
    if not isinstance(value, dict) or set(value) != {"request", "provider_key"}:
        raise ValueError("sdk_launch_frame_invalid")
    if not isinstance(value["request"], dict) or not isinstance(value["provider_key"], str):
        raise ValueError("sdk_launch_frame_invalid")
    # Apply the same Unicode/finite-number/whole-envelope budget on decoding.
    encode_launch_frame(value["request"], value["provider_key"])
    return value
