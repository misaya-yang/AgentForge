"""Tests for VLMOCRService."""

from __future__ import annotations

import asyncio
import json
from unittest.mock import MagicMock

import httpx
import pytest
from knowledge_service.config import OCRSettings
from knowledge_service.services.knowledge.vlm_ocr_service import (
    VLMOCRService,
    _DashScopeOCRBackend,
    _detect_media_type,
)


def _make_service(api_key="test-key", model="gemini-2.5-flash", concurrency=2):
    """Create a VLMOCRService with a mocked backend."""
    svc = VLMOCRService.__new__(VLMOCRService)
    svc.api_key = api_key
    svc.model = model
    svc.provider = "mock"
    svc.concurrency = concurrency
    svc.timeout_seconds = 30
    svc.max_retries = 2
    svc._semaphore = asyncio.Semaphore(concurrency)
    # Mock backend with async-compatible call
    svc._backend = MagicMock()
    return svc


def _set_backend_return(svc, text="Extracted Arabic text"):
    """Configure backend to return given text."""
    async def _call(image_bytes):
        return text
    svc._backend.call = _call


def _set_backend_error(svc, error=RuntimeError("API error")):
    """Configure backend to raise an exception."""
    async def _call(image_bytes):
        raise error
    svc._backend.call = _call


def _set_backend_sequence(svc, results):
    """Configure backend to return different results on each call."""
    call_idx = {"i": 0}
    async def _call(image_bytes):
        idx = call_idx["i"]
        call_idx["i"] += 1
        r = results[idx % len(results)]
        if isinstance(r, Exception):
            raise r
        return r
    svc._backend.call = _call


class TestVLMOCRServiceInit:
    def test_requires_api_key(self):
        with pytest.raises(ValueError):
            VLMOCRService(api_key="")

    def test_auto_detects_gemini_provider(self):
        """Provider 'auto' with gemini model should resolve to gemini."""
        # This will try to import google.genai which may or may not be available
        try:
            svc = VLMOCRService(api_key="test-key", model="gemini-2.5-flash", provider="auto")
            assert svc.provider == "gemini"
        except Exception:
            pytest.skip("google-genai not available")

    def test_auto_detects_dashscope_provider(self):
        """Provider 'auto' with non-gemini model should resolve to dashscope."""
        try:
            svc = VLMOCRService(api_key="test-key", model="qwen-vl-max", provider="auto")
            assert svc.provider == "dashscope"
        except ImportError:
            pytest.skip("dashscope not available")

    def test_unknown_provider_raises(self):
        with pytest.raises(ValueError, match="Unknown OCR provider"):
            svc = VLMOCRService.__new__(VLMOCRService)
            VLMOCRService.__init__(svc, api_key="key", model="x", provider="unknown")


def test_qwen_ocr_is_the_default_vlm_configuration():
    settings = OCRSettings()

    assert settings.vlm_provider == "dashscope"
    assert settings.vlm_model == "qwen-vl-ocr"
    assert settings.vlm_task == "document_parsing"
    assert settings.vlm_min_pixels == 3072
    assert settings.vlm_max_pixels == 8_388_608
    assert settings.vlm_max_tokens == 8_192
    assert settings.vlm_enable_rotate is True


@pytest.mark.asyncio
async def test_dashscope_qwen_ocr_uses_native_document_parsing_contract():
    observed: dict[str, object] = {}

    async def handler(request: httpx.Request) -> httpx.Response:
        observed["url"] = str(request.url)
        observed["payload"] = json.loads(request.content)
        return httpx.Response(
            200,
            json={
                "output": {
                    "choices": [
                        {"message": {"content": [{"text": "Qwen OCR text"}]}}
                    ]
                }
            },
            request=request,
        )

    backend = _DashScopeOCRBackend(
        api_key="test-key",
        model="qwen-vl-ocr",
        base_url="https://workspace.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
        task="document_parsing",
        min_pixels=3072,
        max_pixels=8_388_608,
        max_tokens=8_192,
        enable_rotate=True,
        transport=httpx.MockTransport(handler),
    )
    try:
        result = await backend.call(b"\x89PNG\r\n\x1a\nimage")
    finally:
        await backend.close()

    assert result == "Qwen OCR text"
    assert observed["url"] == (
        "https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1/"
        "services/aigc/multimodal-generation/generation"
    )
    payload = observed["payload"]
    assert isinstance(payload, dict)
    assert payload["model"] == "qwen-vl-ocr"
    assert payload["parameters"] == {
        "max_tokens": 8_192,
        "ocr_options": {"task": "document_parsing"},
    }
    image = payload["input"]["messages"][0]["content"][0]
    assert image["image"].startswith("data:image/png;base64,")
    assert image["min_pixels"] == 3072
    assert image["max_pixels"] == 8_388_608
    assert image["enable_rotate"] is True


@pytest.mark.asyncio
async def test_dashscope_qwen_ocr_surfaces_api_errors_without_leaking_credentials():
    async def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            503,
            json={"code": "ServiceUnavailable", "message": "temporarily unavailable"},
            request=request,
        )

    backend = _DashScopeOCRBackend(
        api_key="secret-must-not-appear",
        model="qwen-vl-ocr",
        transport=httpx.MockTransport(handler),
    )
    try:
        with pytest.raises(RuntimeError, match="503") as exc_info:
            await backend.call(b"image")
    finally:
        await backend.close()

    assert "secret-must-not-appear" not in str(exc_info.value)


class TestOCRImage:
    @pytest.mark.asyncio
    async def test_ocr_image_success(self):
        svc = _make_service()
        _set_backend_return(svc, "بسم الله الرحمن الرحيم")

        result = await svc.ocr_image(b"\x89PNG\r\n\x1a\nfake_png_data")
        assert result == "بسم الله الرحمن الرحيم"

    @pytest.mark.asyncio
    async def test_ocr_image_empty_response(self):
        svc = _make_service()
        _set_backend_return(svc, "")

        result = await svc.ocr_image(b"\x89PNG\r\n\x1a\nfake")
        assert result == ""

    @pytest.mark.asyncio
    async def test_ocr_image_api_error_returns_empty(self):
        svc = _make_service()
        _set_backend_error(svc, RuntimeError("Server error"))

        result = await svc.ocr_image(b"\x89PNG\r\n\x1a\nfake")
        assert result == ""

    @pytest.mark.asyncio
    async def test_ocr_image_exception_retry_and_fail(self):
        svc = _make_service()
        call_count = 0
        original_error = RuntimeError("Network error")

        async def _failing_call(image_bytes):
            nonlocal call_count
            call_count += 1
            raise original_error

        svc._backend.call = _failing_call

        result = await svc.ocr_image(b"\x89PNG\r\n\x1a\nfake")
        assert result == ""
        assert call_count == 2  # max_retries=2


class TestOCRPdfPages:
    @pytest.mark.asyncio
    async def test_ocr_pdf_pages_multiple(self):
        svc = _make_service()
        _set_backend_sequence(svc, ["Page 1 text", "Page 2 text", "Page 3 text"])

        images = [b"\x89PNG\r\n\x1a\npage1", b"\x89PNG\r\n\x1a\npage2", b"\x89PNG\r\n\x1a\npage3"]
        results = await svc.ocr_pdf_pages(images)

        assert len(results) == 3
        assert all(r for r in results)

    @pytest.mark.asyncio
    async def test_ocr_pdf_pages_empty_list(self):
        svc = _make_service()
        results = await svc.ocr_pdf_pages([])
        assert results == []

    @pytest.mark.asyncio
    async def test_ocr_pdf_pages_partial_failure(self):
        svc = _make_service()
        _set_backend_sequence(svc, [
            "Page 1",
            RuntimeError("API error"),  # page 2 fails all retries
            "Page 3",
        ])

        images = [b"\x89PNG\r\n\x1a\np1", b"\x89PNG\r\n\x1a\np2", b"\x89PNG\r\n\x1a\np3"]
        results = await svc.ocr_pdf_pages(images)

        assert len(results) == 3
        non_empty = [r for r in results if r]
        assert len(non_empty) >= 1


class TestOCRPrompt:
    def test_prompt_contains_arabic_instruction(self):
        assert "Arabic" in VLMOCRService.OCR_PROMPT
        assert "RTL" in VLMOCRService.OCR_PROMPT

    def test_prompt_contains_no_translate(self):
        assert "NOT translate" in VLMOCRService.OCR_PROMPT


class TestMediaTypeDetection:
    def test_detect_png(self):
        assert _detect_media_type(b"\x89PNG\r\n\x1a\n") == "image/png"

    def test_detect_jpeg(self):
        assert _detect_media_type(b"\xff\xd8" + b"\x00" * 6) == "image/jpeg"

    def test_detect_unknown_defaults_to_png(self):
        assert _detect_media_type(b"unknown_") == "image/png"

    def test_detect_short_bytes(self):
        assert _detect_media_type(b"ab") == "image/png"

    def test_instance_method_backward_compat(self):
        """VLMOCRService._detect_media_type still works as instance method."""
        svc = _make_service()
        assert svc._detect_media_type(b"\x89PNG\r\n\x1a\n") == "image/png"
