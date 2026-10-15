"""FastAPI route class that never reflects submitted values in 422 responses."""

from __future__ import annotations

import uuid
from collections.abc import Callable, Coroutine
from typing import Any

from fastapi import Request, Response
from fastapi.exceptions import RequestValidationError
from fastapi.routing import APIRoute
from starlette.responses import JSONResponse


class RedactedValidationRoute(APIRoute):
    """Remove Pydantic's `input`/`ctx` fields at credential-bearing routes."""

    def get_route_handler(
        self,
    ) -> Callable[[Request], Coroutine[Any, Any, Response]]:
        original = super().get_route_handler()

        async def redacted(request: Request) -> Response:
            try:
                return await original(request)
            except RequestValidationError as exc:
                request_id = str(
                    getattr(request.state, "request_id", "")
                    or getattr(request.state, "trace_id", "")
                    or uuid.uuid4()
                )
                request.state.request_id = request_id
                fields = [
                    {
                        "loc": [str(part) for part in error.get("loc", ())],
                        "type": str(error.get("type") or "value_error"),
                    }
                    for error in exc.errors()
                ]
                return JSONResponse(
                    status_code=422,
                    content={
                        "detail": {
                            "code": "REQUEST_VALIDATION_FAILED",
                            "message": "Request validation failed",
                            "request_id": request_id,
                            "fields": fields,
                        }
                    },
                )

        return redacted


__all__ = ["RedactedValidationRoute"]
