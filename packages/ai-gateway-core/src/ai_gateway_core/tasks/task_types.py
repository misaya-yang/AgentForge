from __future__ import annotations

import logging
from typing import Any, Protocol

from ai_gateway_core.auth._core import UserContext


class FileProcessorProtocol(Protocol):
    async def preprocess_file(
        self,
        *,
        file_path: str,
        user: UserContext,
        model_supports_vision: bool,
    ) -> Any: ...

logger = logging.getLogger(__name__)


async def process_file_task(
    payload: dict[str, Any], file_processor: FileProcessorProtocol
) -> None:
    """
    Background task to process an uploaded file.

    Payload:
    - file_path: str
    - user_id: str
    - tenant_id: str
    """
    file_path = payload.get("file_path")
    user_id = payload.get("user_id")
    tenant_id = payload.get("tenant_id")

    if not file_path or not user_id:
        logger.error("Missing required fields in process_file_task payload")
        return

    # Construct minimal user context
    user = UserContext(
        user_id=user_id,
        tenant_id=tenant_id or "default",
        roles=[],  # Roles not needed for file processing
    )

    try:
        # Preprocess file (PDF conversion, etc.) and cache result
        # By default we enable vision support to pre-generate images
        await file_processor.preprocess_file(
            file_path=file_path, user=user, model_supports_vision=True
        )
        logger.info(f"Async processing completed for {file_path}")
    except Exception as e:
        logger.error(f"Async processing failed for {file_path}: {e}")
