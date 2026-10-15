"""Bounded, message-free diagnostics for caught internal exceptions.

The standard ``logging`` traceback path keeps the exception object alive and
renders its message, source line, and potentially secret-bearing values.  This
module deliberately records only stable code coordinates, a closed argument
shape, bounded cause/context and ExceptionGroup structure, and strictly
validated numeric errno.  Callers must pass a static event name; the exception
itself is never attached to the ``LogRecord``.
"""

from __future__ import annotations

import builtins
import hashlib
import logging
import re
from collections import deque
from typing import Any

from ai_gateway_core.security.redaction import redact_trace_text

_SCHEMA_VERSION = "internal-exception/v2"
_MAX_FRAMES = 24
_MAX_CHAIN_DEPTH = 4
_MAX_CHAIN_FRAMES = 8
_MAX_GROUP_MEMBERS = 8
_MAX_GROUP_MEMBER_FRAMES = 4
_MAX_ARGS_SHAPE = 8
_MAX_REPORTED_COUNT = 255
_MIN_SAFE_ERRNO = -65_535
_MAX_SAFE_ERRNO = 65_535
_MAX_LINE_NUMBER = 2_147_483_647
_MAX_FILE_CHARS = 200
_MAX_FUNCTION_CHARS = 160
_MAX_EXCEPTION_TYPE_CHARS = 120
_MAX_EVENT_CHARS = 160
_SAFE_EVENT_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.:=\-]*$")
_ARG_KINDS = frozenset(
    {"none", "bool", "int", "float", "text", "bytes", "mapping", "sequence", "exception", "other"}
)
_CHAIN_RELATIONS = frozenset({"cause", "context"})

_BASE_ARGS = BaseException.__dict__["args"]
_BASE_TRACEBACK = BaseException.__dict__["__traceback__"]
_BASE_CAUSE = BaseException.__dict__["__cause__"]
_BASE_CONTEXT = BaseException.__dict__["__context__"]
_BASE_SUPPRESS_CONTEXT = BaseException.__dict__["__suppress_context__"]
_OSERROR_ERRNO = OSError.__dict__["errno"]
_BASE_EXCEPTION_GROUP = getattr(builtins, "BaseExceptionGroup", None)
_GROUP_EXCEPTIONS = (
    _BASE_EXCEPTION_GROUP.__dict__["exceptions"] if _BASE_EXCEPTION_GROUP is not None else None
)


def _bounded_text(value: str, *, limit: int, fallback: str) -> str:
    """Return a single-line, redacted, bounded diagnostic component."""
    cleaned = "".join(char if char >= " " and char != "\x7f" else "?" for char in value)
    cleaned = redact_trace_text(cleaned, limit=limit).strip()
    return cleaned or fallback


def _safe_file_name(value: str) -> str:
    # A basename identifies the failing source file without disclosing host
    # directory layouts.  Both path separators are handled for diagnostics
    # produced by code developed on a different operating system.
    basename = value.replace("\\", "/").rsplit("/", 1)[-1]
    return _bounded_text(basename, limit=_MAX_FILE_CHARS, fallback="<unknown>")


def _safe_function_name(value: str) -> str:
    return _bounded_text(value, limit=_MAX_FUNCTION_CHARS, fallback="<unknown>")


def _safe_exception_type(exc: BaseException) -> str:
    try:
        # Bypass a custom metaclass ``__getattribute__``.  Diagnostic logging
        # must not execute user-defined accessors while handling a failure.
        name = type.__getattribute__(type(exc), "__name__")
    except Exception:
        return "Exception"
    if not isinstance(name, str):
        return "Exception"
    return _bounded_text(name, limit=_MAX_EXCEPTION_TYPE_CHARS, fallback="Exception")


def _safe_args(exc: BaseException) -> tuple[Any, ...]:
    try:
        args = _BASE_ARGS.__get__(exc, type(exc))
    except Exception:
        return ()
    return args if type(args) is tuple else ()


def _arg_kind(value: Any) -> str:
    """Classify an argument without invoking its protocol methods."""
    value_type = type(value)
    if value is None:
        return "none"
    if value_type is bool:
        return "bool"
    if value_type is int:
        return "int"
    if value_type is float:
        return "float"
    if value_type is str:
        return "text"
    if value_type in {bytes, bytearray, memoryview}:
        return "bytes"
    if value_type is dict:
        return "mapping"
    if value_type in {list, tuple}:
        return "sequence"
    if isinstance(value, BaseException):
        return "exception"
    return "other"


def _build_args_shape(exc: BaseException) -> dict[str, Any]:
    args = _safe_args(exc)
    return {
        "count": min(len(args), _MAX_REPORTED_COUNT),
        "truncated": len(args) > _MAX_ARGS_SHAPE or len(args) > _MAX_REPORTED_COUNT,
        "kinds": [_arg_kind(value) for value in args[:_MAX_ARGS_SHAPE]],
    }


def _safe_errno(exc: BaseException) -> int | None:
    if not isinstance(exc, OSError):
        return None
    try:
        value = _OSERROR_ERRNO.__get__(exc, type(exc))
    except Exception:
        return None
    if type(value) is not int or not _MIN_SAFE_ERRNO <= value <= _MAX_SAFE_ERRNO:
        return None
    return value


def _extract_frames(
    exc: BaseException,
    *,
    limit: int,
) -> tuple[list[dict[str, str | int]], bool]:
    frames: deque[dict[str, str | int]] = deque(maxlen=limit)
    visited = 0
    try:
        traceback_cursor = _BASE_TRACEBACK.__get__(exc, type(exc))
    except Exception:
        traceback_cursor = None

    # Walk traceback links directly.  Unlike traceback.extract_tb(), this does
    # not read source files or inspect frame locals.  The deque bounds retained
    # memory while preserving the innermost root-failure coordinates.
    while traceback_cursor is not None:
        frame = traceback_cursor.tb_frame
        frames.append(
            {
                "file": _safe_file_name(frame.f_code.co_filename),
                "function": _safe_function_name(frame.f_code.co_name),
                "line": max(min(traceback_cursor.tb_lineno, _MAX_LINE_NUMBER), 0),
            }
        )
        visited += 1
        traceback_cursor = traceback_cursor.tb_next
    return list(frames), visited > limit


def _fingerprint(
    exception_type: str,
    frames: list[dict[str, str | int]],
    args_shape: dict[str, Any],
    *,
    errno_value: int | None = None,
    group: dict[str, Any] | None = None,
    chain: list[dict[str, Any]] | None = None,
) -> str:
    components = [exception_type, f"args:{args_shape['count']}:{','.join(args_shape['kinds'])}"]
    components.extend(f"{frame['file']}:{frame['function']}:{frame['line']}" for frame in frames)
    if errno_value is not None:
        components.append(f"errno:{errno_value}")
    if group is not None:
        components.extend(
            f"member:{member['exception_type']}:{member['fingerprint']}"
            for member in group["members"]
        )
    if chain is not None:
        components.extend(
            f"chain:{node['relation']}:{node['exception_type']}:{node['fingerprint']}"
            for node in chain
        )
    return hashlib.sha256("\n".join(components).encode("utf-8")).hexdigest()[:16]


def _build_exception_group(exc: BaseException) -> dict[str, Any] | None:
    if _BASE_EXCEPTION_GROUP is None or not isinstance(exc, _BASE_EXCEPTION_GROUP):
        return None
    descriptor = _GROUP_EXCEPTIONS
    if descriptor is None:
        return None
    try:
        members = descriptor.__get__(exc, type(exc))
    except Exception:
        return None
    if type(members) is not tuple:
        return None

    summaries = [
        _build_exception_node(member, frame_limit=_MAX_GROUP_MEMBER_FRAMES, include_group=False)
        for member in members[:_MAX_GROUP_MEMBERS]
    ]
    return {
        "member_count": min(len(members), _MAX_REPORTED_COUNT),
        "members_truncated": len(members) > _MAX_GROUP_MEMBERS
        or len(members) > _MAX_REPORTED_COUNT,
        "members": summaries,
    }


def _build_exception_node(
    exc: BaseException,
    *,
    frame_limit: int,
    include_group: bool = True,
) -> dict[str, Any]:
    exception_type = _safe_exception_type(exc)
    frames, frames_truncated = _extract_frames(exc, limit=frame_limit)
    args_shape = _build_args_shape(exc)
    errno_value = _safe_errno(exc)
    group = _build_exception_group(exc) if include_group else None
    node: dict[str, Any] = {
        "exception_type": exception_type,
        "fingerprint": _fingerprint(
            exception_type,
            frames,
            args_shape,
            errno_value=errno_value,
            group=group,
        ),
        "frames_truncated": frames_truncated,
        "frames": frames,
        "args_shape": args_shape,
    }
    if errno_value is not None:
        node["errno"] = errno_value
    if group is not None:
        node["exception_group"] = group
    return node


def _next_exception_link(exc: BaseException) -> tuple[str, BaseException] | None:
    try:
        cause = _BASE_CAUSE.__get__(exc, type(exc))
    except Exception:
        cause = None
    if isinstance(cause, BaseException):
        return "cause", cause

    try:
        suppress_context = _BASE_SUPPRESS_CONTEXT.__get__(exc, type(exc)) is True
        context = _BASE_CONTEXT.__get__(exc, type(exc))
    except Exception:
        return None
    if not suppress_context and isinstance(context, BaseException):
        return "context", context
    return None


def _build_internal_exception_diagnostic(exc: BaseException) -> dict[str, Any]:
    """Build a bounded primitive-only diagnostic without rendering values."""
    root = _build_exception_node(exc, frame_limit=_MAX_FRAMES)
    chain: list[dict[str, Any]] = []
    chain_truncated = False
    seen = {id(exc)}
    cursor = exc
    while True:
        link = _next_exception_link(cursor)
        if link is None:
            break
        relation, linked = link
        if id(linked) in seen or len(chain) >= _MAX_CHAIN_DEPTH:
            chain_truncated = True
            break
        seen.add(id(linked))
        node = _build_exception_node(linked, frame_limit=_MAX_CHAIN_FRAMES)
        node = {"relation": relation, **node}
        chain.append(node)
        cursor = linked

    root["chain_truncated"] = chain_truncated
    root["chain"] = chain
    root["fingerprint"] = _fingerprint(
        root["exception_type"],
        root["frames"],
        root["args_shape"],
        errno_value=root.get("errno"),
        group=root.get("exception_group"),
        chain=chain,
    )
    return {"schema_version": _SCHEMA_VERSION, **root}


def _coerce_args_shape(value: Any) -> dict[str, Any]:
    if type(value) is not dict:
        return {"count": 0, "truncated": False, "kinds": []}
    raw_kinds = value.get("kinds")
    kinds = (
        [kind for kind in raw_kinds[:_MAX_ARGS_SHAPE] if type(kind) is str and kind in _ARG_KINDS]
        if type(raw_kinds) is list
        else []
    )
    raw_count = value.get("count")
    count = (
        raw_count
        if type(raw_count) is int and 0 <= raw_count <= _MAX_REPORTED_COUNT
        else min(len(kinds), _MAX_REPORTED_COUNT)
    )
    return {
        "count": count,
        "truncated": value.get("truncated") is True
        or (type(raw_kinds) is list and len(raw_kinds) != len(kinds))
        or count > len(kinds),
        "kinds": kinds,
    }


def _coerce_frames(value: Any, *, limit: int) -> tuple[list[dict[str, str | int]], bool]:
    frames: list[dict[str, str | int]] = []
    if type(value) is not list:
        return frames, False
    for raw_frame in value[:limit]:
        if type(raw_frame) is not dict:
            continue
        raw_file = raw_frame.get("file")
        raw_function = raw_frame.get("function")
        raw_line = raw_frame.get("line")
        if type(raw_file) is not str or type(raw_function) is not str:
            continue
        if type(raw_line) is not int or not 0 <= raw_line <= _MAX_LINE_NUMBER:
            continue
        frames.append(
            {
                "file": _safe_file_name(raw_file),
                "function": _safe_function_name(raw_function),
                "line": raw_line,
            }
        )
    return frames, len(value) > limit or len(frames) != len(value)


def _coerce_errno(value: Any) -> int | None:
    if type(value) is int and _MIN_SAFE_ERRNO <= value <= _MAX_SAFE_ERRNO:
        return value
    return None


def _coerce_exception_group(value: Any) -> dict[str, Any] | None:
    if type(value) is not dict:
        return None
    raw_members = value.get("members")
    members = (
        [
            _coerce_exception_node(
                member,
                frame_limit=_MAX_GROUP_MEMBER_FRAMES,
                include_group=False,
            )
            for member in raw_members[:_MAX_GROUP_MEMBERS]
            if type(member) is dict
        ]
        if type(raw_members) is list
        else []
    )
    raw_count = value.get("member_count")
    member_count = (
        raw_count
        if type(raw_count) is int and 0 <= raw_count <= _MAX_REPORTED_COUNT
        else len(members)
    )
    return {
        "member_count": max(member_count, len(members)),
        "members_truncated": value.get("members_truncated") is True
        or (type(raw_members) is list and len(raw_members) > len(members))
        or member_count > len(members),
        "members": members,
    }


def _coerce_exception_node(
    value: dict[str, Any],
    *,
    frame_limit: int,
    include_group: bool = True,
    include_relation: bool = False,
) -> dict[str, Any]:
    raw_type = value.get("exception_type")
    exception_type = (
        _bounded_text(raw_type, limit=_MAX_EXCEPTION_TYPE_CHARS, fallback="Exception")
        if type(raw_type) is str
        else "Exception"
    )
    raw_frames = value.get("frames")
    frames, invalid_or_truncated_frames = _coerce_frames(raw_frames, limit=frame_limit)
    args_shape = _coerce_args_shape(value.get("args_shape"))
    errno_value = _coerce_errno(value.get("errno"))
    group = _coerce_exception_group(value.get("exception_group")) if include_group else None
    node: dict[str, Any] = {
        "exception_type": exception_type,
        "fingerprint": _fingerprint(
            exception_type,
            frames,
            args_shape,
            errno_value=errno_value,
            group=group,
        ),
        "frames_truncated": value.get("frames_truncated") is True or invalid_or_truncated_frames,
        "frames": frames,
        "args_shape": args_shape,
    }
    if include_relation:
        relation = value.get("relation")
        if type(relation) is str and relation in _CHAIN_RELATIONS:
            node = {"relation": relation, **node}
    if errno_value is not None:
        node["errno"] = errno_value
    if group is not None:
        node["exception_group"] = group
    return node


def _coerce_internal_exception_diagnostic(value: Any) -> dict[str, Any] | None:
    """Validate formatter input and discard every unrecognised field.

    The helper already emits this exact shape.  Re-validating in formatters is
    defense in depth: an arbitrary ``extra`` mapping cannot smuggle an
    exception message or object into rendered output under this field.
    """
    if type(value) is not dict:
        return None
    root = _coerce_exception_node(value, frame_limit=_MAX_FRAMES)
    raw_chain = value.get("chain")
    chain: list[dict[str, Any]] = []
    if type(raw_chain) is list:
        for item in raw_chain[:_MAX_CHAIN_DEPTH]:
            if type(item) is not dict:
                continue
            node = _coerce_exception_node(
                item,
                frame_limit=_MAX_CHAIN_FRAMES,
                include_relation=True,
            )
            if node.get("relation") in _CHAIN_RELATIONS:
                chain.append(node)
    root["chain_truncated"] = value.get("chain_truncated") is True or (
        type(raw_chain) is list and len(raw_chain) > len(chain)
    )
    root["chain"] = chain
    root["fingerprint"] = _fingerprint(
        root["exception_type"],
        root["frames"],
        root["args_shape"],
        errno_value=root.get("errno"),
        group=root.get("exception_group"),
        chain=chain,
    )
    return {"schema_version": _SCHEMA_VERSION, **root}


def _emit_internal_exception(
    logger: logging.Logger | str,
    event: str,
    exc: BaseException,
    *,
    level: int,
    stacklevel: int,
) -> None:
    safe_event = (
        event
        if isinstance(event, str)
        and len(event) <= _MAX_EVENT_CHARS
        and _SAFE_EVENT_RE.fullmatch(event)
        else "internal.exception"
    )
    diagnostic = _build_internal_exception_diagnostic(exc)
    target_logger = logging.getLogger(logger) if isinstance(logger, str) else logger
    target_logger.log(
        level,
        "%s exception_type=%s fingerprint=%s",
        safe_event,
        diagnostic["exception_type"],
        diagnostic["fingerprint"],
        extra={"internal_exception": diagnostic},
        stacklevel=stacklevel,
    )


def log_internal_exception(
    logger: logging.Logger | str,
    event: str,
    exc: BaseException,
    *,
    level: int = logging.ERROR,
) -> None:
    """Log a caught internal exception without its message or traceback.

    ``event`` is expected to be a static, low-cardinality identifier such as
    ``"assistant.responses.stream.failed"``.  No ``exc_info`` is supplied, so
    the resulting record cannot render or retain the raw exception.
    """
    _emit_internal_exception(logger, event, exc, level=level, stacklevel=3)


def record_internal_exception(
    logger: logging.Logger | str,
    event: str,
    exc: BaseException,
    *,
    level: int = logging.ERROR,
) -> None:
    """Alias with a migration-friendly name for catch-boundary source gates."""

    _emit_internal_exception(logger, event, exc, level=level, stacklevel=3)
