"""Gateway /version projection from the unified compatibility manifest."""

from ai_gateway_core.release_manifest import ReleaseManifestUnavailable, service_version
from fastapi import APIRouter
from fastapi.responses import JSONResponse

router = APIRouter(tags=["Health"])


@router.get("/version", operation_id="get_gateway_release_version")
async def version():
    try:
        return service_version("gateway")
    except ReleaseManifestUnavailable:
        return JSONResponse(
            status_code=503,
            content={
                "schema_version": "ai-platform/service-version/v1",
                "service_id": "gateway",
                "status": "unavailable",
                "code": "RELEASE_MANIFEST_UNAVAILABLE",
            },
        )
