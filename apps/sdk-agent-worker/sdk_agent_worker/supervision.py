"""Strong run ownership. Request cancellation never owns process cleanup."""
import asyncio
import anyio
import os
from collections import deque
from .lifecycle import begin_cleanup, complete_cleanup, touch_activity

CLEANUP_OBSERVATION_SECONDS = 5.0
SHUTDOWN_OBSERVATION_SECONDS = 5.0
MAX_QUEUED_EVENTS = 16
FAILURE = {"event": "error", "code": "sdk_execution_failed"}


class RunOwner:
    def __init__(self, supervisor, state, lock, operation, cleanup, execution_seconds):
        self.supervisor, self.state, self.lock = supervisor, state, lock
        self.operation, self.cleanup = operation, cleanup
        self.execution_seconds = execution_seconds
        self.drain = None
        self.task = self.execution = self.cleanup_task = self.spawn_task = None
        self.child = None
        self.watchers = []
        self.events = deque()
        self.changed, self.capacity = asyncio.Event(), asyncio.Event()
        self.stop_event = asyncio.Event()
        self.stop_wait = None
        self.stopping = self.detached = self.finished = self.released = False
        self.uncertain = False
        self.commit_started = self.committed = False
        self.committed_result = None
        self.result = None

    def start(self):
        if self.task is None and not self.released:
            # The durable refusal exists before any subprocess creation.
            try:
                begin_cleanup(self.state)
            except Exception:
                self.uncertain = True
                try:
                    self.abort_unstarted()
                except Exception:
                    pass
                self.finish(FAILURE)
                return
            self.task = asyncio.create_task(self._run())
            self.task.add_done_callback(self._done)

    def _done(self, task):
        if task.cancelled() or task.exception() is not None:
            self.uncertain = True
            self.finish(self.committed_result if self.committed else FAILURE)
        # A failed owner remains strongly referenced, including its lock.

    def request_stop(self, detach=False):
        self.stopping = True
        self.stop_event.set()
        if detach:
            self.detached = True
            self.events.clear()
            self.capacity.set()
        if self.execution is not None and not self.execution.done():
            self.execution.cancel()
        if self.task is not None:
            self.cleanup_started()

    async def emit(self, value):
        while len(self.events) >= MAX_QUEUED_EVENTS and not self.detached:
            self.capacity.clear()
            await self.capacity.wait()
        if not self.detached:
            self.events.append(value)
            self.changed.set()

    def finish(self, result):
        if not self.finished:
            self.result, self.finished = result, True
            self.changed.set()

    async def iterate(self):
        try:
            self.start()
            while True:
                if self.events:
                    value = self.events.popleft()
                    self.capacity.set()
                    yield value
                elif self.finished:
                    yield self.result
                    return
                else:
                    self.changed.clear()
                    await self.changed.wait()
        finally:
            self.detached = True
            self.events.clear()
            self.capacity.set()
            if not self.finished:
                self.request_stop()
                # wait() observes without propagating cancellation into the owner.
                if self.task is not None:
                    with anyio.CancelScope(shield=True):
                        _, pending = await asyncio.wait({self.task}, timeout=CLEANUP_OBSERVATION_SECONDS)
                        if pending:
                            self.uncertain = True
                            self.finish(self.committed_result if self.committed else FAILURE)

    def cleanup_started(self):
        if self.cleanup_task is None:
            self.cleanup_deadline = asyncio.get_running_loop().time() + CLEANUP_OBSERVATION_SECONDS
            self.cleanup_task = asyncio.create_task(self.cleanup(self))
            self.cleanup_task.add_done_callback(self._cleanup_done)
        return self.cleanup_task

    def _cleanup_done(self, task):
        if task.cancelled() or task.exception() is not None:
            self.uncertain = True

    async def observe_cleanup(self):
        cleanup = self.cleanup_started()
        remaining = max(0, self.cleanup_deadline - asyncio.get_running_loop().time())
        done, _ = await asyncio.wait({cleanup}, timeout=remaining)
        return done

    async def cleaned(self):
        if not await self.observe_cleanup():
            self.uncertain = True
            raise RuntimeError("sdk_cleanup_unresolved")
        self.cleanup_task.result()

    def release(self):
        # Only the observed-success path calls this, never a cancellation finally.
        complete_cleanup(self.state)
        touch_activity(self.state)
        self.lock.close()
        self.released = True
        self.supervisor.owners.pop(self.state, None)

    def abort_unstarted(self):
        if self.task is None and not self.released:
            # No child creation was started; retain execution's pending marker.
            self.release()

    async def _run(self):
        result = FAILURE
        try:
            self.execution = asyncio.create_task(self.operation(self))
            if self.stopping:
                self.request_stop()
            # Unlike timeout cancellation alone, observation stays bounded even
            # if an awaited operation suppresses its cancellation.
            self.stop_wait = asyncio.create_task(self.stop_event.wait())
            done, _ = await asyncio.wait({self.execution, self.stop_wait},
                timeout=self.execution_seconds, return_when=asyncio.FIRST_COMPLETED)
            self.stop_wait.cancel()
            await asyncio.gather(self.stop_wait, return_exceptions=True)
            if self.execution not in done:
                self.request_stop()
            elif not self.execution.cancelled():
                try:
                    result = self.execution.result()
                except Exception:
                    pass
            cleanup = self.cleanup_started()
            remaining = max(0, self.cleanup_deadline - asyncio.get_running_loop().time())
            _, pending = await asyncio.wait({self.execution, cleanup}, timeout=remaining)
            if pending:
                self.uncertain = True
                self.request_stop()
                self.finish(self.committed_result if self.committed else FAILURE)
                # Strong ownership continues after response observation expires.
                self.drain = asyncio.gather(self.execution, cleanup, return_exceptions=True)
                await asyncio.shield(self.drain)
                result = FAILURE
            cleanup.result()
            if self.commit_started and not self.committed:
                self.uncertain = True
                self.finish(FAILURE)
                return
            self.release()
            self.finish(self.committed_result if self.committed else result)
        except (asyncio.CancelledError, Exception):
            self.uncertain = True
            self.request_stop()
            self.finish(self.committed_result if self.committed else FAILURE)
            # Strong registry + durable marker retain unknown ownership. No unlock.


class RunSupervisor:
    def __init__(self):
        self.owners = {}
        self.closing = False
        self.retention_task = None

    def ready(self, root):
        if self.closing or any(owner.uncertain for owner in self.owners.values()):
            return False
        directory = root / ".cleanup"
        if directory.is_symlink():
            return False
        if directory.exists():
            # A crash/restart marker without its live owner is never auto-cleared.
            with os.scandir(directory) as entries:
                for index, entry in enumerate(entries):
                    if index >= 100 or root / entry.name.removesuffix(".json") not in self.owners:
                        return False
        return True

    def own(self, state, lock, operation, cleanup, execution_seconds=300):
        if not self.ready(state.parent) or state in self.owners:
            raise RuntimeError("sdk_cleanup_unavailable")
        owner = RunOwner(self, state, lock, operation, cleanup, execution_seconds)
        self.owners[state] = owner
        return owner

    async def shutdown(self, retention=None):
        self.closing = True
        tasks = set()
        if retention is not None:
            self.retention_task = retention
            if not retention.done():
                retention.cancel()
            tasks.add(retention)
        for owner in list(self.owners.values()):
            if owner.task is None:
                owner.abort_unstarted()
            else:
                owner.request_stop(detach=True)
                tasks.update(task for task in [owner.task, owner.execution,
                    owner.cleanup_task, owner.spawn_task, owner.stop_wait,
                    owner.drain, *owner.watchers] if task is not None)
        if tasks:
            _, pending = await asyncio.wait(tasks, timeout=SHUTDOWN_OBSERVATION_SECONDS)
            for owner in self.owners.values():
                if owner.task in pending:
                    owner.uncertain = True
                    owner.finish(owner.committed_result if owner.committed else FAILURE)
        retention_clean = retention is None or (retention.done() and (retention.cancelled() or retention.exception() is None))
        # Returning is not proof of physical termination. Unresolved owners/locks
        # stay held while this process lives; durable refusal survives its exit.
        return not self.owners and retention_clean
