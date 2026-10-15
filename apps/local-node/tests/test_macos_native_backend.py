from __future__ import annotations

import json
import platform
import shutil
import stat
import subprocess
from pathlib import Path

import pytest

from local_node.computer import ComputerAction
from local_node.errors import CapabilityDenied, StaleTargetError
from local_node.macos_native import (
    MacOSNativeComputerBackend,
    NativeComputerUseDenied,
    NativeTakeoverRequired,
)


def _backend(tmp_path: Path, monkeypatch, *, origins=frozenset()):
    helper = tmp_path / "trusted-helper"
    helper.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    helper.chmod(0o700)
    backend = MacOSNativeComputerBackend(
        helper_path=helper,
        session_id="session-native-test",
        allowed_apps=frozenset({"com.example.SafeApp"}),
        allowed_origins=origins,
    )
    return backend


def _observation_response(*, origin=None, flags=()):
    return {
        "ok": True,
        "app_id": "com.example.SafeApp",
        "window_id": "42",
        "window_title": "Live fixture",
        "width": 800,
        "height": 600,
        "x": 100,
        "y": 200,
        "frontmost": True,
        "origin": origin,
        "accessibility_lines": ["AXWindow | Live fixture", "AXStaticText | SAFE READBACK"],
        "risk_flags": list(flags),
    }


def test_backend_binds_app_origin_session_window_and_latest_observation(tmp_path, monkeypatch):
    backend = _backend(tmp_path, monkeypatch, origins=frozenset({"https://example.test"}))
    responses = iter(
        [
            _observation_response(origin="https://example.test"),
            {"ok": True, "kind": "click"},
        ]
    )
    monkeypatch.setattr(backend, "_invoke", lambda *args, **kwargs: next(responses))
    observed = backend.observe("com.example.SafeApp", None)
    details = backend.last_details
    assert details is not None
    assert details.session_id == "session-native-test"
    assert observed.window_id == "42"
    assert observed.origin == "https://example.test"
    backend.execute(ComputerAction("click", observed.observation_id, {"x": 20, "y": 30}))

    with pytest.raises(StaleTargetError):
        backend.execute(ComputerAction("click", "obs_stale", {"x": 20, "y": 30}))
    with pytest.raises(NativeComputerUseDenied, match="outside"):
        backend.observe("com.example.OtherApp", None)


def test_backend_refuses_sensitive_ui_secret_text_and_control_after_stop(tmp_path, monkeypatch):
    backend = _backend(tmp_path, monkeypatch)
    monkeypatch.setattr(
        backend,
        "_invoke",
        lambda *args, **kwargs: _observation_response(flags=("secure_text_field",)),
    )
    with pytest.raises(NativeTakeoverRequired):
        backend.observe("com.example.SafeApp", None)

    monkeypatch.setattr(backend, "_invoke", lambda *args, **kwargs: {"ok": True})
    backend.stop()
    monkeypatch.setattr(backend, "_invoke", lambda *args, **kwargs: _observation_response())
    observed = backend.observe("com.example.SafeApp", None)
    backend.stop()
    with pytest.raises(StaleTargetError):
        backend.execute(ComputerAction("click", observed.observation_id, {"x": 1, "y": 1}))

    monkeypatch.setattr(backend, "_invoke", lambda *args, **kwargs: _observation_response())
    clean = backend.observe("com.example.SafeApp", None)
    with pytest.raises(CapabilityDenied, match="secret-like"):
        backend.execute(
            ComputerAction(
                "type_text",
                clean.observation_id,
                {"text": "sk-SECRET_CANARY_MUST_NOT_LEAK"},
            )
        )


def test_backend_rejects_untrusted_helper_path(tmp_path):
    helper = tmp_path / "helper"
    helper.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    helper.chmod(stat.S_IRWXU | stat.S_IWOTH)
    with pytest.raises(CapabilityDenied):
        MacOSNativeComputerBackend(
            helper_path=helper,
            session_id="session-a",
            allowed_apps=frozenset({"com.example.SafeApp"}),
        )


@pytest.mark.skipif(platform.system() != "Darwin", reason="native helper is macOS-only")
def test_bundled_swift_helper_compiles_and_reports_real_permission_truth(tmp_path):
    swiftc = shutil.which("swiftc", path="/usr/bin:/Library/Developer/CommandLineTools/usr/bin")
    if swiftc is None:
        pytest.skip("Swift compiler is unavailable")
    source = Path(__file__).resolve().parents[1] / "native" / "macos" / "LocalNodeMacOSHelper.swift"
    helper = tmp_path / "local-node-macos-helper"
    compiled = subprocess.run(
        (
            swiftc,
            "-O",
            "-framework",
            "AppKit",
            "-framework",
            "ApplicationServices",
            "-framework",
            "CoreGraphics",
            "-framework",
            "Security",
            str(source),
            "-o",
            str(helper),
        ),
        check=False,
        capture_output=True,
        timeout=30,
        env={"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "C", "LC_ALL": "C"},
    )
    assert compiled.returncode == 0, compiled.stderr.decode("utf-8", errors="replace")
    completed = subprocess.run(
        (str(helper),),
        input=b'{"command":"doctor"}',
        check=False,
        capture_output=True,
        timeout=5,
        env={"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "C", "LC_ALL": "C"},
    )
    assert completed.returncode == 0, completed.stdout
    report = json.loads(completed.stdout)
    assert set(report) == {"accessibility", "ok", "screen_recording"}
    assert report["ok"] is True
    assert isinstance(report["accessibility"], bool)
    assert isinstance(report["screen_recording"], bool)
