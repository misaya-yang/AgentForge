from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def _block(source: str, marker: str) -> str:
    start = source.index(marker)
    opening = source.index("{", start)
    depth = 0
    for index in range(opening, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return source[start : index + 1]
    raise AssertionError(f"unterminated Nginx block: {marker}")


def test_web_nginx_keeps_hosted_framing_and_proxies_embed_without_static_framing() -> None:
    source = (ROOT / "web" / "nginx.conf").read_text(encoding="utf-8")
    embed = _block(source, "location /embed/agents/")
    assert 'add_header X-Frame-Options "SAMEORIGIN" always;' in source
    assert "frame-ancestors 'self'" in source
    assert "proxy_pass http://$backend" in embed
    assert "X-Frame-Options" not in embed
    assert "Content-Security-Policy" not in embed
    assert 'add_header X-Content-Type-Options "nosniff" always;' in embed


def test_helm_preserves_dynamic_embed_route_to_gateway() -> None:
    config = (
        ROOT / "deploy/helm/ai-gateway/templates/frontend-configmap.yaml"
    ).read_text(encoding="utf-8")
    ingress = (ROOT / "deploy/helm/ai-gateway/templates/ingress.yaml").read_text(
        encoding="utf-8"
    )
    embed = _block(config, "location /embed/agents/")
    assert 'add_header X-Frame-Options "SAMEORIGIN" always;' in config
    assert "frame-ancestors 'self'" in config
    assert "proxy_pass http://backend" in embed
    assert "X-Frame-Options" not in embed
    assert "Content-Security-Policy" not in embed
    assert "- path: /embed/agents" in ingress
    path_block = ingress[ingress.index("- path: /embed/agents") :]
    assert "-gateway" in path_block.split("# API routes", 1)[0]


def test_gateway_security_middleware_exempts_only_dedicated_embed_document() -> None:
    source = (
        ROOT / "src" / "core" / "middleware" / "_streaming" / "security_headers.py"
    ).read_text(encoding="utf-8")
    assert '_EMBED_PREFIX = "/embed/agents/"' in source
    assert "allow_embed_frame = path.startswith(_EMBED_PREFIX)" in source
    assert 'b"x-frame-options"' in source
    assert 'b"DENY"' in source

    main_source = (ROOT / "src" / "main.py").read_text(encoding="utf-8")
    assert "SecurityHeadersMiddleware" in main_source
    assert "app.include_router(agent_embed_document_router)" in main_source


def test_header_script_has_ownership_guard_and_built_image_mode() -> None:
    path = ROOT / "scripts" / "new" / "test-agent-embed-headers.sh"
    source = path.read_text(encoding="utf-8")
    assert source.startswith("#!/bin/bash\nset -euo pipefail")
    assert 'assert_compose_owner "$PROJECT_ROOT"' in source
    assert '"--config-only"' in source
    assert '"--built-image"' in source
    assert "docker build --load" in source
    assert "docker rm -f" in source


def test_embed_assets_are_in_frontend_build_context_and_credential_free() -> None:
    for name in ("agent-embed.js", "agent-widget.js", "agent-embed.css"):
        path = ROOT / "web" / "public" / name
        assert path.is_file()
    combined = "\n".join(
        (ROOT / "web" / "public" / name).read_text(encoding="utf-8")
        for name in ("agent-embed.js", "agent-widget.js")
    )
    assert "AGENT_RUNTIME_TOKEN" not in combined
    assert "GATEWAY_ASSISTANT_SHARED_SECRET" not in combined
    assert "agt_" not in combined
