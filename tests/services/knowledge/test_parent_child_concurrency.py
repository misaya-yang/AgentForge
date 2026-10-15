"""Offline parent-fold resource/cancellation regressions; no database or models."""

from __future__ import annotations

import asyncio
import copy
import unittest
from types import SimpleNamespace

from knowledge_service.services.knowledge.parent_child_retrieval import (
    ParentChildSettings,
    fold_candidates_to_parents,
)


def child(index, parent_id=None, *, score=None):
    return {
        "segment_id": f"child-{index}",
        "document_id": "doc-a",
        "text": f"child text {index}",
        "metadata": {"parent_segment_id": parent_id or f"parent-{index}"},
        "_final_score": float(index if score is None else score),
        "_sources": {"dense"},
    }


def parent(segment_id):
    return {"segment_id": segment_id, "document_id": "doc-a", "text": segment_id}


class LookupProbe:
    def __init__(self, *, failures=None):
        self.failures = failures or {}
        self.calls = []
        self.active = 0
        self.peak_active = 0
        self.tasks = set()

    async def get_segment_scoped(self, **kwargs):
        self.calls.append(kwargs)
        self.tasks.add(asyncio.current_task())
        self.active += 1
        self.peak_active = max(self.peak_active, self.active)
        try:
            await asyncio.sleep(0)
            segment_id = kwargs["segment_id"]
            if segment_id in self.failures:
                value = self.failures[segment_id]
                if isinstance(value, BaseException):
                    raise value
                return value
            return parent(segment_id)
        finally:
            self.active -= 1


class ParentChildConcurrencyTests(unittest.IsolatedAsyncioTestCase):
    async def fold(self, rows, db, **kwargs):
        return await fold_candidates_to_parents(
            rows, settings=ParentChildSettings(), db=db,
            dataset_id="dataset-a", tenant_id="tenant-a", **kwargs,
        )

    async def test_2000_parents_bound_queries_and_tasks_without_truncation(self):
        rows = [child(index) for index in range(2000)]
        original = copy.deepcopy(rows)
        db = LookupProbe()
        folded, stats = await self.fold(rows, db, max_concurrency=3)
        self.assertEqual(db.peak_active, 3)
        self.assertEqual(len(db.tasks), 3)
        self.assertEqual(len(db.calls), 2000)
        self.assertEqual(len(folded), 2000)
        self.assertEqual(stats["unresolved_parents"], 0)
        self.assertEqual([row["segment_id"] for row in folded],
                         [f"parent-{index}" for index in range(2000)])
        self.assertEqual([row["_final_score"] for row in folded],
                         [float(index) for index in range(2000)])
        self.assertEqual(rows, original)
        self.assertTrue(all(task.done() for task in db.tasks))
        self.assertTrue(all(call["dataset_id"] == "dataset-a" and
                            call["tenant_id"] == "tenant-a" for call in db.calls))

    async def test_default_and_small_inputs_bound_worker_count(self):
        for count, limit, expected in ((25, None, 8), (2, 8, 2), (20, 1, 1), (20, 0, 1)):
            with self.subTest(count=count, limit=limit):
                db = LookupProbe()
                kwargs = {} if limit is None else {"max_concurrency": limit}
                await self.fold([child(index) for index in range(count)], db, **kwargs)
                self.assertEqual(db.peak_active, expected)
                self.assertEqual(len(db.tasks), expected)

    async def test_parallel_matches_serial_with_errors_and_duplicate_parents(self):
        rows = [child(0, "shared", score=0.1), child(1, score=0.9),
                child(2, "shared", score=0.8), child(3), child(4), child(5), child(6)]
        failures = {"parent-3": None, "parent-4": RuntimeError("read failure"),
                    "parent-5": parent("wrong-parent"),
                    "parent-6": asyncio.TimeoutError("read deadline")}
        serial_db, parallel_db = LookupProbe(failures=failures), LookupProbe(failures=failures)
        serial = await self.fold(copy.deepcopy(rows), serial_db, max_concurrency=1)
        parallel = await self.fold(copy.deepcopy(rows), parallel_db, max_concurrency=3)
        self.assertEqual(parallel, serial)
        folded, stats = parallel
        self.assertEqual([row["segment_id"] for row in folded],
                         ["parent-1", "shared", "child-3", "child-4", "child-5", "child-6"])
        self.assertEqual(stats["unresolved_parents"], 4)
        self.assertEqual(len(parallel_db.calls), 6)
        self.assertEqual(sum(call["segment_id"] == "shared" for call in parallel_db.calls), 1)
        self.assertEqual(folded[1]["_final_score"], 0.8)

    async def test_completion_order_does_not_change_rank_or_tie_order(self):
        started = asyncio.Event()
        releases = {f"parent-{i}": asyncio.Event() for i in range(3)}
        calls = []

        async def get_segment_scoped(**kwargs):
            segment_id = kwargs["segment_id"]
            calls.append(segment_id)
            if len(calls) == 3:
                started.set()
            await releases[segment_id].wait()
            return parent(segment_id)

        task = asyncio.create_task(self.fold(
            [child(i, score=0.5) for i in range(3)],
            SimpleNamespace(get_segment_scoped=get_segment_scoped), max_concurrency=3,
        ))
        try:
            await asyncio.wait_for(started.wait(), 1)
            for index in (2, 1, 0):
                releases[f"parent-{index}"].set()
                await asyncio.sleep(0)
            folded, _ = await asyncio.wait_for(task, 1)
            self.assertEqual([row["segment_id"] for row in folded],
                             ["parent-0", "parent-1", "parent-2"])
        finally:
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)

    async def test_context_mode_preserves_child_identity(self):
        rows = [child(0, "shared", score=0.1), child(1, "shared", score=0.9)]
        folded, _ = await fold_candidates_to_parents(
            rows, settings=ParentChildSettings(return_mode="context"), db=LookupProbe(),
            dataset_id="dataset-a", tenant_id="tenant-a", max_concurrency=3,
        )
        self.assertEqual(folded[0]["segment_id"], "child-1")
        self.assertEqual(folded[0]["text"], "child text 1")
        self.assertEqual(folded[0]["metadata"]["parent_context"], "shared")

    async def test_unscoped_lookup_is_never_used(self):
        async def forbidden(*_args, **_kwargs):
            self.fail("unscoped authority must not be used")
        rows = [child(1)]
        folded, stats = await self.fold(rows, SimpleNamespace(
            get_segment=forbidden, get_segments_by_ids=forbidden,
        ))
        self.assertEqual(folded[0]["segment_id"], "child-1")
        self.assertEqual(stats["unresolved_parents"], 1)

    async def test_no_parent_links_start_no_workers(self):
        rows = [child(1)]
        rows[0]["metadata"] = {}
        db = LookupProbe()
        folded, stats = await self.fold(rows, db)
        self.assertEqual(folded, rows)
        self.assertEqual(stats["parents"], 0)
        self.assertFalse(db.tasks)
        self.assertEqual(await self.fold([], db), ([], stats))

    async def cancellation_case(self, *, child_cancel=False, outer_timeout=False):
        started = asyncio.Event()
        cancel_child = asyncio.Event()
        never = asyncio.Event()
        calls, tasks, cleaned = [], set(), set()

        async def get_segment_scoped(**kwargs):
            segment_id = kwargs["segment_id"]
            calls.append(segment_id)
            tasks.add(asyncio.current_task())
            if len(calls) == 3:
                started.set()
            try:
                if child_cancel and segment_id == "parent-0":
                    await cancel_child.wait()
                    raise asyncio.CancelledError()
                await never.wait()
            finally:
                cleaned.add(segment_id)

        task = asyncio.create_task(self.fold(
            [child(i) for i in range(40)],
            SimpleNamespace(get_segment_scoped=get_segment_scoped), max_concurrency=3,
        ))
        try:
            await asyncio.wait_for(started.wait(), 1)
            if child_cancel:
                cancel_child.set()
            elif not outer_timeout:
                task.cancel()
            expected = TimeoutError if outer_timeout else asyncio.CancelledError
            with self.assertRaises(expected):
                await asyncio.wait_for(task, 0.01 if outer_timeout else 1)
            self.assertEqual(len(calls), 3)
            self.assertEqual(cleaned, set(calls))
            self.assertTrue(all(worker.done() for worker in tasks))
        finally:
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)

    async def test_caller_cancellation_drains_workers_and_does_not_start_queued_reads(self):
        await self.cancellation_case()

    async def test_cancelled_read_fails_fast_and_drains_siblings(self):
        await self.cancellation_case(child_cancel=True)

    async def test_enclosing_deadline_drains_workers(self):
        await self.cancellation_case(outer_timeout=True)


if __name__ == "__main__":
    unittest.main()
