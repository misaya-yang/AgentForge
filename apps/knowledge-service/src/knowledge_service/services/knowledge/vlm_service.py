"""
VLM (Vision Language Model) Service for Image Description Generation.

Uses DashScope's multimodal models (qwen-vl-max, qwen2-vl-72b-instruct) to generate
textual descriptions of images for enhanced RAG retrieval.

This service complements multimodal embeddings by generating searchable text
descriptions that can be indexed alongside the original image vectors.
"""

from __future__ import annotations

import asyncio
import base64
import logging
from dataclasses import dataclass
from typing import Any

logger = logging.getLogger(__name__)


class VLMError(RuntimeError):
    """VLM service error."""

    pass


@dataclass
class ImageDescription:
    """Result of image description generation."""

    description: str
    model: str
    tokens_used: int = 0
    metadata: dict[str, Any] | None = None


class DashScopeVLMService:
    """
    DashScope VLM service for generating image descriptions.

    Uses qwen-vl-max or qwen2-vl-72b-instruct to analyze images and generate
    detailed textual descriptions suitable for RAG indexing.

    Supported models:
    - qwen-vl-max: Best quality, recommended for production
    - qwen-vl-plus: Balanced quality and speed
    - qwen2-vl-72b-instruct: Latest Qwen2 VL model
    """

    # Model configurations
    MODEL_INFO: dict[str, dict[str, Any]] = {
        "qwen-vl-max": {"max_tokens": 2000, "supports_url": True},
        "qwen-vl-plus": {"max_tokens": 2000, "supports_url": True},
        "qwen2-vl-72b-instruct": {"max_tokens": 4096, "supports_url": True},
        "qwen2-vl-7b-instruct": {"max_tokens": 4096, "supports_url": True},
    }

    # Default prompt for image description
    DEFAULT_PROMPT = """请详细描述这张图片的内容，包括：
1. 图片的主要内容和主题
2. 图片中包含的文字（如果有）
3. 数据、表格或图表的具体信息（如果有）
4. 其他重要的视觉元素

请用清晰、结构化的方式描述，方便后续搜索和理解。"""

    # Prompt for table/chart images
    TABLE_PROMPT = """这是一张包含表格或图表的图片。请详细提取其中的信息：
1. 表格/图表的标题或主题
2. 列名和行名
3. 具体的数据值
4. 单位和注释
5. 关键发现或趋势

请以结构化的方式输出，保留原始数据的完整性。"""

    def __init__(
        self,
        api_key: str,
        model: str = "qwen-vl-max",
        base_url: str | None = None,
        timeout_seconds: float = 60.0,
        max_retries: int = 3,
    ):
        """
        Initialize VLM service.

        Args:
            api_key: DashScope API key
            model: Model name (qwen-vl-max, qwen-vl-plus, etc.)
            base_url: Optional base URL override
            timeout_seconds: Request timeout
            max_retries: Max retry attempts
        """
        if not api_key:
            raise VLMError("DashScope API key is required for VLM service")

        self.api_key = api_key
        self.model = model
        self.base_url = base_url
        self.timeout_seconds = timeout_seconds
        self.max_retries = max_retries

        # Validate model
        if model not in self.MODEL_INFO:
            logger.warning(f"Unknown VLM model: {model}, using default config")

        # Initialize DashScope
        try:
            import dashscope
            from dashscope import MultiModalConversation

            self._MultiModalConversation = MultiModalConversation
            if base_url:
                dashscope.base_http_api_url = base_url
        except ImportError as exc:
            raise VLMError(
                "dashscope package is required for VLM service (pip install dashscope>=1.24.6)"
            ) from exc

        logger.info(f"VLM service initialized with model: {model}")

    def _image_to_base64_data_uri(self, image_bytes: bytes, media_type: str = "image/png") -> str:
        """Convert image bytes to base64 data URI."""
        b64_data = base64.b64encode(image_bytes).decode("utf-8")
        return f"data:{media_type};base64,{b64_data}"

    def _detect_media_type(self, image_bytes: bytes) -> str:
        """Detect image media type from magic bytes."""
        if len(image_bytes) < 8:
            return "image/png"

        if image_bytes[:8] == b"\x89PNG\r\n\x1a\n":
            return "image/png"
        elif image_bytes[:2] == b"\xff\xd8":
            return "image/jpeg"
        elif image_bytes[:6] in (b"GIF87a", b"GIF89a"):
            return "image/gif"
        elif image_bytes[:2] == b"BM":
            return "image/bmp"
        elif image_bytes[:4] == b"RIFF" and len(image_bytes) > 12 and image_bytes[8:12] == b"WEBP":
            return "image/webp"
        else:
            return "image/png"

    async def describe_image(
        self,
        image_bytes: bytes,
        prompt: str | None = None,
        image_type: str = "general",
        context: str | None = None,
        max_tokens: int = 1500,
    ) -> ImageDescription:
        """
        Generate a textual description of an image.

        Args:
            image_bytes: Image content as bytes
            prompt: Custom prompt (uses default if not provided)
            image_type: Type hint - "general", "table", "chart", "document"
            context: Optional context about the image (e.g., page title)
            max_tokens: Maximum tokens in response

        Returns:
            ImageDescription with generated text
        """
        # Select appropriate prompt
        if prompt:
            final_prompt = prompt
        elif image_type in ("table", "chart"):
            final_prompt = self.TABLE_PROMPT
        else:
            final_prompt = self.DEFAULT_PROMPT

        # Add context if provided
        if context:
            final_prompt = f"图片来源上下文：{context}\n\n{final_prompt}"

        # Convert image to data URI
        media_type = self._detect_media_type(image_bytes)
        image_uri = self._image_to_base64_data_uri(image_bytes, media_type)

        # Build messages
        messages = [
            {
                "role": "user",
                "content": [
                    {"image": image_uri},
                    {"text": final_prompt},
                ],
            }
        ]

        # Call API with retries
        last_error = None
        for attempt in range(self.max_retries):
            try:
                response = await asyncio.to_thread(
                    self._MultiModalConversation.call,
                    model=self.model,
                    messages=messages,
                    api_key=self.api_key,
                    max_tokens=max_tokens,
                )

                # Check response status
                status_code = getattr(response, "status_code", None)
                if status_code and int(status_code) >= 400:
                    code = getattr(response, "code", "") or ""
                    message = getattr(response, "message", "") or ""
                    raise VLMError(f"VLM API error: {status_code} {code} {message}")

                # Extract description from response
                output = getattr(response, "output", None)
                if not output:
                    raise VLMError("VLM response missing output")

                # Parse response format
                choices = output.get("choices", [])
                if not choices:
                    raise VLMError("VLM response has no choices")

                message_content = choices[0].get("message", {}).get("content", [])
                description_text = ""
                for item in message_content:
                    if isinstance(item, dict) and "text" in item:
                        description_text += item["text"]
                    elif isinstance(item, str):
                        description_text += item

                if not description_text:
                    raise VLMError("VLM response has no text content")

                # Get token usage
                usage = getattr(response, "usage", {}) or {}
                tokens_used = usage.get("total_tokens", 0)

                return ImageDescription(
                    description=description_text.strip(),
                    model=self.model,
                    tokens_used=tokens_used,
                    metadata={
                        "image_type": image_type,
                        "media_type": media_type,
                        "image_size": len(image_bytes),
                    },
                )

            except VLMError:
                raise
            except Exception as e:
                last_error = e
                if attempt < self.max_retries - 1:
                    wait_time = (attempt + 1) * 2  # Exponential backoff
                    logger.warning(
                        f"VLM API call failed (attempt {attempt + 1}), "
                        f"retrying in {wait_time}s: {e}"
                    )
                    await asyncio.sleep(wait_time)
                else:
                    break

        raise VLMError(f"VLM API call failed after {self.max_retries} attempts: {last_error}")

    async def describe_images_batch(
        self,
        images: list[bytes],
        prompts: list[str] | None = None,
        image_types: list[str] | None = None,
        contexts: list[str] | None = None,
        max_concurrent: int = 8,  # Increased from 3 for better throughput
    ) -> list[ImageDescription]:
        """
        Generate descriptions for multiple images.

        Args:
            images: List of image bytes
            prompts: Optional list of custom prompts
            image_types: Optional list of image type hints
            contexts: Optional list of context strings
            max_concurrent: Maximum concurrent API calls (default: 8)

        Returns:
            List of ImageDescription results
        """
        if not images:
            return []

        # Normalize inputs
        prompts = prompts or [None] * len(images)
        image_types = image_types or ["general"] * len(images)
        contexts = contexts or [None] * len(images)

        # Semaphore for concurrency control
        semaphore = asyncio.Semaphore(max_concurrent)

        async def describe_with_semaphore(
            idx: int,
            image: bytes,
            prompt: str | None,
            image_type: str,
            context: str | None,
        ) -> tuple[int, ImageDescription | Exception]:
            async with semaphore:
                try:
                    result = await self.describe_image(
                        image_bytes=image,
                        prompt=prompt,
                        image_type=image_type,
                        context=context,
                    )
                    return idx, result
                except Exception as e:
                    logger.error(f"Failed to describe image {idx}: {e}")
                    return idx, e

        # Create tasks
        tasks = [
            describe_with_semaphore(i, img, prompts[i], image_types[i], contexts[i])
            for i, img in enumerate(images)
        ]

        # Execute and collect results
        results_raw = await asyncio.gather(*tasks)

        # Sort by index and extract results
        results_sorted = sorted(results_raw, key=lambda x: x[0])
        results = []
        for _idx, result in results_sorted:
            if isinstance(result, Exception):
                # Return a placeholder description for failed images
                results.append(
                    ImageDescription(
                        description=f"[图片描述生成失败: {result}]",
                        model=self.model,
                        metadata={"error": str(result)},
                    )
                )
            else:
                results.append(result)

        return results

    async def describe_table_image(
        self,
        image_bytes: bytes,
        table_context: str | None = None,
    ) -> ImageDescription:
        """
        Specialized method for describing table/chart images.

        Extracts structured data from tables and charts for better RAG retrieval.

        Args:
            image_bytes: Image content
            table_context: Optional context (e.g., document title, section)

        Returns:
            ImageDescription with structured table data
        """
        return await self.describe_image(
            image_bytes=image_bytes,
            image_type="table",
            context=table_context,
            max_tokens=2000,  # Tables need more tokens
        )


def create_vlm_service(
    api_key: str,
    model: str = "qwen-vl-max",
    base_url: str | None = None,
) -> DashScopeVLMService:
    """
    Factory function to create VLM service.

    Args:
        api_key: DashScope API key
        model: Model name
        base_url: Optional base URL

    Returns:
        DashScopeVLMService instance
    """
    return DashScopeVLMService(
        api_key=api_key,
        model=model,
        base_url=base_url,
    )
