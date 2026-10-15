"""Private Responses endpoint consumed only by the Agent Runtime container."""

from __future__ import annotations

import asyncio
import hmac
import json
import logging

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse, StreamingResponse

from ...core.gateway.admission import CapacityRejected, _finish_cleanup
from ...services.agent_runtime import AgentModelPlaneError

router = APIRouter(
    prefix="/internal/v1/agent-model-plane",
    include_in_schema=False,
)
logger = logging.getLogger(__name__)


def _turn_metadata_header(request: Request) -> str:
    """Read the platform header or one unambiguous runtime-compatible alias."""
    platform_value = request.headers.get("x-agent-turn-metadata", "")
    if platform_value:
        return platform_value
    compatible_values = [
        value
        for name, value in request.headers.items()
        if name.endswith("-turn-metadata")
    ]
    return compatible_values[0] if len(compatible_values) == 1 else ""


def _error(error: AgentModelPlaneError) -> JSONResponse:
    return JSONResponse(
        status_code=error.status_code,
        content={
            "error": {
                "type": "runtime_model_plane_error",
                "code": error.code,
                "message": "The private model request was rejected.",
            }
        },
    )


class _OwnedModelStreamResponse(StreamingResponse):
    """Own a primed producer even if ASGI send fails before body iteration."""

    def __init__(self, source, first):
        self._source = source

        async def body():
            yield first
            async for chunk in source:
                yield chunk

        super().__init__(body(), media_type="text/event-stream",
                         headers={"cache-control": "no-cache", "x-accel-buffering": "no"})

    async def __call__(self, scope, receive, send):
        try:
            await super().__call__(scope, receive, send)
        finally:
            async def close():
                try:
                    await self.body_iterator.aclose()
                finally:
                    await self._source.aclose()
            await _finish_cleanup(asyncio.create_task(close()))


@router.post("/responses")
async def responses(request: Request):
    expected = str(getattr(request.app.state, "agent_model_plane_internal_token", "") or "")
    authorization = request.headers.get("authorization", "")
    supplied = authorization[7:] if authorization.startswith("Bearer ") else ""
    if not expected or not supplied or not hmac.compare_digest(supplied, expected):
        return _error(AgentModelPlaneError("RUNTIME_MODEL_PLANE_UNAUTHORIZED", status_code=401))

    try:
        body = json.loads(await request.body())
    except (json.JSONDecodeError, UnicodeDecodeError):
        return _error(AgentModelPlaneError("RUNTIME_MODEL_REQUEST_INVALID"))
    if not isinstance(body, dict) or body.get("stream") is not True:
        return _error(AgentModelPlaneError("RUNTIME_MODEL_REQUEST_INVALID"))
    metadata_raw = _turn_metadata_header(request)
    try:
        turn_metadata = json.loads(metadata_raw)
    except json.JSONDecodeError:
        return _error(AgentModelPlaneError("RUNTIME_TURN_METADATA_INVALID", status_code=401))
    if not isinstance(turn_metadata, dict):
        return _error(AgentModelPlaneError("RUNTIME_TURN_METADATA_INVALID", status_code=401))
    plane = getattr(request.app.state, "agent_model_plane", None)
    if plane is None:
        return _error(AgentModelPlaneError("RUNTIME_MODEL_PLANE_UNAVAILABLE", status_code=503))
    try:
        authorized = await plane.authorize_and_reserve(
            body=body,
            turn_metadata=turn_metadata,
        )
    except AgentModelPlaneError as error:
        logger.warning(
            "Agent model-plane request rejected code=%s status=%s",
            error.code,
            error.status_code,
        )
        return _error(error)
    source = plane.stream(body=body, turn_metadata=turn_metadata, authorized_call=authorized)
    try:
        # Admission and provider preflight must finish before the 200 headers.
        first = await anext(source)
    except CapacityRejected as exc:
        await source.aclose()
        response = _error(AgentModelPlaneError(exc.code, status_code=exc.status_code))
        response.headers.update(exc.headers)
        return response
    except AgentModelPlaneError as exc:
        await source.aclose()
        logger.warning(
            "Agent model stream preflight rejected code=%s status=%s",
            exc.code, exc.status_code,
        )
        return _error(exc)
    except StopAsyncIteration:
        await source.aclose()
        return _error(AgentModelPlaneError("RUNTIME_PROVIDER_STREAM_EMPTY", status_code=502))
    except BaseException:
        await _finish_cleanup(asyncio.create_task(source.aclose()))
        raise
    return _OwnedModelStreamResponse(source, first)



__all__ = ["router"]
