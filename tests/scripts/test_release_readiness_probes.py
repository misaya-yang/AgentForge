"""Offline Gateway probe contract; Helm rendering never contacts a cluster."""

from __future__ import annotations

import json
import re
import shutil
import subprocess
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
CHART = ROOT / "deploy/helm/ai-gateway"
HELM = shutil.which("helm")
EXPECTED_PATHS = {"liveness": "/health/live", "readiness": "/health/ready"}


def _yaml(path: Path) -> dict:
    return yaml.safe_load(path.read_text(encoding="utf-8"))


class GatewayProbeConfigurationTests(unittest.TestCase):
    def test_default_values_separate_process_and_admission(self) -> None:
        probes = _yaml(CHART / "values.yaml")["gateway"]["probes"]
        for name, path in EXPECTED_PATHS.items():
            with self.subTest(probe=name):
                self.assertEqual(probes[name]["path"], path)
                self.assertEqual(probes[name]["port"], 8080)

    def test_production_values_inherit_or_preserve_probe_contract(self) -> None:
        defaults = _yaml(CHART / "values.yaml")["gateway"]["probes"]
        overrides = _yaml(CHART / "values-production.yaml")["gateway"].get("probes", {})
        for name, path in EXPECTED_PATHS.items():
            effective = defaults[name] | overrides.get(name, {})
            with self.subTest(probe=name):
                self.assertEqual(effective["path"], path)
                self.assertEqual(effective["port"], 8080)

    def test_template_keeps_operator_path_port_and_timing_overrides(self) -> None:
        template = (CHART / "templates/gateway-deployment.yaml").read_text(encoding="utf-8")
        for name in EXPECTED_PATHS:
            block = template.split(f"          {name}Probe:\n", 1)[1]
            block = re.split(r"\n          \w", block, maxsplit=1)[0]
            for field in ("path", "port", "initialDelaySeconds", "periodSeconds"):
                with self.subTest(probe=name, field=field):
                    self.assertIn(f"{field}: {{{{ .Values.gateway.probes.{name}.{field} }}}}", block)

    def test_compose_profiles_retain_gateway_readiness(self) -> None:
        base = _yaml(ROOT / "docker-compose.yml")["services"]["gateway"]["healthcheck"]
        self.assertEqual(base["test"], ["CMD", "curl", "-f", "http://localhost:8080/health/ready"])
        for path in sorted(ROOT.glob("docker-compose.*.yml")):
            gateway = _yaml(path).get("services", {}).get("gateway", {})
            probe = gateway.get("healthcheck", base)
            with self.subTest(profile=path.name):
                self.assertFalse(probe.get("disable", False))
                self.assertEqual(probe.get("test", base["test"]), base["test"])

    def test_status_gateway_check_uses_fail_closed_readiness(self) -> None:
        common = (ROOT / "scripts/new/common.sh").read_text(encoding="utf-8")
        function = re.search(r"(?ms)^check_gateway_health\(\) \{.*?^\}", common)
        self.assertIsNotNone(function)
        cases = json.loads(
            (ROOT / "tests/fixtures/deployment/gateway-readiness.v1.json")
            .read_text(encoding="utf-8")
        )["cases"]
        # Execute the real shell function, with only its HTTP client replaced.
        # The stub models curl -f: an HTTP 503 must not become a successful gate.
        fake_curl = '''
            curl() {
                [ "$2" = "http://localhost:18080/health/ready" ] || return 99
                if [ "$CURL_HTTP_STATUS" -ge 400 ]; then
                    case "$1" in *f*) return 22 ;; esac
                fi
                return 0
            }
        '''
        for case in cases:
            with self.subTest(case=case["id"]):
                result = subprocess.run(
                    ["bash", "-c", fake_curl + function.group(0)
                     + f"\nGATEWAY_PORT=18080; CURL_HTTP_STATUS={case['expected_status']}; check_gateway_health"],
                    capture_output=True, text=True, check=False, timeout=5,
                )
                self.assertEqual(result.returncode, 0 if case["expected_status"] == 200 else 22)


@unittest.skipUnless(HELM, "helm is not installed; real template rendering was not verified")
class HelmGatewayProbeRenderingTests(unittest.TestCase):
    def render(self, *args: str) -> dict[str, dict]:
        result = subprocess.run(
            [HELM, "template", "probe-contract", str(CHART),
             "--set-string", "secrets.postgresPassword=probe-fixture-only",
             "--set-string", "secrets.redisPassword=probe-fixture-only",
             "--set-string", "secrets.jwtSecret=probe-fixture-only", *args],
            capture_output=True, text=True, check=False, timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return {
            container["name"]: container
            for doc in yaml.safe_load_all(result.stdout)
            if doc and doc.get("kind") == "Deployment"
            for container in doc["spec"]["template"]["spec"]["containers"]
        }

    def assert_default_gateway(self, containers: dict[str, dict]) -> None:
        for name, path in EXPECTED_PATHS.items():
            self.assertEqual(containers["gateway"][f"{name}Probe"]["httpGet"], {
                "path": path, "port": 8080,
            })

    def test_default_template_and_non_gateway_probes(self) -> None:
        containers = self.render()
        self.assert_default_gateway(containers)
        for name in EXPECTED_PATHS:
            self.assertEqual(containers["frontend"][f"{name}Probe"]["httpGet"]["path"], "/health")
            self.assertEqual(
                containers["knowledge-service"][f"{name}Probe"]["httpGet"]["path"],
                EXPECTED_PATHS[name],
            )

    def test_production_template(self) -> None:
        self.assert_default_gateway(self.render("-f", str(CHART / "values-production.yaml")))

    def test_operator_probe_overrides_survive_rendering(self) -> None:
        containers = self.render(
            "--set-string", "gateway.probes.liveness.path=/operator/live",
            "--set-string", "gateway.probes.readiness.path=/operator/ready",
            "--set", "gateway.probes.liveness.port=18080",
            "--set", "gateway.probes.readiness.port=18081",
            "--set", "gateway.probes.readiness.initialDelaySeconds=17",
            "--set", "gateway.probes.readiness.periodSeconds=19",
        )
        gateway = containers["gateway"]
        self.assertEqual(gateway["livenessProbe"]["httpGet"], {"path": "/operator/live", "port": 18080})
        self.assertEqual(gateway["readinessProbe"]["httpGet"], {"path": "/operator/ready", "port": 18081})
        self.assertEqual(gateway["readinessProbe"]["initialDelaySeconds"], 17)
        self.assertEqual(gateway["readinessProbe"]["periodSeconds"], 19)
        self.assertEqual(containers["frontend"]["readinessProbe"]["httpGet"]["path"], "/health")


if __name__ == "__main__":
    unittest.main()
