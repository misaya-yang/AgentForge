"""Image generation callback service.

Sends completed image generation results to an external callback URL.
Used by downstream services that prefer push over polling.

Features a module-level HTTP client (connection pooling) and exponential
backoff retry for transient 5xx / network errors.
"""

import asyncio
import os
from typing import Any

import httpx

from ai_gateway_core.logging import get_logger

logger = get_logger(__name__)

_CALLBACK_TIMEOUT = float(os.getenv("IMAGE_CALLBACK_TIMEOUT", "10"))
_CALLBACK_MAX_RETRIES = int(os.getenv("IMAGE_CALLBACK_RETRIES", "3"))
_CALLBACK_BACKOFF_BASE = float(os.getenv("IMAGE_CALLBACK_BACKOFF_BASE", "0.5"))

_client: httpx.AsyncClient | None = None
_client_lock = asyncio.Lock()


async def _get_client() -> httpx.AsyncClient:
    global _client
    if _client is None:
        async with _client_lock:
            if _client is None:
                # follow_redirects=False so a hostile callback can't redirect
                # us into the private network mid-transaction. Validation
                # only happens on the original URL — without this guard a
                # 302 → http://169.254.169.254 would still leak.
                _client = httpx.AsyncClient(
                    timeout=_CALLBACK_TIMEOUT,
                    follow_redirects=False,
                )
    return _client


async def send_image_callback(callback_url: str, task: dict[str, Any]) -> bool:
    """POST completed task data to the callback URL with retry.

    Retries on 5xx or transport errors with exponential backoff. Stops
    immediately on 4xx (client error — retrying won't help).

    Returns True only when the receiver returned ``{"code": 0}``.

    SSRF guard: user-controlled callbacks are posted through
    ``safe_callback_post`` so the destination is DNS-validated, IP-pinned,
    and redirects are disabled.
    """
    from ai_gateway_core.security import SafeFetchError, safe_callback_post

    images = [
        {
            "artifact_id": img.get("artifact_id"),
            "download_url": img.get("download_url"),
            "url": img.get("url", ""),
            "width": img.get("width"),
            "height": img.get("height"),
        }
        for img in task.get("images", [])
    ]

    payload = {
        "task_id": task.get("task_id"),
        "status": task.get("status", "completed"),
        "progress": task.get("progress", 100),
        "prompt": task.get("prompt", ""),
        "model_id": task.get("model_id", ""),
        "provider": task.get("provider", ""),
        "images": images,
        "duration_ms": task.get("duration_ms"),
        "error": task.get("error"),
        "created_at": task.get("created_at"),
        "completed_at": task.get("completed_at"),
        # ---- Image-redesign additions (all optional; receivers ignore unknowns) ----
        "turn_id": task.get("turn_id"),
        "session_id": task.get("session_id"),
        "parent_artifact_id": task.get("parent_artifact_id"),
        "output_artifact_id": task.get("output_artifact_id"),
        "client_request_id": task.get("client_request_id"),
        "error_code": task.get("error_code"),
    }

    task_id = task.get("task_id")

    for attempt in range(_CALLBACK_MAX_RETRIES):
        try:
            resp = await safe_callback_post(callback_url, json=payload, timeout=_CALLBACK_TIMEOUT)
            if resp.status_code == 200:
                try:
                    body = resp.json()
                except Exception:
                    body = {}
                if body.get("code") == 0:
                    logger.info(
                        "[ImageCallback] Success → %s (task=%s, attempt=%d)",
                        callback_url, task_id, attempt + 1,
                    )
                    return True
                logger.warning(
                    "[ImageCallback] Non-zero code from %s: %s",
                    callback_url, body,
                )
                return False  # Application-level failure — don't retry

            if 400 <= resp.status_code < 500:
                logger.warning(
                    "[ImageCallback] HTTP %d from %s — not retrying",
                    resp.status_code, callback_url,
                )
                return False

            logger.warning(
                "[ImageCallback] HTTP %d from %s (attempt %d/%d)",
                resp.status_code, callback_url, attempt + 1, _CALLBACK_MAX_RETRIES,
            )
        except SafeFetchError as exc:
            logger.warning(
                "[ImageCallback] Refusing callback to %s: %s", callback_url, exc,
            )
            return False
        except (
            httpx.ConnectError,
            httpx.ReadTimeout,
            httpx.RemoteProtocolError,
            httpx.TimeoutException,
        ) as e:
            logger.warning(
                "[ImageCallback] Transport error to %s (attempt %d/%d): %s",
                callback_url, attempt + 1, _CALLBACK_MAX_RETRIES, e,
            )
        except Exception as e:
            logger.error("[ImageCallback] Unexpected error to %s: %s", callback_url, e)
            return False

        if attempt < _CALLBACK_MAX_RETRIES - 1:
            await asyncio.sleep(_CALLBACK_BACKOFF_BASE * (2 ** attempt))

    logger.error(
        "[ImageCallback] All %d attempts failed for task=%s",
        _CALLBACK_MAX_RETRIES, task_id,
    )
    return False
