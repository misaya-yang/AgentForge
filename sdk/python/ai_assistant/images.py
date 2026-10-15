"""
Image generation module.

Wraps the gateway's smart-routing image generation endpoint which
automatically routes to the appropriate provider (DashScope Wanx,
Gemini native, etc.) based on the selected model.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from ai_assistant.exceptions import AssistantError, UnknownOutcomeError
from ai_assistant.models.request import ImageGenerationRequest
from ai_assistant.models.response import ImageResult

if TYPE_CHECKING:
    from ai_assistant.transport.http import HTTPTransport


class ImageModule:
    """High-level image generation operations."""

    _GENERATE_PATH = "/api/v1/assistant/generate-image"
    _ASYNC_PATH = "/api/v1/assistant/generate-image-async"
    _TASK_PATH = "/api/v1/assistant/image-task/{task_id}"

    def __init__(self, transport: HTTPTransport) -> None:
        self._transport = transport

    async def generate(
        self,
        prompt: str,
        *,
        model_id: str | None = None,
        style: str | None = None,
        size: str | None = None,
        n: int = 1,
    ) -> ImageResult:
        """Generate images synchronously.

        Blocks until the server returns the generated image URLs.

        Args:
            prompt: Text description of the desired image.
            model_id: Model identifier (determines provider routing).
            style: Optional style preset.
            size: Optional size string, e.g. ``"1024x1024"``.
            n: Number of images to generate.

        Returns:
            ``ImageResult`` containing the image URLs and metadata.
        """
        req = ImageGenerationRequest(
            prompt=prompt,
            model_id=model_id,
            style=style,
            size=size,
            n=n,
        )
        resp = await self._transport.request("POST", self._GENERATE_PATH, json=req.to_dict())
        return ImageResult.from_dict(resp.json())

    async def generate_async(
        self,
        prompt: str,
        *,
        model_id: str | None = None,
        style: str | None = None,
        size: str | None = None,
        n: int = 1,
    ) -> dict[str, Any]:
        """Submit an image generation request asynchronously.

        Requires an explicit registered model ID. Returns a task receipt that
        can be read with ``get_task()``. Sends once; uncertain submission raises
        ``UnknownOutcomeError`` and must not be blindly repeated.

        Returns:
            Dict with ``task_id`` and initial status.
        """
        if not isinstance(model_id, str) or not model_id.strip():
            raise ValueError("Async image generation requires an explicit model_id")
        req = ImageGenerationRequest(
            prompt=prompt,
            model_id=model_id,
            style=style,
            size=size,
            n=n,
        )
        try:
            resp = await self._transport.request("POST", self._ASYNC_PATH, json=req.to_dict())
        except AssistantError as exc:
            status = exc.status_code
            if status is not None and 400 <= status < 500 and status != 408:
                raise
            raise UnknownOutcomeError(
                "Image submission outcome unknown; do not automatically resubmit",
                status_code=status, request_id=exc.request_id,
            ) from exc
        try:
            return self._task_response(resp.json())
        except ValueError as exc:
            raise UnknownOutcomeError(
                "Image submission outcome unknown; no trustworthy task receipt received"
            ) from exc

    async def get_task(self, task_id: str) -> dict[str, Any]:
        """Poll the status of an async image generation task.

        Returns:
            Dict with current task status and, if complete, image URLs.
        """
        self._validate_task_id(task_id)
        path = self._TASK_PATH.format(task_id=task_id)
        resp = await self._transport.request("GET", path)
        try:
            value = self._task_response(resp.json())
            if value["task_id"] != task_id:
                raise ValueError("Mismatched image task identity")
            return value
        except ValueError as exc:
            raise AssistantError("Invalid image task response") from exc

    async def cancel(self, task_id: str, *, reason: str | None = None) -> dict[str, Any]:
        """Image-task cancellation is not supported by the Gateway contract.

        Raises ``NotImplementedError`` locally without sending a request. Image
        tasks are separate from managed Agent runs and cannot use their cancel API.
        """
        raise NotImplementedError("Image task cancellation is not supported by the Gateway")

    @staticmethod
    def _validate_task_id(task_id: str) -> None:
        if (
            not isinstance(task_id, str) or not task_id or len(task_id) > 160
            or task_id in {".", ".."}
            or any(char.isspace() or ord(char) < 32 for char in task_id)
            or any(char in task_id for char in "/\\?#%")
        ):
            raise ValueError("Invalid image task ID")

    @staticmethod
    def _task_response(value: Any) -> dict[str, Any]:
        if not isinstance(value, dict):
            raise ValueError("Invalid image task response")
        ImageModule._validate_task_id(value.get("task_id"))
        if not isinstance(value.get("status"), str) or not value["status"].strip():
            raise ValueError("Invalid image task status")
        # Keep producer statuses and fields; unknown never means completed.
        return value
