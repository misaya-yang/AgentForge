from __future__ import annotations

import json

import pytest

from local_node.cli import build_parser, main


def test_doctor_never_starts_network_or_claims_secure_readiness(capsys):
    assert main(["doctor", "--endpoint", "https://control.example.test/node"]) == 2
    report = json.loads(capsys.readouterr().out)
    assert report["status"] == "unavailable"
    assert report["network_started"] is False
    assert report["listener"] == "disabled"
    assert report["endpoint"] == "ready"
    assert report["transport"] == "available_not_started"
    assert report["secure_credential_storage"] == "unavailable"
    assert report["platform_signature_verifier"] == "unavailable"
    assert report["trusted_local_approval"] == "unavailable"


def test_cli_has_no_secret_or_serve_flags(capsys):
    parser = build_parser()
    help_text = parser.format_help().casefold()
    assert "serve" not in help_text
    assert "--token" not in help_text
    assert "--credential" not in help_text
    assert "--secret" not in help_text

    with pytest.raises(SystemExit):
        parser.parse_args(["doctor", "--credential", "do-not-print"])
    assert "do-not-print" not in capsys.readouterr().err


def test_doctor_rejects_endpoint_credentials_without_echo(capsys):
    endpoint = "https://user:sensitive-password@control.example.test/node"
    assert main(["doctor", "--endpoint", endpoint]) == 2
    output = capsys.readouterr().out
    assert "sensitive-password" not in output
    assert json.loads(output)["endpoint"] == "denied"
