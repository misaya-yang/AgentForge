"""
Confluence Polling Scheduler.

Provides scheduled polling for Confluence space synchronization.
Uses asyncio for lightweight scheduling without external dependencies.
"""

from __future__ import annotations

import asyncio
import contextlib
import logging
from collections.abc import Callable
from datetime import datetime, timedelta, timezone
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .sync_service import ConfluenceSyncService

logger = logging.getLogger(__name__)


def _to_naive_utc(dt: datetime | None) -> datetime | None:
    """
    Convert a datetime to naive UTC for comparison.

    This handles both timezone-aware (from database) and naive datetimes.
    """
    if dt is None:
        return None
    if dt.tzinfo is not None:
        # Convert to UTC and remove timezone info
        return dt.astimezone(timezone.utc).replace(tzinfo=None)
    return dt


def _utc_now() -> datetime:
    """Get current UTC time as naive datetime for consistent comparisons."""
    # Use timezone-aware now() instead of deprecated utcnow()
    return datetime.now(timezone.utc).replace(tzinfo=None)


class PollingTask:
    """单个轮询任务（绑定级别）"""

    def __init__(
        self,
        binding_id: str,
        interval_minutes: int,
        callback: Callable,
    ):
        self.binding_id = binding_id
        self.interval_minutes = interval_minutes
        self.callback = callback
        self.last_run: datetime | None = None
        self.next_run: datetime | None = None
        self.is_running: bool = False
        self.run_count: int = 0
        self.error_count: int = 0
        self.last_error: str | None = None
        self._task: asyncio.Task | None = None

    @property
    def interval_seconds(self) -> int:
        return self.interval_minutes * 60

    def schedule_next(self) -> None:
        """计算下次运行时间"""
        self.next_run = _utc_now() + timedelta(minutes=self.interval_minutes)

    def should_run(self) -> bool:
        """检查是否应该运行"""
        if self.is_running:
            return False
        if self.next_run is None:
            return True
        # Convert to naive UTC for comparison (handles tz-aware from DB)
        next_run_naive = _to_naive_utc(self.next_run)
        return _utc_now() >= next_run_naive


class PagePollingTask:
    """单个页面轮询任务"""

    def __init__(
        self,
        page_record_id: str,
        interval_minutes: int,
        priority: int = 0,
    ):
        self.page_record_id = page_record_id
        self.interval_minutes = interval_minutes
        self.priority = priority
        self.last_run: datetime | None = None
        self.next_run: datetime | None = None
        self.is_running: bool = False
        self.run_count: int = 0
        self.error_count: int = 0
        self.last_error: str | None = None

    def schedule_next(self) -> None:
        """计算下次运行时间"""
        self.next_run = _utc_now() + timedelta(minutes=self.interval_minutes)

    def should_run(self) -> bool:
        """检查是否应该运行"""
        if self.is_running:
            return False
        if self.next_run is None:
            return True
        # Convert to naive UTC for comparison (handles tz-aware from DB)
        next_run_naive = _to_naive_utc(self.next_run)
        return _utc_now() >= next_run_naive


class ConfluenceScheduler:
    """
    Confluence 轮询调度器

    管理多个空间绑定的定时同步任务。
    使用 asyncio 实现轻量级调度。

    Features:
    - 动态添加/移除轮询任务
    - 独立的轮询间隔
    - 错误重试和回退
    - 并发控制
    """

    def __init__(
        self,
        sync_service: ConfluenceSyncService,
        max_concurrent: int = 3,
        error_backoff_minutes: int = 5,
        max_error_backoff_minutes: int = 60,
        check_interval_seconds: int = 30,
        test_mode: bool = False,
        test_interval_seconds: int = 10,
    ):
        """
        初始化调度器

        Args:
            sync_service: Confluence 同步服务
            max_concurrent: 最大并发同步数
            error_backoff_minutes: 错误后初始回退时间
            max_error_backoff_minutes: 最大回退时间
            check_interval_seconds: 调度器检查间隔（秒）
            test_mode: 是否启用测试模式（允许更短的轮询间隔）
            test_interval_seconds: 测试模式下的默认轮询间隔（秒）
        """
        self.sync_service = sync_service
        self.max_concurrent = max_concurrent
        self.error_backoff_minutes = error_backoff_minutes
        self.max_error_backoff_minutes = max_error_backoff_minutes
        self.check_interval_seconds = check_interval_seconds
        self.test_mode = test_mode
        self.test_interval_seconds = test_interval_seconds

        self._tasks: dict[str, PollingTask] = {}
        self._page_tasks: dict[str, PagePollingTask] = {}  # 页面级轮询任务
        self._running: bool = False
        self._scheduler_task: asyncio.Task | None = None
        self._semaphore: asyncio.Semaphore | None = None
        self._active_syncs: set[str] = set()
        self._active_page_syncs: set[str] = set()  # 正在同步的页面

    @property
    def is_running(self) -> bool:
        """调度器是否正在运行"""
        return self._running

    @property
    def task_count(self) -> int:
        """当前绑定任务数量"""
        return len(self._tasks)

    @property
    def active_sync_count(self) -> int:
        """当前正在同步的数量"""
        return len(self._active_syncs)

    @property
    def page_task_count(self) -> int:
        """当前页面任务数量"""
        return len(self._page_tasks)

    @property
    def active_page_sync_count(self) -> int:
        """当前正在同步的页面数量"""
        return len(self._active_page_syncs)

    async def start(self) -> None:
        """启动调度器"""
        if self._running:
            logger.warning("Scheduler is already running")
            return

        self._running = True
        self._semaphore = asyncio.Semaphore(self.max_concurrent)

        # 加载已启用轮询的绑定
        await self._load_polling_bindings()

        # 加载已启用轮询的页面
        await self._load_polling_pages()

        # 启动调度循环
        self._scheduler_task = asyncio.create_task(self._scheduler_loop())
        logger.info(
            f"Confluence scheduler started with {self.task_count} binding tasks "
            f"and {self.page_task_count} page tasks"
        )

    async def stop(self, timeout_seconds: float = 30.0) -> None:
        """
        停止调度器（优雅关闭）

        Args:
            timeout_seconds: 等待活动同步完成的最大时间（秒），默认 30 秒

        Notes:
            1. 首先停止接受新的同步请求
            2. 等待所有活动同步完成（最多等待 timeout_seconds）
            3. 超时后记录被中断的任务
        """
        if not self._running:
            return

        self._running = False
        logger.info("Confluence scheduler shutting down...")

        # 取消调度任务（停止新任务调度）
        if self._scheduler_task:
            self._scheduler_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._scheduler_task

        # 等待所有活动同步完成
        active_bindings = set(self._active_syncs)
        active_pages = set(self._active_page_syncs)
        active_count = len(active_bindings) + len(active_pages)

        if active_count > 0:
            logger.info(
                f"Waiting for {active_count} active syncs to complete (timeout={timeout_seconds}s)"
            )
            logger.debug(f"Active binding syncs: {active_bindings}")
            logger.debug(f"Active page syncs: {active_pages}")

            # 等待活动同步完成，定期检查
            start_time = asyncio.get_running_loop().time()
            poll_interval = 0.5  # 每 0.5 秒检查一次

            while True:
                elapsed = asyncio.get_running_loop().time() - start_time
                current_count = len(self._active_syncs) + len(self._active_page_syncs)

                if current_count == 0:
                    logger.info("All active syncs completed gracefully")
                    break

                if elapsed >= timeout_seconds:
                    # 超时 - 记录被中断的任务
                    interrupted_bindings = list(self._active_syncs)
                    interrupted_pages = list(self._active_page_syncs)

                    logger.warning(
                        f"Shutdown timeout reached after {elapsed:.1f}s. "
                        f"Interrupted syncs: bindings={interrupted_bindings}, "
                        f"pages={interrupted_pages}"
                    )

                    # 记录被中断的任务以便后续恢复（如果需要）
                    if interrupted_bindings or interrupted_pages:
                        logger.info(
                            "These syncs may need to be re-run on next startup: "
                            f"bindings={interrupted_bindings}, pages={interrupted_pages}"
                        )
                    break

                await asyncio.sleep(poll_interval)

        self._tasks.clear()
        self._page_tasks.clear()
        self._active_syncs.clear()
        self._active_page_syncs.clear()
        logger.info("Confluence scheduler stopped")

    async def add_task(
        self,
        binding_id: str,
        interval_minutes: int,
        run_immediately: bool = False,
    ) -> None:
        """
        添加轮询任务

        Args:
            binding_id: 空间绑定 ID
            interval_minutes: 轮询间隔（分钟）
            run_immediately: 是否立即运行一次
        """
        if binding_id in self._tasks:
            # 更新现有任务的间隔
            self._tasks[binding_id].interval_minutes = interval_minutes
            logger.info(f"Updated polling interval for binding {binding_id}")
            return

        task = PollingTask(
            binding_id=binding_id,
            interval_minutes=interval_minutes,
            callback=self._sync_binding,
        )

        if run_immediately:
            task.next_run = _utc_now()
        else:
            task.schedule_next()

        self._tasks[binding_id] = task
        logger.info(f"Added polling task for binding {binding_id}, interval={interval_minutes}min")

    async def remove_task(self, binding_id: str) -> None:
        """
        移除轮询任务

        Args:
            binding_id: 空间绑定 ID
        """
        if binding_id in self._tasks:
            del self._tasks[binding_id]
            logger.info(f"Removed polling task for binding {binding_id}")

    async def trigger_now(self, binding_id: str) -> bool:
        """
        立即触发同步

        Args:
            binding_id: 空间绑定 ID

        Returns:
            是否成功触发
        """
        if binding_id in self._active_syncs:
            logger.warning(f"Binding {binding_id} is already syncing")
            return False

        # 创建临时任务如果不存在
        if binding_id not in self._tasks:
            task = PollingTask(
                binding_id=binding_id,
                interval_minutes=0,  # 一次性任务
                callback=self._sync_binding,
            )
            self._tasks[binding_id] = task

        # 设置立即运行
        self._tasks[binding_id].next_run = _utc_now()
        return True

    async def get_status(self) -> dict:
        """获取调度器状态"""
        tasks_status = []
        for binding_id, task in self._tasks.items():
            tasks_status.append(
                {
                    "binding_id": binding_id,
                    "interval_minutes": task.interval_minutes,
                    "is_running": task.is_running,
                    "last_run": task.last_run.isoformat() if task.last_run else None,
                    "next_run": task.next_run.isoformat() if task.next_run else None,
                    "run_count": task.run_count,
                    "error_count": task.error_count,
                    "last_error": task.last_error,
                }
            )

        page_tasks_status = []
        for page_id, task in self._page_tasks.items():
            page_tasks_status.append(
                {
                    "page_record_id": page_id,
                    "interval_minutes": task.interval_minutes,
                    "priority": task.priority,
                    "is_running": task.is_running,
                    "last_run": task.last_run.isoformat() if task.last_run else None,
                    "next_run": task.next_run.isoformat() if task.next_run else None,
                    "run_count": task.run_count,
                    "error_count": task.error_count,
                    "last_error": task.last_error,
                }
            )

        return {
            "is_running": self._running,
            "task_count": self.task_count,
            "page_task_count": self.page_task_count,
            "active_sync_count": self.active_sync_count,
            "active_page_sync_count": self.active_page_sync_count,
            "max_concurrent": self.max_concurrent,
            "binding_tasks": tasks_status,
            "page_tasks": page_tasks_status,
        }

    async def _load_polling_bindings(self) -> None:
        """从数据库加载启用轮询的绑定（支持绑定级别的 sync_mode 配置）"""
        try:
            # 使用统一的 list_bindings_for_polling 方法
            # 该方法已处理绑定级别和连接级别的配置回退逻辑
            polling_bindings = await self.sync_service.list_bindings_for_polling()

            for binding in polling_bindings:
                binding_id = binding["binding_id"]
                interval_minutes = binding.get("polling_interval_minutes", 60)

                # 如果绑定已完成或正在同步，加入调度
                if binding.get("status") in ("completed", "pending", "syncing"):
                    await self.add_task(
                        binding_id=binding_id,
                        interval_minutes=interval_minutes,
                    )

            logger.info(f"Loaded {self.task_count} polling tasks from database")

        except Exception as e:
            logger.error(f"Failed to load polling bindings: {e}")

    async def _load_polling_pages(self) -> None:
        """
        从数据库加载所有启用轮询的页面。

        使用 get_all_polling_pages 而不是 get_pages_due_for_sync，
        这样可以加载所有启用轮询的页面，而不只是已到期的页面。
        调度器会根据 next_sync_at 决定何时执行每个任务。
        """
        try:
            # 使用 get_all_polling_pages 加载所有启用轮询的页面
            polling_pages = await self.sync_service.db.get_all_polling_pages(limit=500)

            for page in polling_pages:
                page_id = page.get("id")
                if not page_id:
                    continue

                # Use "or 60" to handle both missing key AND NULL value from DB
                # (dict.get("key", default) returns NULL if key exists with NULL value)
                interval_minutes = page.get("polling_interval_minutes") or 60
                priority = page.get("sync_priority") or 0

                if page_id not in self._page_tasks:
                    task = PagePollingTask(
                        page_record_id=page_id,
                        interval_minutes=interval_minutes,
                        priority=priority,
                    )
                    # 根据 next_sync_at 设置下次运行时间（转换为 naive UTC）
                    next_sync_at = page.get("next_sync_at")
                    if next_sync_at:
                        # 使用 _to_naive_utc 处理 tz-aware datetime
                        task.next_run = _to_naive_utc(next_sync_at)
                    else:
                        task.next_run = _utc_now()  # 立即运行
                    self._page_tasks[page_id] = task

            logger.info(f"Loaded {self.page_task_count} page polling tasks from database")

        except Exception as e:
            logger.error(f"Failed to load polling pages: {e}")

    async def _scheduler_loop(self) -> None:
        """调度主循环"""
        check_interval = self.check_interval_seconds
        if self.test_mode:
            check_interval = min(check_interval, 5)  # 测试模式下更频繁检查
            logger.info(f"Scheduler running in TEST MODE with check_interval={check_interval}s")
        logger.debug(f"Scheduler loop started (check_interval={check_interval}s)")

        while self._running:
            try:
                # 检查所有绑定任务
                binding_tasks_to_run = []
                for binding_id, task in list(self._tasks.items()):
                    if task.should_run():
                        binding_tasks_to_run.append(binding_id)

                # 检查所有页面任务（按优先级排序）
                page_tasks_to_run = []
                for page_id, task in list(self._page_tasks.items()):
                    if task.should_run():
                        page_tasks_to_run.append((page_id, task.priority))

                # 按优先级排序页面任务（高优先级先执行）
                page_tasks_to_run.sort(key=lambda x: x[1], reverse=True)
                page_ids_to_run = [pid for pid, _ in page_tasks_to_run]

                # 并发执行待运行的任务
                all_tasks = []
                if binding_tasks_to_run:
                    logger.debug(f"Running {len(binding_tasks_to_run)} binding sync tasks")
                    all_tasks.extend([self._run_task(bid) for bid in binding_tasks_to_run])

                if page_ids_to_run:
                    logger.debug(f"Running {len(page_ids_to_run)} page sync tasks")
                    all_tasks.extend([self._run_page_task(pid) for pid in page_ids_to_run])

                if all_tasks:
                    await asyncio.gather(*all_tasks, return_exceptions=True)

                # 等待一段时间再检查
                await asyncio.sleep(check_interval)

            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error(f"Error in scheduler loop: {e}")
                await asyncio.sleep(check_interval * 2)

        logger.debug("Scheduler loop stopped")

    async def _run_task(self, binding_id: str) -> None:
        """运行单个任务"""
        if binding_id not in self._tasks:
            return

        task = self._tasks[binding_id]
        if task.is_running or binding_id in self._active_syncs:
            return

        # 获取信号量
        if self._semaphore is None:
            return

        async with self._semaphore:
            task.is_running = True
            self._active_syncs.add(binding_id)

            try:
                logger.info(f"Starting polling sync for binding {binding_id}")
                await task.callback(binding_id)

                # 成功后重置错误计数
                task.run_count += 1
                task.error_count = 0
                task.last_error = None
                task.last_run = _utc_now()
                task.schedule_next()

                # 更新数据库中的 next_sync_at
                await self._update_binding_next_sync(binding_id, task.interval_minutes)

                logger.info(f"Polling sync completed for binding {binding_id}")

            except Exception as e:
                logger.error(f"Polling sync failed for binding {binding_id}: {e}")
                task.error_count += 1
                task.last_error = str(e)

                # 错误回退
                backoff = min(
                    self.error_backoff_minutes * (2 ** (task.error_count - 1)),
                    self.max_error_backoff_minutes,
                )
                task.next_run = _utc_now() + timedelta(minutes=backoff)

                # 更新数据库中的 next_sync_at（使用回退时间）
                await self._update_binding_next_sync(binding_id, backoff)

                logger.info(f"Next retry for binding {binding_id} in {backoff} minutes")

            finally:
                task.is_running = False
                self._active_syncs.discard(binding_id)

    async def _update_binding_next_sync(self, binding_id: str, interval_minutes: int) -> None:
        """更新绑定的下次同步时间到数据库"""
        try:
            next_sync_at = _utc_now() + timedelta(minutes=interval_minutes)
            await self.sync_service.db.update_confluence_binding(
                binding_id, {"next_sync_at": next_sync_at}
            )
        except Exception as e:
            logger.warning(f"Failed to update next_sync_at for binding {binding_id}: {e}")

    async def _sync_binding(self, binding_id: str) -> None:
        """执行绑定同步（使用增量同步）"""
        try:
            # 优先使用增量同步
            await self.sync_service.incremental_sync(binding_id)
        except Exception as e:
            logger.warning(f"Incremental sync failed for {binding_id}, error: {e}")
            # 如果增量同步失败（可能是首次同步），回退到全量同步
            await self.sync_service.trigger_sync(binding_id)

    async def _run_page_task(self, page_record_id: str) -> None:
        """运行单个页面同步任务"""
        if page_record_id not in self._page_tasks:
            return

        task = self._page_tasks[page_record_id]
        if task.is_running or page_record_id in self._active_page_syncs:
            return

        # 获取信号量
        if self._semaphore is None:
            return

        async with self._semaphore:
            task.is_running = True
            self._active_page_syncs.add(page_record_id)

            try:
                logger.info(f"Starting polling sync for page {page_record_id}")
                result = await self.sync_service.sync_page(page_record_id)

                # 检查 sync_page 返回值，如果 status != "success" 则视为失败
                if isinstance(result, dict) and result.get("status") == "error":
                    error_msg = result.get("message", "Unknown error")
                    raise RuntimeError(f"sync_page returned error: {error_msg}")

                # 成功后重置错误计数
                task.run_count += 1
                task.error_count = 0
                task.last_error = None
                task.last_run = _utc_now()
                task.schedule_next()

                # 更新数据库中的 next_sync_at
                await self._update_page_next_sync(page_record_id, task.interval_minutes)

                logger.info(f"Polling sync completed for page {page_record_id}")

            except Exception as e:
                logger.error(f"Polling sync failed for page {page_record_id}: {e}")
                task.error_count += 1
                task.last_error = str(e)

                # 错误回退
                backoff = min(
                    self.error_backoff_minutes * (2 ** (task.error_count - 1)),
                    self.max_error_backoff_minutes,
                )
                task.next_run = _utc_now() + timedelta(minutes=backoff)

                # 更新数据库中的 next_sync_at（使用回退时间）
                await self._update_page_next_sync(page_record_id, backoff)

                logger.info(f"Next retry for page {page_record_id} in {backoff} minutes")

            finally:
                task.is_running = False
                self._active_page_syncs.discard(page_record_id)

    async def _update_page_next_sync(self, page_record_id: str, interval_minutes: int) -> None:
        """更新页面的下次同步时间到数据库"""
        try:
            next_sync_at = _utc_now() + timedelta(minutes=interval_minutes)
            await self.sync_service.db.update_confluence_page_sync_config(
                page_record_id, {"next_sync_at": next_sync_at}
            )
        except Exception as e:
            logger.warning(f"Failed to update next_sync_at for page {page_record_id}: {e}")

    async def reschedule_binding(self, binding_id: str, interval_minutes: int) -> None:
        """
        重新调度绑定（当配置变更时调用）

        Args:
            binding_id: 绑定 ID
            interval_minutes: 新的轮询间隔（分钟）
        """
        if binding_id in self._tasks:
            task = self._tasks[binding_id]
            old_interval = task.interval_minutes
            task.interval_minutes = interval_minutes
            task.schedule_next()
            logger.info(
                f"Rescheduled binding {binding_id}: "
                f"interval changed from {old_interval}min to {interval_minutes}min"
            )
        else:
            # 如果任务不存在，添加新任务
            await self.add_task(binding_id, interval_minutes)

    async def disable_binding(self, binding_id: str) -> None:
        """
        禁用绑定的轮询

        Args:
            binding_id: 绑定 ID
        """
        await self.remove_task(binding_id)
        logger.info(f"Disabled polling for binding {binding_id}")

    async def reload_bindings(self) -> None:
        """
        重新加载所有绑定配置

        在配置变更后调用以刷新调度
        """
        logger.info("Reloading polling bindings...")

        # 获取当前应该轮询的绑定
        polling_bindings = await self.sync_service.list_bindings_for_polling()
        polling_binding_ids = {b["binding_id"] for b in polling_bindings}

        # 移除不再需要轮询的任务
        for binding_id in list(self._tasks.keys()):
            if binding_id not in polling_binding_ids:
                await self.remove_task(binding_id)

        # 添加或更新轮询任务
        for binding in polling_bindings:
            binding_id = binding["binding_id"]
            interval = binding.get("polling_interval_minutes", 60)

            if binding_id in self._tasks:
                # 更新间隔
                if self._tasks[binding_id].interval_minutes != interval:
                    await self.reschedule_binding(binding_id, interval)
            else:
                # 添加新任务
                await self.add_task(binding_id, interval)

        logger.info(f"Reloaded {self.task_count} polling tasks")

    # ============ 页面级任务管理 ============

    async def add_page_task(
        self,
        page_record_id: str,
        interval_minutes: int,
        priority: int = 0,
        run_immediately: bool = False,
    ) -> None:
        """
        添加页面轮询任务

        Args:
            page_record_id: 页面记录 ID
            interval_minutes: 轮询间隔（分钟）
            priority: 同步优先级（高优先级先执行）
            run_immediately: 是否立即运行一次
        """
        if page_record_id in self._page_tasks:
            # 更新现有任务
            task = self._page_tasks[page_record_id]
            task.interval_minutes = interval_minutes
            task.priority = priority
            logger.info(f"Updated page task for {page_record_id}")
            return

        task = PagePollingTask(
            page_record_id=page_record_id,
            interval_minutes=interval_minutes,
            priority=priority,
        )

        if run_immediately:
            task.next_run = _utc_now()
        else:
            task.schedule_next()

        self._page_tasks[page_record_id] = task
        logger.info(f"Added page polling task for {page_record_id}, interval={interval_minutes}min")

    async def remove_page_task(self, page_record_id: str) -> None:
        """
        移除页面轮询任务

        Args:
            page_record_id: 页面记录 ID
        """
        if page_record_id in self._page_tasks:
            del self._page_tasks[page_record_id]
            logger.info(f"Removed page polling task for {page_record_id}")

    async def reschedule_page(
        self,
        page_record_id: str,
        interval_minutes: int,
        priority: int | None = None,
    ) -> None:
        """
        重新调度页面（当配置变更时调用）

        Args:
            page_record_id: 页面记录 ID
            interval_minutes: 新的轮询间隔（分钟）
            priority: 新的优先级（可选）
        """
        if page_record_id in self._page_tasks:
            task = self._page_tasks[page_record_id]
            old_interval = task.interval_minutes
            task.interval_minutes = interval_minutes
            if priority is not None:
                task.priority = priority
            task.schedule_next()
            logger.info(
                f"Rescheduled page {page_record_id}: "
                f"interval changed from {old_interval}min to {interval_minutes}min"
            )
        else:
            # 如果任务不存在，添加新任务
            await self.add_page_task(page_record_id, interval_minutes, priority or 0)

    async def disable_page(self, page_record_id: str) -> None:
        """
        禁用页面的轮询

        Args:
            page_record_id: 页面记录 ID
        """
        await self.remove_page_task(page_record_id)
        logger.info(f"Disabled polling for page {page_record_id}")

    async def reload_pages(self) -> None:
        """
        重新加载所有页面轮询配置

        在配置变更后调用以刷新调度。
        会加载所有 sync_enabled=TRUE AND sync_mode='polling' 的页面。
        """
        logger.info("Reloading page polling tasks...")

        # 获取所有应该轮询的页面
        polling_pages = await self.sync_service.db.get_all_polling_pages(limit=500)
        polling_page_ids = {p["id"] for p in polling_pages if p.get("id")}

        # 移除不再需要轮询的任务
        for page_id in list(self._page_tasks.keys()):
            if page_id not in polling_page_ids:
                await self.remove_page_task(page_id)

        # 添加或更新轮询任务
        for page in polling_pages:
            page_id = page.get("id")
            if not page_id:
                continue

            interval = page.get("polling_interval_minutes", 60)
            priority = page.get("sync_priority", 0)
            next_sync_at = page.get("next_sync_at")

            if page_id in self._page_tasks:
                # 更新配置
                task = self._page_tasks[page_id]
                if task.interval_minutes != interval or task.priority != priority:
                    await self.reschedule_page(page_id, interval, priority)
            else:
                # 添加新任务
                task = PagePollingTask(
                    page_record_id=page_id,
                    interval_minutes=interval,
                    priority=priority,
                )
                # 根据 next_sync_at 设置下次运行时间
                if next_sync_at:
                    task.next_run = _to_naive_utc(next_sync_at)
                else:
                    task.next_run = _utc_now()  # 立即运行
                self._page_tasks[page_id] = task

        logger.info(f"Reloaded {self.page_task_count} page polling tasks")


class SchedulerManager:
    """
    调度器管理器

    提供全局单例访问和生命周期管理。
    """

    _instance: ConfluenceScheduler | None = None

    @classmethod
    def get_instance(cls) -> ConfluenceScheduler | None:
        """获取调度器实例"""
        return cls._instance

    @classmethod
    def initialize(
        cls,
        sync_service: ConfluenceSyncService,
        **kwargs,
    ) -> ConfluenceScheduler:
        """
        初始化调度器

        Args:
            sync_service: 同步服务实例
            **kwargs: 传递给 ConfluenceScheduler 的参数

        Returns:
            调度器实例
        """
        if cls._instance is not None:
            logger.warning("Scheduler already initialized, returning existing instance")
            return cls._instance

        cls._instance = ConfluenceScheduler(sync_service, **kwargs)
        return cls._instance

    @classmethod
    async def start(cls) -> None:
        """启动调度器"""
        if cls._instance:
            await cls._instance.start()

    @classmethod
    async def stop(cls) -> None:
        """停止调度器"""
        if cls._instance:
            await cls._instance.stop()
            cls._instance = None
