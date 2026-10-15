"""Provider-isolated image generation for the Gateway capability worker.

This module deliberately does not route or retry across providers.  A run is
bound to one configured provider, so a failed request cannot silently create a
second billable request.  Provider responses must contain inline base64 data;
URL-only responses are rejected rather than fetched by the Gateway.  That
keeps provider credentials away from untrusted CDNs and avoids an SSRF surface.
"""

from __future__ import annotations

import asyncio
import base64
import binascii
import os
import re
import time
from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import Any
from urllib.parse import urlparse

import httpx

from ai_gateway_core.config import resolve_dashscope, resolve_google
from ai_gateway_core.security.safe_fetch import SafeFetchError, safe_fetch_with_response

_MAX_IMAGES = 4
_MAX_IMAGE_BYTES = 20 * 1024 * 1024
_DEFAULT_TIMEOUT = 120.0
_SUPPORTED_MIME = {"image/png", "image/jpeg", "image/jpg", "image/webp", "image/gif"}
_MODEL_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,254}$")
_HOST_SUFFIX = re.compile(r"^[A-Za-z0-9][A-Za-z0-9.-]{0,252}[A-Za-z0-9]$")
_DEFAULT_RESULT_HOST_SUFFIXES = ("aliyuncs.com", "alibabacloud.com", "volces.com")


@dataclass(frozen=True)
class ImageGenerationConfig:
    """Immutable provider configuration resolved by Gateway startup."""

    provider: str
    api_key: str
    base_url: str
    model: str
    timeout: float = _DEFAULT_TIMEOUT
    dashscope_submit_path: str = "/services/aigc/image-generation/generation"
    dashscope_protocol: str = "wan26"
    google_backend: str = "ai_studio"
    result_host_suffixes: tuple[str, ...] = _DEFAULT_RESULT_HOST_SUFFIXES
    supports_reference_images: bool = False

    @classmethod
    def from_environment(cls) -> ImageGenerationConfig:
        provider = os.getenv("IMAGE_GENERATION_PROVIDER", "").strip().lower()
        google_key, google_base, google_backend = resolve_google("image")
        dashscope_key, dashscope_base = resolve_dashscope("image")
        ark_key = os.getenv("ARK_API_KEY", "").strip()
        configured_suffixes = tuple(
            part.strip().lower()
            for part in os.getenv("IMAGE_GENERATION_RESULT_HOST_SUFFIXES", "").split(",")
            if part.strip()
        )
        result_host_suffixes = configured_suffixes or _DEFAULT_RESULT_HOST_SUFFIXES
        if not provider:
            if google_key:
                provider = "google"
            elif dashscope_key:
                provider = "dashscope"
            elif ark_key:
                provider = "doubao"
        if provider in {"google", "gemini", "ai_studio", "vertex"}:
            return cls(
                provider="google",
                api_key=google_key,
                base_url=google_base,
                model=os.getenv("GOOGLE_IMAGE_MODEL", "gemini-3.1-flash-image-preview"),
                google_backend=google_backend,
                result_host_suffixes=result_host_suffixes,
                supports_reference_images=os.getenv(
                    "IMAGE_GENERATION_SUPPORTS_REFERENCE_IMAGES", "true"
                ).lower()
                in {"1", "true", "yes"},
            )
        if provider in {"dashscope", "qwen"}:
            return cls(
                provider="dashscope",
                api_key=dashscope_key,
                base_url=dashscope_base,
                model=os.getenv("DASHSCOPE_IMAGE_MODEL", "wan2.6-t2i"),
                dashscope_submit_path=os.getenv(
                    "DASHSCOPE_IMAGE_SUBMIT_PATH",
                    "/services/aigc/image-generation/generation",
                ),
                dashscope_protocol=os.getenv("DASHSCOPE_IMAGE_PROTOCOL", "wan26").strip().lower(),
                result_host_suffixes=result_host_suffixes,
            )
        if provider in {"doubao", "volcengine", "ark"}:
            return cls(
                provider="doubao",
                api_key=ark_key,
                base_url=os.getenv(
                    "ARK_BASE_URL", "https://ark.cn-beijing.volces.com/api/v3"
                ).strip(),
                model=os.getenv("DOUBAO_IMAGE_MODEL", "doubao-seedream-5-0-260128"),
                result_host_suffixes=result_host_suffixes,
            )
        return cls(provider=provider or "none", api_key="", base_url="", model="")


@dataclass
class ImageGenerationResult:
    """Stable capability result; provider secrets never appear in this object."""

    success: bool
    provider: str
    images: list[dict[str, Any]] = field(default_factory=list)
    error: str | None = None
    error_code: str | None = None
    blocked: bool = False
    outcome_unknown: bool = False

    def as_dict(self) -> dict[str, Any]:
        return {
            "success": self.success,
            "provider": self.provider,
            "images": self.images,
            "error": self.error,
            "error_code": self.error_code,
            "blocked": self.blocked,
            "outcome_unknown": self.outcome_unknown,
        }


class ImageGenerationProvider:
    """One configured image provider with a reusable async HTTP client."""

    def __init__(
        self,
        config: ImageGenerationConfig | None = None,
        *,
        client: httpx.AsyncClient | None = None,
    ) -> None:
        self.config = config or ImageGenerationConfig.from_environment()
        self._client = client
        self._owns_client = client is None

    async def _http(self) -> httpx.AsyncClient:
        if self._client is None:
            self._client = httpx.AsyncClient(timeout=self.config.timeout)
        return self._client

    async def close(self) -> None:
        if self._client is not None and self._owns_client:
            await self._client.aclose()
            self._client = None

    async def generate(
        self,
        *,
        prompt: str,
        n: int = 1,
        size: str = "1536*1536",
        style: str = "<auto>",
        negative_prompt: str = "",
        reference_image: bytes | None = None,
        reference_mime: str | None = None,
    ) -> ImageGenerationResult:
        if not prompt.strip():
            return self._failure("invalid_request", "prompt is required")
        if not 1 <= n <= _MAX_IMAGES:
            return self._failure("invalid_request", "n is out of range")
        if self.config.provider not in {"google", "dashscope", "doubao"}:
            return self._failure("provider_unconfigured", "image provider is not configured")
        if not self.config.api_key:
            return self._failure("provider_unconfigured", "image provider is not configured")
        if reference_image is not None:
            if self.config.provider != "google" or not self.config.supports_reference_images:
                return self._failure(
                    "reference_unsupported",
                    "configured image provider does not support reference images",
                )
            if (
                not reference_mime
                or reference_mime not in _SUPPORTED_MIME
                or len(reference_image) > _MAX_IMAGE_BYTES
                or not validate_image_bytes(reference_image, reference_mime)
            ):
                return self._failure("invalid_reference", "reference image is invalid")
        if (
            not self._is_safe_endpoint(self.config.base_url)
            or not _MODEL_ID.fullmatch(self.config.model)
            or not 1.0 <= self.config.timeout <= 300.0
            or self.config.google_backend not in {"ai_studio", "vertex"}
            or not self._valid_submit_path(self.config.dashscope_submit_path)
            or self.config.dashscope_protocol not in {"wan26", "legacy"}
            or not self.config.result_host_suffixes
            or any(
                not _HOST_SUFFIX.fullmatch(suffix) for suffix in self.config.result_host_suffixes
            )
        ):
            return self._failure("provider_endpoint_invalid", "image provider endpoint is invalid")

        try:
            if self.config.provider == "google":
                return await self._generate_google(prompt, n, size, reference_image, reference_mime)
            if self.config.provider == "dashscope":
                return await self._generate_dashscope(prompt, n, size, style, negative_prompt)
            return await self._generate_doubao(prompt, n, size)
        except (httpx.HTTPError, ValueError, KeyError, TypeError, binascii.Error):
            # Do not include exception text: provider errors can contain URLs,
            # request bodies, or other sensitive upstream details.
            return self._failure(
                "provider_request_failed",
                "image provider request failed",
                outcome_unknown=True,
            )

    async def _generate_google(
        self,
        prompt: str,
        n: int,
        size: str,
        reference_image: bytes | None = None,
        reference_mime: str | None = None,
    ) -> ImageGenerationResult:
        if self.config.google_backend == "vertex":
            endpoint = f"{self.config.base_url.rstrip('/')}/v1/publishers/google/models/{self.config.model}:generateContent"
        else:
            endpoint = f"{self.config.base_url.rstrip('/')}/v1beta/models/{self.config.model}:generateContent"
        response = await (await self._http()).post(
            endpoint,
            headers={"Content-Type": "application/json", "x-goog-api-key": self.config.api_key},
            json={
                "contents": [
                    {
                        "role": "user",
                        "parts": (
                            [
                                {
                                    "inlineData": {
                                        "mimeType": reference_mime,
                                        "data": base64.b64encode(reference_image).decode("ascii"),
                                    }
                                }
                            ]
                            if reference_image is not None
                            else []
                        )
                        + [{"text": prompt}],
                    }
                ],
                "generationConfig": {
                    "responseModalities": ["TEXT", "IMAGE"],
                    "candidateCount": n,
                    "imageConfig": {"aspectRatio": _aspect_ratio(size)},
                },
            },
        )
        if response.status_code != 200:
            return self._http_failure(response.status_code)
        payload = response.json()
        feedback = payload.get("promptFeedback") or {}
        if feedback.get("blockReason"):
            return self._failure("content_blocked", "image generation was blocked", blocked=True)
        return self._result_from_inline_parts("google", _google_parts(payload))

    async def _generate_doubao(self, prompt: str, n: int, size: str) -> ImageGenerationResult:
        response = await (await self._http()).post(
            f"{self.config.base_url.rstrip('/')}/images/generations",
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {self.config.api_key}",
            },
            json={
                "model": self.config.model,
                "prompt": prompt,
                "n": n,
                "size": _doubao_size(size),
                "response_format": "b64_json",
            },
        )
        if response.status_code != 200:
            return self._http_failure(response.status_code)
        items = response.json().get("data", [])
        if any(
            isinstance(item, Mapping) and item.get("url")
            for item in items
            if isinstance(item, Mapping)
        ):
            return await self._result_from_urls(
                "doubao",
                [item["url"] for item in items if isinstance(item, Mapping) and item.get("url")],
            )
        return self._result_from_inline_parts(
            "doubao",
            [item.get("b64_json") for item in items if isinstance(item, Mapping)],
        )

    async def _generate_dashscope(
        self, prompt: str, n: int, size: str, style: str, negative: str
    ) -> ImageGenerationResult:
        response = await (await self._http()).post(
            f"{self.config.base_url.rstrip('/')}/{self.config.dashscope_submit_path.lstrip('/')}",
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {self.config.api_key}",
                "X-DashScope-Async": "enable",
            },
            json=self._dashscope_payload(prompt, n, size, style, negative),
        )
        if response.status_code != 200:
            return self._http_failure(response.status_code)
        output = response.json().get("output", {})
        inline = output.get("images") or output.get("results") or _dashscope_choice_images(output)
        if isinstance(inline, list) and inline:
            values = [
                item.get("b64_json") or item.get("content_base64")
                for item in inline
                if isinstance(item, Mapping)
            ]
            if any(values):
                return self._result_from_inline_parts("dashscope", values)
            if any(isinstance(item, Mapping) and item.get("url") for item in inline):
                return await self._result_from_urls(
                    "dashscope",
                    [
                        item["url"]
                        for item in inline
                        if isinstance(item, Mapping) and item.get("url")
                    ],
                )
        task_id = output.get("task_id")
        if not isinstance(task_id, str) or not task_id:
            return self._failure("invalid_provider_response", "image provider returned no task")
        client = await self._http()
        deadline = time.monotonic() + self.config.timeout
        while time.monotonic() < deadline:
            task_response = await client.get(
                f"{self.config.base_url.rstrip('/')}/tasks/{task_id}",
                headers={"Authorization": f"Bearer {self.config.api_key}"},
            )
            if task_response.status_code != 200:
                return self._http_failure(task_response.status_code)
            task_output = task_response.json().get("output", {})
            status = task_output.get("task_status")
            if status == "SUCCEEDED":
                results = task_output.get("results") or task_output.get("images") or _dashscope_choice_images(task_output)
                values = [
                    item.get("b64_json") or item.get("content_base64")
                    for item in results
                    if isinstance(item, Mapping)
                ]
                if any(values):
                    return self._result_from_inline_parts("dashscope", values)
                return await self._result_from_urls(
                    "dashscope",
                    [
                        item["url"]
                        for item in results
                        if isinstance(item, Mapping) and item.get("url")
                    ],
                )
            if status == "FAILED":
                return self._failure(
                    "provider_generation_failed", "image provider generation failed"
                )
            if status not in {"PENDING", "RUNNING"}:
                return self._failure(
                    "invalid_provider_response", "image provider returned an invalid task status"
                )
            await asyncio.sleep(min(2.0, max(0.0, deadline - time.monotonic())))
        return self._failure("provider_timeout", "image provider timed out", outcome_unknown=True)

    def _dashscope_payload(
        self, prompt: str, n: int, size: str, style: str, negative: str
    ) -> dict[str, Any]:
        if self.config.dashscope_protocol == "wan26":
            return {
                "model": self.config.model,
                "input": {"messages": [{"role": "user", "content": [{"text": prompt}]}]},
                "parameters": {
                    "negative_prompt": negative,
                    "size": "1280*1280" if size == "1536*1536" else size,
                    "n": n,
                    "prompt_extend": True,
                    "watermark": False,
                },
            }
        request: dict[str, Any] = {
            "model": self.config.model,
            "input": {"prompt": prompt},
            "parameters": {"size": size, "style": style, "n": n},
        }
        if negative:
            request["input"]["negative_prompt"] = negative
        return request

    def _result_from_inline_parts(self, provider: str, values: list[Any]) -> ImageGenerationResult:
        images: list[dict[str, Any]] = []
        for index, value in enumerate(values[:_MAX_IMAGES], 1):
            mime_type = "image/png"
            if isinstance(value, Mapping):
                mime_type = str(value.get("mime_type") or mime_type).lower()
                value = value.get("data")
            if not isinstance(value, str) or not value or mime_type not in _SUPPORTED_MIME:
                continue
            raw = base64.b64decode(value, validate=True)
            if not raw or len(raw) > _MAX_IMAGE_BYTES or not validate_image_bytes(raw, mime_type):
                return self._failure(
                    "image_too_large",
                    "provider returned an invalid image",
                    outcome_unknown=True,
                )
            extension = "jpg" if mime_type in {"image/jpeg", "image/jpg"} else mime_type[6:]
            images.append(
                {
                    "filename": f"generated_{index}.{extension}",
                    "content_base64": value,
                    "mime_type": mime_type,
                    "size_bytes": len(raw),
                }
            )
        if not images:
            return self._failure(
                "no_image", "provider returned no inline images", outcome_unknown=True
            )
        return ImageGenerationResult(True, provider, images)

    async def _result_from_urls(self, provider: str, urls: list[Any]) -> ImageGenerationResult:
        images: list[dict[str, Any]] = []
        for index, url in enumerate(urls[:_MAX_IMAGES], 1):
            if not isinstance(url, str) or not url:
                continue
            try:
                response = await safe_fetch_with_response(
                    url,
                    max_bytes=_MAX_IMAGE_BYTES,
                    max_redirects=1,
                    timeout=min(self.config.timeout, 60.0),
                    allowed_hosts=self.config.result_host_suffixes,
                )
            except SafeFetchError:
                return self._failure(
                    "url_fetch_failed",
                    "provider image download failed",
                    outcome_unknown=True,
                )
            mime_type = response.content_type.partition(";")[0].strip().lower()
            if mime_type not in _SUPPORTED_MIME or not validate_image_bytes(
                response.body, mime_type
            ):
                return self._failure(
                    "invalid_provider_image",
                    "provider returned an invalid image",
                    outcome_unknown=True,
                )
            extension = "jpg" if mime_type in {"image/jpeg", "image/jpg"} else mime_type[6:]
            images.append(
                {
                    "filename": f"generated_{index}.{extension}",
                    "content_base64": base64.b64encode(response.body).decode("ascii"),
                    "mime_type": mime_type,
                    "size_bytes": len(response.body),
                }
            )
        if not images:
            return self._failure("no_image", "provider returned no images", outcome_unknown=True)
        return ImageGenerationResult(True, provider, images)

    @staticmethod
    def _is_safe_endpoint(base_url: str) -> bool:
        parsed = urlparse(base_url)
        return (
            parsed.scheme == "https"
            and bool(parsed.hostname)
            and not parsed.username
            and not parsed.password
            and not parsed.query
            and not parsed.fragment
        )

    @staticmethod
    def _valid_submit_path(path: str) -> bool:
        return (
            path.startswith("/services/")
            and ".." not in path
            and "?" not in path
            and "#" not in path
        )

    def _http_failure(self, status_code: int) -> ImageGenerationResult:
        return self._failure(
            "provider_http_error",
            "image provider request failed",
            outcome_unknown=status_code >= 500,
        )

    def _failure(
        self,
        code: str,
        message: str,
        *,
        blocked: bool = False,
        outcome_unknown: bool = False,
    ) -> ImageGenerationResult:
        return ImageGenerationResult(
            False,
            self.config.provider,
            [],
            message,
            code,
            blocked,
            outcome_unknown,
        )


def _dashscope_choice_images(output: Mapping[str, Any]) -> list[dict[str, str]]:
    """Wan2.6 returns image URLs in choices, for sync and completed tasks."""
    images: list[dict[str, str]] = []
    choices = output.get("choices")
    for choice in choices if isinstance(choices, list) else []:
        message = choice.get("message") if isinstance(choice, Mapping) else None
        content = message.get("content") if isinstance(message, Mapping) else None
        for part in content if isinstance(content, list) else []:
            url = part.get("image") if isinstance(part, Mapping) else None
            if isinstance(url, str) and url:
                images.append({"url": url})
    return images[:_MAX_IMAGES]


def _aspect_ratio(size: str) -> str:
    try:
        width, height = (float(part) for part in size.split("*", 1))
        ratio = width / height
    except (ValueError, ZeroDivisionError):
        return "1:1"
    options = {"1:1": 1.0, "16:9": 16 / 9, "9:16": 9 / 16, "4:3": 4 / 3, "3:4": 3 / 4}
    return min(options, key=lambda item: abs(options[item] - ratio))


def _doubao_size(size: str) -> str:
    return {
        "1536*1536": "2048x2048",
        "1024*1024": "2048x2048",
        "720*1280": "1440x2560",
        "1280*720": "2560x1440",
    }.get(size, "2048x2048")


def validate_image_bytes(content: bytes, mime_type: str) -> bool:
    """Validate the declared image type without decoding untrusted pixels."""

    signatures = {
        "image/png": content.startswith(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg": content.startswith(b"\xff\xd8\xff"),
        "image/jpg": content.startswith(b"\xff\xd8\xff"),
        "image/gif": content.startswith((b"GIF87a", b"GIF89a")),
        "image/webp": len(content) >= 12
        and content.startswith(b"RIFF")
        and content[8:12] == b"WEBP",
    }
    return signatures.get(mime_type.lower(), False)


def _google_parts(payload: Mapping[str, Any]) -> list[Any]:
    values: list[Any] = []
    for candidate in payload.get("candidates", []):
        for part in (candidate.get("content", {}) or {}).get("parts", []):
            inline = part.get("inlineData") if isinstance(part, Mapping) else None
            if isinstance(inline, Mapping) and inline.get("data"):
                values.append(
                    {
                        "data": inline["data"],
                        "mime_type": inline.get("mimeType")
                        or inline.get("mime_type")
                        or "image/png",
                    }
                )
    return values
