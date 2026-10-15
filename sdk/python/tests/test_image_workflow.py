"""Offline image submit/read wire contracts through the public Python SDK."""
from __future__ import annotations

import json
import unittest

import httpx
from ai_assistant import AssistantClient, AssistantError, UnknownOutcomeError

TASK = "imt_0123456789abcdef0123"
MODEL = "registered-image-model"
SUBMIT = "/api/v1/assistant/generate-image-async"
POLL = f"/api/v1/assistant/image-task/{TASK}"


class ImageWorkflowTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.client = AssistantClient("synthetic-key", base_url="https://gateway.example")
        await self.client._transport._client.aclose()
        self.requests = []
        self.responses = []

        def handle(request):
            self.requests.append(request)
            if not self.responses:
                raise AssertionError("Unexpected additional request")
            response = self.responses.pop(0)
            if isinstance(response, BaseException):
                raise response
            return response

        self.client._transport._client = httpx.AsyncClient(
            base_url="https://gateway.example", transport=httpx.MockTransport(handle),
        )

    async def asyncTearDown(self):
        await self.client.close()

    def receipt(self, **changes):
        return {"task_id": TASK, "status": "pending", "message": "Submitted", **changes}

    async def test_async_uses_submit_route(self):
        receipt = self.receipt()
        self.responses = [httpx.Response(200, json=receipt)]
        self.assertEqual(await self.client.images.generate_async("hello", model_id=MODEL), receipt)
        self.assertEqual(self.requests[0].url.path, SUBMIT)
        self.assertEqual(self.requests[0].method, "POST")
        self.assertEqual(json.loads(self.requests[0].content), {"prompt": "hello",
                                                               "model_id": MODEL,
                                                               "n": 1})
        self.assertEqual(self.requests[0].headers["X-API-Key"], "synthetic-key")
        self.assertEqual(len(self.requests), 1)

    async def test_poll_uses_image_task_route(self):
        value = self.receipt(status="running", progress=0, images=[], duration_ms=None)
        self.responses = [httpx.Response(200, json=value)]
        self.assertEqual(await self.client.images.get_task(TASK), value)
        self.assertEqual(self.requests[0].url.path, POLL)
        self.assertEqual(self.requests[0].method, "GET")
        self.assertEqual(len(self.requests), 1)

    async def test_async_requires_explicit_model(self):
        self.responses = [httpx.Response(200, json=self.receipt())]
        with self.assertRaises(ValueError):
            await self.client.images.generate_async("hello")
        self.assertEqual(self.requests, [])

    async def test_cancel_is_explicitly_unsupported(self):
        self.responses = [httpx.Response(200, json={"cancelled": True})]
        with self.assertRaises(NotImplementedError):
            await self.client.images.cancel(TASK, reason="stop")
        self.assertEqual(self.requests, [])

    async def test_sync_generate_unchanged(self):
        self.responses = [httpx.Response(200, json={"success": True, "images": [],
                                                  "provider": "fixture", "duration_ms": 0})]
        result = await self.client.images.generate("hello", style="anime", size="1024*1024", n=2)
        self.assertTrue(result.success)
        self.assertEqual(result.duration_ms, 0)
        self.assertEqual(self.requests[0].url.path, "/api/v1/assistant/generate-image")
        self.assertEqual(json.loads(self.requests[0].content), {"prompt": "hello", "style": "anime",
                                                               "size": "1024*1024", "n": 2})

    async def test_submit_then_explicit_reads_never_resubmits(self):
        completed = self.receipt(status="completed", progress=100,
                                images=[{"artifact_id": "art_one", "url": "/download/one"}],
                                effective_model_id=MODEL, latest_advanced=True)
        self.responses = [httpx.Response(200, json=self.receipt()),
                          httpx.Response(200, json=self.receipt(status="running")),
                          httpx.Response(200, json=completed)]
        receipt = await self.client.images.generate_async("hello", model_id=MODEL,
                                                          style="anime", size="512*512", n=2)
        running = await self.client.images.get_task(receipt["task_id"])
        self.assertEqual(running["status"], "running")
        self.assertEqual(await self.client.images.get_task(receipt["task_id"]), completed)
        self.assertEqual([(r.method, r.url.path) for r in self.requests],
                         [("POST", SUBMIT), ("GET", POLL), ("GET", POLL)])
        self.assertEqual(json.loads(self.requests[0].content), {"prompt": "hello",
                                                               "model_id": MODEL,
                                                               "style": "anime", "size": "512*512",
                                                               "n": 2})

    async def test_poll_rejected_access_does_not_return_previous_snapshot(self):
        self.responses = [httpx.Response(200, json=self.receipt(status="running")),
                          httpx.Response(403, json={"detail": "Denied"})]
        await self.client.images.get_task(TASK)
        with self.assertRaises(AssistantError) as caught:
            await self.client.images.get_task(TASK)
        self.assertEqual(caught.exception.status_code, 403)
        self.assertEqual([r.method for r in self.requests], ["GET", "GET"])

    async def test_poll_mismatched_identity_is_read_error(self):
        self.responses = [httpx.Response(200, json=self.receipt(task_id="imt_other"))]
        with self.assertRaises(AssistantError) as caught:
            await self.client.images.get_task(TASK)
        self.assertNotIsInstance(caught.exception, UnknownOutcomeError)
        self.assertEqual(len(self.requests), 1)

    async def test_submit_malformed_json_is_unknown(self):
        self.responses = [httpx.Response(200, content=b"not-json")]
        with self.assertRaises(UnknownOutcomeError):
            await self.client.images.generate_async("hello", model_id=MODEL)
        self.assertEqual(len(self.requests), 1)

    async def test_poll_malformed_json_is_read_error(self):
        self.responses = [httpx.Response(200, content=b"not-json")]
        with self.assertRaises(AssistantError) as caught:
            await self.client.images.get_task(TASK)
        self.assertNotIsInstance(caught.exception, UnknownOutcomeError)
        self.assertEqual(len(self.requests), 1)

    async def test_submit_timeout_is_unknown_without_replay(self):
        self.responses = [httpx.ReadTimeout("fixture timeout")]
        with self.assertRaises(UnknownOutcomeError):
            await self.client.images.generate_async("hello", model_id=MODEL)
        self.assertEqual(len(self.requests), 1)

    async def test_submit_transport_error_is_unknown_without_replay(self):
        self.responses = [httpx.ConnectError("fixture connection error")]
        with self.assertRaises(UnknownOutcomeError):
            await self.client.images.generate_async("hello", model_id=MODEL)
        self.assertEqual(len(self.requests), 1)

    async def check_status(self, status):
        value = self.receipt(status=status, images=[], error_code="synthetic",
                             future_field={"v": 1})
        self.responses = [httpx.Response(200, json=value)]
        self.assertEqual(await self.client.images.get_task(TASK), value)
        self.assertEqual(len(self.requests), 1)

    async def check_invalid_model(self, model):
        with self.assertRaises(ValueError):
            await self.client.images.generate_async("hello", model_id=model)
        self.assertEqual(self.requests, [])

    async def check_invalid_task(self, task):
        with self.assertRaises(ValueError):
            await self.client.images.get_task(task)
        self.assertEqual(self.requests, [])

    async def check_invalid_receipt(self, value):
        self.responses = [httpx.Response(200, json=value), httpx.Response(200, json=value)]
        with self.assertRaises(UnknownOutcomeError):
            await self.client.images.generate_async("hello", model_id=MODEL)
        with self.assertRaises(AssistantError) as caught:
            await self.client.images.get_task(TASK)
        self.assertNotIsInstance(caught.exception, UnknownOutcomeError)
        self.assertEqual([r.method for r in self.requests], ["POST", "GET"])

    async def check_submit_status(self, status):
        self.responses = [httpx.Response(status, json={"detail": "Rejected"},
                                         headers={"Location": "https://elsewhere.example"})]
        with self.assertRaises(AssistantError) as caught:
            await self.client.images.generate_async("hello", model_id=MODEL)
        self.assertEqual(caught.exception.status_code, status)
        if status in (408, 500, 503, 307):
            self.assertIsInstance(caught.exception, UnknownOutcomeError)
        else:
            self.assertNotIsInstance(caught.exception, UnknownOutcomeError)
        self.assertEqual(len(self.requests), 1)


def case(check, value):
    async def test(self):
        await getattr(self, "check_" + check)(value)
    return test


_CASES = {
    "status": ["pending", "running", "completed", "failed", "unknown", "future_state"],
    "invalid_model": ["", "  ", 7, False],
    "invalid_task": ["", ".", "..", "x/y", "x\\y", "x?y", "x#y", "x%2fy", "x y", "x\ny",
                     7, None, "x" * 161],
    "invalid_receipt": [[], {}, {"task_id": TASK}, {"task_id": "../wrong", "status": "pending"},
                        {"task_id": TASK, "status": ""}, {"task_id": TASK, "status": "  "},
                        {"task_id": TASK, "status": 1}],
    "submit_status": [401, 403, 404, 422, 429, 408, 500, 503, 307],
}
for _check, _values in _CASES.items():
    for _index, _value in enumerate(_values):
        setattr(ImageWorkflowTests, f"test_{_check}_{_index}", case(_check, _value))
