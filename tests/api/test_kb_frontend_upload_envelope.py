from pathlib import Path

from src.api.v1 import _proxy_utils as proxy_utils

ROOT = Path(__file__).resolve().parents[2]


def test_frontend_nginx_allows_the_gateway_upload_envelope() -> None:
    nginx_config = (ROOT / "web/nginx.conf").read_text(encoding="utf-8")
    api_location = nginx_config.split("location /api/ {", 1)[1].split(
        "# V1 API compatibility", 1
    )[0]

    assert "client_max_body_size 50m;" in api_location
    assert (
        proxy_utils._MAX_FILE_BODY_MB * 1024 * 1024 + proxy_utils._MULTIPART_OVERHEAD_BYTES
        < 50 * 1024 * 1024
    )
