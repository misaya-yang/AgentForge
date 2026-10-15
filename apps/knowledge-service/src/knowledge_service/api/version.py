"""Knowledge API /version projection from one compatibility manifest."""

from ai_gateway_core.release_manifest import ReleaseManifestUnavailable, service_version
from fastapi import APIRouter, FastAPI
from fastapi.responses import JSONResponse


def version_router(service_id: str) -> APIRouter:
    router = APIRouter(tags=["Health"])

    @router.get("/version", operation_id=f"get_{service_id.replace('-', '_')}_release_version")
    async def version():
        try:
            return service_version(service_id)
        except ReleaseManifestUnavailable:
            return JSONResponse(
                status_code=503,
                content={
                    "schema_version": "ai-platform/service-version/v1",
                    "service_id": service_id,
                    "status": "unavailable",
                    "code": "RELEASE_MANIFEST_UNAVAILABLE",
                },
            )

    return router


def register_version_route(app: FastAPI, service_id: str) -> None:
    """Register the API release surface without an included-router marker."""

    async def version():
        try:
            return service_version(service_id)
        except ReleaseManifestUnavailable:
            return JSONResponse(
                status_code=503,
                content={
                    "schema_version": "ai-platform/service-version/v1",
                    "service_id": service_id,
                    "status": "unavailable",
                    "code": "RELEASE_MANIFEST_UNAVAILABLE",
                },
            )

    app.add_api_route(
        "/version",
        version,
        methods=["GET"],
        tags=["Health"],
        operation_id=f"get_{service_id.replace('-', '_')}_release_version",
    )
