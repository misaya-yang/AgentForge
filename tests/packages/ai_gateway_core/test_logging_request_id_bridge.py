"""PR-3: bridge tests — REQUEST_ID_CTX + OTel context → log records.

Verifies that ``configure_structured_logging`` wires ``ContextFilter``
onto every handler so the 2,443 existing ``logger.info(...)`` call
sites pick up ``request_id`` / ``trace_id`` / ``span_id`` / ``service``
without any source edits.

The tests deliberately attach a fresh ``StreamHandler`` writing to an
``io.StringIO`` so we don't have to fight ``capsys`` interaction with
``StreamHandler(sys.stdout)`` (FastAPI / pytest both reassign stdout
mid-run, which makes ``capsys`` flaky for stdlib logging output).
"""

from __future__ import annotations

import builtins
import io
import json
import logging

import pytest
from ai_gateway_core.logging import (
    ContextFilter,
    LogContext,
    SimpleFormatter,
    StructuredFormatter,
    clear_log_context,
    configure_structured_logging,
    set_log_context,
)
from ai_gateway_core.proxy.request_id_middleware import REQUEST_ID_CTX


@pytest.fixture
def json_capture():
    """Configure JSON logging and return (logger, buffer) for assertions.

    Uses ``log_to_file=False`` so the test never touches the filesystem.
    Adds a buffer-backed handler with ``ContextFilter`` attached so each
    record gets stamped exactly the way the production console handler
    would.
    """
    configure_structured_logging(
        level="DEBUG",
        format_type="json",
        log_to_file=False,
        service="test-service",
    )

    buf = io.StringIO()
    h = logging.StreamHandler(buf)
    h.setFormatter(StructuredFormatter())
    h.addFilter(ContextFilter())
    logging.getLogger().addHandler(h)

    yield logging.getLogger("bridge_test"), buf

    logging.getLogger().removeHandler(h)
    # Reset legacy LogContext so adjacent tests don't see leakage.
    clear_log_context()


@pytest.fixture
def simple_capture():
    """Configure SIMPLE logging and capture output."""
    configure_structured_logging(
        level="DEBUG",
        format_type="simple",
        log_to_file=False,
        service="test-service",
    )
    buf = io.StringIO()
    h = logging.StreamHandler(buf)
    h.setFormatter(SimpleFormatter(use_colors=False))
    h.addFilter(ContextFilter())
    logging.getLogger().addHandler(h)
    yield logging.getLogger("simple_test"), buf
    logging.getLogger().removeHandler(h)


def _last_json_line(buf: io.StringIO) -> dict:
    raw = buf.getvalue().strip().splitlines()
    assert raw, "no log lines produced"
    return json.loads(raw[-1])


def test_request_id_stamped_when_contextvar_set(json_capture):
    """REQUEST_ID_CTX → record.request_id → top-level JSON key."""
    logger, buf = json_capture
    token = REQUEST_ID_CTX.set("req-XYZ")
    try:
        logger.info("hi")
    finally:
        REQUEST_ID_CTX.reset(token)

    payload = _last_json_line(buf)
    assert payload["request_id"] == "req-XYZ"
    assert payload["message"] == "hi"
    assert payload["level"] == "INFO"
    assert payload["timestamp"]
    assert payload["service"] == "test-service"
    assert payload["logger"] == "bridge_test"


def test_no_request_id_when_outside_request(json_capture):
    """No contextvar → still valid JSON, request_id omitted."""
    logger, buf = json_capture
    # Clear any stray value (test isolation hygiene).
    REQUEST_ID_CTX.set("")
    logger.info("no ctx")
    payload = _last_json_line(buf)
    assert payload["message"] == "no ctx"
    # Either absent or falsy — both acceptable.
    assert not payload.get("request_id")


def test_otel_trace_id_stamped_when_span_active(json_capture):
    """Active OTel span → record.trace_id (32-hex) + record.span_id (16-hex)."""
    pytest.importorskip("opentelemetry")
    from opentelemetry import trace
    from opentelemetry.sdk.trace import TracerProvider

    # Tests reset the provider per-run so multiple test runs don't pollute
    # each other. Idempotent set_tracer_provider is a no-op after first
    # call; we use whatever the env gives us.
    if not isinstance(trace.get_tracer_provider(), TracerProvider):
        trace.set_tracer_provider(TracerProvider())

    logger, buf = json_capture
    tracer = trace.get_tracer("bridge_test")
    with tracer.start_as_current_span("unit-span"):
        logger.info("with span")

    payload = _last_json_line(buf)
    assert "trace_id" in payload
    assert "span_id" in payload
    assert len(payload["trace_id"]) == 32
    assert len(payload["span_id"]) == 16
    # Hex sanity.
    int(payload["trace_id"], 16)
    int(payload["span_id"], 16)


def test_simple_format_unchanged(simple_capture):
    """``simple`` format remains human-readable, NOT JSON."""
    logger, buf = simple_capture
    logger.info("hello dev")
    output = buf.getvalue().strip()
    # Not JSON.
    with pytest.raises(json.JSONDecodeError):
        json.loads(output)
    # Has level + name + message.
    assert "INFO" in output
    assert "hello dev" in output
    assert "simple_test" in output


def test_existing_set_log_context_still_works(json_capture):
    """Legacy ``LogContext`` API keeps merging into JSON output."""
    logger, buf = json_capture
    set_log_context(LogContext(user_id="u1", tenant_id="t1"))
    try:
        logger.info("with legacy ctx")
    finally:
        clear_log_context()

    payload = _last_json_line(buf)
    assert payload["user_id"] == "u1"
    assert payload["tenant_id"] == "t1"
    assert payload["message"] == "with legacy ctx"


def test_request_id_present_at_warning_and_error(json_capture):
    """request_id stamping is level-agnostic — warning + error too."""
    logger, buf = json_capture
    token = REQUEST_ID_CTX.set("req-multi-level")
    try:
        logger.warning("warn line")
        logger.error("err line")
    finally:
        REQUEST_ID_CTX.reset(token)

    lines = [json.loads(line) for line in buf.getvalue().strip().splitlines()]
    assert all(rec["request_id"] == "req-multi-level" for rec in lines[-2:])
    assert lines[-2]["level"] == "WARNING"
    assert lines[-1]["level"] == "ERROR"


def test_service_default_falls_back_when_not_passed(monkeypatch):
    """No ``service=`` arg + no SERVICE_NAME env → 'ai-gateway' default."""
    # Reload the module-global to its env-derived default.
    monkeypatch.delenv("SERVICE_NAME", raising=False)
    import importlib

    from ai_gateway_core.logging import _core as core_mod

    importlib.reload(core_mod)

    # Re-fetch symbols from reloaded module.
    core_mod.configure_structured_logging(format_type="json", log_to_file=False)
    buf = io.StringIO()
    h = logging.StreamHandler(buf)
    h.setFormatter(core_mod.StructuredFormatter())
    h.addFilter(core_mod.ContextFilter())
    logging.getLogger().addHandler(h)
    try:
        logging.getLogger("svc_default").info("x")
        payload = json.loads(buf.getvalue().strip().splitlines()[-1])
        assert payload["service"] == "ai-gateway"
    finally:
        logging.getLogger().removeHandler(h)


class _RecordCollector(logging.Handler):
    def __init__(self) -> None:
        super().__init__()
        self.records: list[logging.LogRecord] = []

    def emit(self, record: logging.LogRecord) -> None:
        self.records.append(record)


def _assert_primitive_tree(value) -> None:
    assert type(value) in {type(None), bool, int, str, list, dict}
    if type(value) is list:
        for item in value:
            _assert_primitive_tree(item)
    elif type(value) is dict:
        assert all(type(key) is str for key in value)
        for item in value.values():
            _assert_primitive_tree(item)


def _raise_internal_failure() -> None:
    local_private_value = "private-local-value-that-must-not-be-logged"
    raise RuntimeError(f"private-exception-message-that-must-not-be-logged {local_private_value}")


def _raise_recursive_failure(depth: int) -> None:
    if depth <= 0:
        _raise_internal_failure()
    _raise_recursive_failure(depth - 1)


def test_internal_exception_helper_emits_safe_structured_diagnostic(json_capture) -> None:
    from ai_gateway_core.logging import log_internal_exception

    logger, buf = json_capture
    collector = _RecordCollector()
    logger.addHandler(collector)
    try:
        try:
            _raise_internal_failure()
        except RuntimeError as exc:
            log_internal_exception(logger, "test.internal_failure", exc)
    finally:
        logger.removeHandler(collector)

    payload = _last_json_line(buf)
    diagnostic = payload["internal_exception"]
    assert payload["message"].startswith("test.internal_failure exception_type=RuntimeError ")
    assert diagnostic["schema_version"] == "internal-exception/v2"
    assert diagnostic["exception_type"] == "RuntimeError"
    assert len(diagnostic["fingerprint"]) == 16
    int(diagnostic["fingerprint"], 16)
    assert diagnostic["frames_truncated"] is False
    assert diagnostic["frames"]
    assert all(set(frame) == {"file", "function", "line"} for frame in diagnostic["frames"])
    last_frame = diagnostic["frames"][-1]
    assert last_frame["file"].endswith("test_logging_request_id_bridge.py")
    assert "/" not in last_frame["file"] and "\\" not in last_frame["file"]
    assert last_frame["function"] == "_raise_internal_failure"
    assert isinstance(last_frame["line"], int) and last_frame["line"] > 0

    rendered = buf.getvalue()
    assert "private-exception-message-that-must-not-be-logged" not in rendered
    assert "private-local-value-that-must-not-be-logged" not in rendered
    assert "exception_message" not in diagnostic
    assert collector.records
    record = collector.records[-1]
    assert record.exc_info is None
    assert record.exc_text is None
    assert not any(isinstance(value, BaseException) for value in vars(record).values())
    _assert_primitive_tree(record.internal_exception)


def test_structured_formatter_drops_uncontrolled_internal_exception_fields(json_capture) -> None:
    logger, buf = json_capture
    raw_marker = "private-uncontrolled-exception-value"

    logger.error(
        "test.untrusted_internal_diagnostic",
        extra={
            "internal_exception": {
                "schema_version": "untrusted",
                "exception_type": "RuntimeError",
                "fingerprint": "a" * 16,
                "frames_truncated": False,
                "frames": [
                    {
                        "file": f"/private/{raw_marker}/module.py",
                        "function": "failing_function",
                        "line": 7,
                        "source": raw_marker,
                        "locals": {"secret": raw_marker},
                    }
                ],
                "exception_message": raw_marker,
                "raw_exception": RuntimeError(raw_marker),
            }
        },
    )

    payload = _last_json_line(buf)
    diagnostic = payload["internal_exception"]
    assert diagnostic["schema_version"] == "internal-exception/v2"
    assert diagnostic["frames"] == [
        {"file": "module.py", "function": "failing_function", "line": 7}
    ]
    assert set(diagnostic) == {
        "schema_version",
        "exception_type",
        "fingerprint",
        "frames_truncated",
        "frames",
        "args_shape",
        "chain_truncated",
        "chain",
    }
    assert raw_marker not in buf.getvalue()


def test_internal_exception_helper_accepts_module_name(json_capture) -> None:
    from ai_gateway_core.logging import log_internal_exception

    _logger, buf = json_capture
    try:
        _raise_internal_failure()
    except RuntimeError as exc:
        log_internal_exception("bridge_test", "test.module_name_logger", exc)

    payload = _last_json_line(buf)
    assert payload["logger"] == "bridge_test"
    assert payload["message"].startswith("test.module_name_logger exception_type=RuntimeError ")
    assert payload["internal_exception"]["exception_type"] == "RuntimeError"


def test_internal_exception_helper_bounds_traceback_frames(json_capture) -> None:
    from ai_gateway_core.logging import log_internal_exception

    logger, buf = json_capture
    try:
        _raise_recursive_failure(100)
    except RuntimeError as exc:
        log_internal_exception(logger, "test.recursive_internal_failure", exc)

    diagnostic = _last_json_line(buf)["internal_exception"]
    assert diagnostic["frames_truncated"] is True
    assert 1 <= len(diagnostic["frames"]) <= 24
    assert len(buf.getvalue().encode("utf-8")) < 16_384


def test_internal_exception_helper_is_visible_in_simple_formatter(simple_capture) -> None:
    from ai_gateway_core.logging import log_internal_exception

    logger, buf = simple_capture
    try:
        _raise_internal_failure()
    except RuntimeError as exc:
        log_internal_exception(logger, "test.simple_internal_failure", exc)

    rendered = buf.getvalue()
    assert "test.simple_internal_failure" in rendered
    assert "internal_exception=" in rendered
    assert '"exception_type":"RuntimeError"' in rendered
    assert '"function":"_raise_internal_failure"' in rendered
    assert "private-exception-message-that-must-not-be-logged" not in rendered
    assert "private-local-value-that-must-not-be-logged" not in rendered


class _PoisonDiagnosticArg:
    str_calls = 0
    repr_calls = 0

    def __str__(self) -> str:
        type(self).str_calls += 1
        return "private-poison-str-value"

    def __repr__(self) -> str:
        type(self).repr_calls += 1
        return "private-poison-repr-value"


def _raise_oserror(error_number: int, marker: str) -> None:
    raise OSError(error_number, marker, f"/private/{marker}")


def test_internal_exception_v2_records_closed_args_shape_without_rendering_values(
    json_capture,
) -> None:
    from ai_gateway_core.logging import log_internal_exception

    logger, buf = json_capture
    _PoisonDiagnosticArg.str_calls = 0
    _PoisonDiagnosticArg.repr_calls = 0
    marker = "private-args-shape-value"
    try:
        raise RuntimeError(_PoisonDiagnosticArg(), marker, 7, {"secret": marker})
    except RuntimeError as exc:
        log_internal_exception(logger, "test.args_shape", exc)

    diagnostic = _last_json_line(buf)["internal_exception"]
    assert diagnostic["schema_version"] == "internal-exception/v2"
    assert diagnostic["args_shape"] == {
        "count": 4,
        "truncated": False,
        "kinds": ["other", "text", "int", "mapping"],
    }
    assert _PoisonDiagnosticArg.str_calls == 0
    assert _PoisonDiagnosticArg.repr_calls == 0
    rendered = buf.getvalue()
    assert marker not in rendered
    assert "private-poison-str-value" not in rendered
    assert "private-poison-repr-value" not in rendered


def test_internal_exception_v2_errno_is_bounded_and_changes_fingerprint(json_capture) -> None:
    from ai_gateway_core.logging import log_internal_exception

    logger, buf = json_capture
    marker = "private-oserror-value"
    for error_number in (1001, 1002):
        try:
            _raise_oserror(error_number, marker)
        except OSError as exc:
            log_internal_exception(logger, "test.oserror", exc)

    payloads = [json.loads(line) for line in buf.getvalue().strip().splitlines()][-2:]
    diagnostics = [payload["internal_exception"] for payload in payloads]
    assert [diagnostic["errno"] for diagnostic in diagnostics] == [1001, 1002]
    assert diagnostics[0]["fingerprint"] != diagnostics[1]["fingerprint"]
    assert marker not in buf.getvalue()


@pytest.mark.parametrize("unsafe_errno", [True, 65_536, -65_536])
def test_internal_exception_v2_omits_non_integer_or_out_of_range_errno(
    json_capture,
    unsafe_errno,
) -> None:
    from ai_gateway_core.logging import log_internal_exception

    logger, buf = json_capture
    exc = OSError("private-unsafe-errno-value")
    exc.errno = unsafe_errno
    log_internal_exception(logger, "test.unsafe_errno", exc)

    diagnostic = _last_json_line(buf)["internal_exception"]
    assert "errno" not in diagnostic
    assert "private-unsafe-errno-value" not in buf.getvalue()


def test_internal_exception_v2_cause_chain_is_bounded_and_cycle_safe(json_capture) -> None:
    from ai_gateway_core.logging import log_internal_exception

    logger, buf = json_capture
    marker = "private-cause-chain-value"
    inner = OSError(5, marker, f"/private/{marker}")
    outer = RuntimeError(marker)
    outer.__cause__ = inner
    inner.__cause__ = outer

    try:
        raise outer
    except RuntimeError as exc:
        log_internal_exception(logger, "test.cause_chain", exc)

    diagnostic = _last_json_line(buf)["internal_exception"]
    assert diagnostic["chain_truncated"] is True
    assert len(diagnostic["chain"]) == 1
    assert diagnostic["chain"][0]["relation"] == "cause"
    assert diagnostic["chain"][0]["exception_type"] == "OSError"
    assert diagnostic["chain"][0]["errno"] == 5
    assert marker not in buf.getvalue()


def test_internal_exception_v2_records_implicit_context_without_messages(json_capture) -> None:
    from ai_gateway_core.logging import log_internal_exception

    logger, buf = json_capture
    marker = "private-context-chain-value"
    try:
        try:
            raise ValueError(marker)
        except ValueError:
            raise RuntimeError(marker)
    except RuntimeError as exc:
        log_internal_exception(logger, "test.context_chain", exc)

    diagnostic = _last_json_line(buf)["internal_exception"]
    assert diagnostic["chain_truncated"] is False
    assert len(diagnostic["chain"]) == 1
    assert diagnostic["chain"][0]["relation"] == "context"
    assert diagnostic["chain"][0]["exception_type"] == "ValueError"
    assert marker not in buf.getvalue()


def test_internal_exception_v2_cause_chain_depth_is_bounded(json_capture) -> None:
    from ai_gateway_core.logging import log_internal_exception

    logger, buf = json_capture
    marker = "private-deep-cause-value"
    errors = [RuntimeError(marker) for _ in range(10)]
    for current, linked in zip(errors[:-1], errors[1:], strict=True):
        current.__cause__ = linked

    try:
        raise errors[0]
    except RuntimeError as exc:
        log_internal_exception(logger, "test.deep_cause_chain", exc)

    diagnostic = _last_json_line(buf)["internal_exception"]
    assert diagnostic["chain_truncated"] is True
    assert len(diagnostic["chain"]) == 4
    assert marker not in buf.getvalue()


def test_internal_exception_v2_exception_group_summary_is_bounded(json_capture) -> None:
    from ai_gateway_core.logging import log_internal_exception

    logger, buf = json_capture
    collector = _RecordCollector()
    logger.addHandler(collector)
    marker = "private-exception-group-value"
    members = [ValueError(marker), OSError(13, marker, f"/private/{marker}")]
    members.extend(RuntimeError(f"{marker}-{index}") for index in range(10))
    group_error = builtins.ExceptionGroup(marker, members)
    try:
        try:
            raise group_error
        except BaseException as exc:
            log_internal_exception(logger, "test.exception_group", exc)
    finally:
        logger.removeHandler(collector)

    diagnostic = _last_json_line(buf)["internal_exception"]
    group = diagnostic["exception_group"]
    assert group["member_count"] == 12
    assert group["members_truncated"] is True
    assert len(group["members"]) == 8
    assert group["members"][0]["exception_type"] == "ValueError"
    assert group["members"][1]["exception_type"] == "PermissionError"
    assert all(len(member["fingerprint"]) == 16 for member in group["members"])
    assert collector.records[-1].exc_info is None
    _assert_primitive_tree(collector.records[-1].internal_exception)
    assert marker not in buf.getvalue()


def test_v2_formatter_rejects_uncontrolled_nested_diagnostic_fields(json_capture) -> None:
    logger, buf = json_capture
    marker = "private-nested-diagnostic-value"
    logger.error(
        "test.untrusted_nested_diagnostic",
        extra={
            "internal_exception": {
                "exception_type": "RuntimeError",
                "frames": [],
                "args_shape": {
                    "count": 2,
                    "truncated": False,
                    "kinds": ["text", marker],
                    "values": [marker],
                },
                "chain": [
                    {
                        "relation": "cause",
                        "exception_type": "OSError",
                        "frames": [],
                        "errno": 13,
                        "message": marker,
                        "locals": {"secret": marker},
                    }
                ],
                "exception_group": {
                    "member_count": 1,
                    "members": [
                        {
                            "exception_type": "ValueError",
                            "frames": [],
                            "message": marker,
                        }
                    ],
                    "raw_members": [RuntimeError(marker)],
                },
                "message": marker,
            }
        },
    )

    diagnostic = _last_json_line(buf)["internal_exception"]
    assert diagnostic["schema_version"] == "internal-exception/v2"
    assert diagnostic["args_shape"]["kinds"] == ["text"]
    assert diagnostic["chain"][0]["errno"] == 13
    assert set(diagnostic["chain"][0]).issubset(
        {
            "relation",
            "exception_type",
            "fingerprint",
            "frames_truncated",
            "frames",
            "args_shape",
            "errno",
            "exception_group",
        }
    )
    assert marker not in buf.getvalue()
