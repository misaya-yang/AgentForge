"""Actual Python route metadata, with no lifespan/provider or credential access."""
import json
import os
from pathlib import Path

from fastapi import FastAPI

from src.api.v1.connectors import router


def test_callback_metadata_preserves_denial_queries_and_real_redirect():
    app = FastAPI()
    app.include_router(router, prefix="/api/v1")
    operation = app.openapi()["paths"]["/api/v1/connectors/callback/{provider}"]["get"]
    parameters = {p["name"]: p for p in operation["parameters"]}
    assert parameters["provider"]["required"] is True
    assert parameters["code"]["required"] is False
    assert parameters["error"]["required"] is False
    assert parameters["state"]["schema"]["default"] == ""
    assert set(operation["responses"]) == {"302", "422"}
    assert "content" not in operation["responses"]["302"]
    assert operation["responses"]["302"]["headers"]["Location"] == {
        "required": True, "schema": {"type": "string"}}
    # The initial source snapshot still contains the previous publication. The
    # generated artifact is tested explicitly after the actual application export.
    artifact = os.environ.get("CONNECTOR_PUBLISHED_CANDIDATE")
    if artifact:
        published = json.loads(Path(artifact).read_text())
        assert published["paths"]["/api/v1/connectors/callback/{provider}"]["get"] == operation
