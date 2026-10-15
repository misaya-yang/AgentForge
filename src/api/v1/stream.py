from __future__ import annotations

import logging
import time
import traceback

from ai_gateway_core.exceptions import GatewayError
from fastapi import APIRouter, Depends, HTTPException, Request
from fastapi.responses import StreamingResponse

from ...core.auth.user_resolver import UserContext
from ...core.gateway.dispatcher import GatewayDispatcher
from ..deps import get_dispatcher, get_user_context
from ..schemas.request import UnifiedRequestSchema
from ..schemas.response import StreamChunkSchema

router = APIRouter()
logger = logging.getLogger(__name__)


@router.post(
    "/stream",
    response_class=StreamingResponse,
    responses={200: {"content": {"text/event-stream": {"schema": {"type": "string"}}}}},
)
async def stream(
    body: UnifiedRequestSchema,
    request: Request,
    dispatcher: GatewayDispatcher = Depends(get_dispatcher),
    user: UserContext = Depends(get_user_context),
):
    """Stream endpoint - returns SSE stream."""
    t0 = time.perf_counter()
    timing = {}

    domain_req = body.to_domain(default_user_id=user.user_id, default_tenant_id=user.tenant_id)
    client_ip = request.client.host if request.client else None

    # Session handling before streaming starts
    session_id_header: str | None = None
    try:
        registry = getattr(request.app.state, "registry", None)
        service = await registry.get(domain_req.service_id) if registry else None

        if service and getattr(service, "session_enabled", False):
            session = await dispatcher.session_manager.get_or_create(
                domain_req.session_id,
                user_id=user.user_id,
                tenant_id=user.tenant_id,
                service_id=service.service_id,
            )
            domain_req.session_id = session.session_id
            session_id_header = session.session_id
    except GatewayError as exc:
        raise HTTPException(status_code=403, detail=str(exc))

    timing["preflight_ms"] = (time.perf_counter() - t0) * 1000
    logger.info(f"Stream preflight completed in {timing['preflight_ms']:.2f}ms")

    async def event_generator():
        """Generate SSE events with minimal latency."""
        nonlocal timing
        t_gen_start = time.perf_counter()
        chunk_count = 0
        first_chunk_time = None

        try:
            # Log when generator actually starts (important for debugging middleware buffering)
            timing["generator_start_ms"] = (t_gen_start - t0) * 1000
            logger.info(
                f"[TIMING] Generator started: {timing['generator_start_ms']:.2f}ms from request"
            )

            t_stream_start = time.perf_counter()
            async for chunk in dispatcher.stream(domain_req, roles=user.roles, client_ip=client_ip):
                t_chunk_received = time.perf_counter()
                if first_chunk_time is None:
                    first_chunk_time = t_chunk_received
                    timing["first_chunk_ms"] = (first_chunk_time - t0) * 1000
                    timing["upstream_latency_ms"] = (first_chunk_time - t_stream_start) * 1000
                    logger.info(
                        f"[TIMING] First chunk received: {timing['first_chunk_ms']:.2f}ms (upstream: {timing['upstream_latency_ms']:.2f}ms)"
                    )

                # Log event type and content for debugging streaming issues
                event_type = getattr(chunk, "event_type", "unknown")
                content_data = getattr(getattr(chunk, "content", None), "data", "")
                content_len = len(content_data) if isinstance(content_data, str) else 0
                tool_call = getattr(chunk, "tool_call", None)
                tool_info = f", tool={getattr(tool_call, 'name', '')}" if tool_call else ""

                # Log every chunk for debugging (use debug level to avoid log spam)
                if chunk_count < 10 or chunk_count % 20 == 0:
                    logger.debug(
                        f"[STREAM] #{chunk_count}: event={event_type}, content_len={content_len}{tool_info}"
                    )

                payload = StreamChunkSchema.from_domain(chunk).model_dump_json()
                t_serialized = time.perf_counter()

                yield f"data: {payload}\n\n"

                if chunk_count == 0:
                    t_yielded = time.perf_counter()
                    logger.info(
                        f"[TIMING] First chunk yielded: {(t_yielded - t0) * 1000:.2f}ms (serialize: {(t_serialized - t_chunk_received) * 1000:.2f}ms)"
                    )
                chunk_count += 1

            timing["total_chunks"] = chunk_count
            timing["stream_duration_ms"] = (time.perf_counter() - t_stream_start) * 1000
            logger.info(
                f"[STREAM COMPLETE] Stream finished: {chunk_count} chunks in {timing['stream_duration_ms']:.0f}ms"
            )

        except GatewayError as exc:
            logger.error(f"Stream gateway error: {exc}")
            yield f"event: error\ndata: {str(exc)}\n\n"
        except Exception as exc:
            logger.error(f"Stream unexpected error: {exc}\n{traceback.format_exc()}")
            yield f"event: error\ndata: {str(exc)}\n\n"

    return StreamingResponse(
        event_generator(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "Connection": "keep-alive",
            "X-Accel-Buffering": "no",
            **({"X-Session-Id": session_id_header} if session_id_header else {}),
        },
    )
