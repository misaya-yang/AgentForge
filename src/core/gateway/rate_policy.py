from __future__ import annotations

import asyncio
import time
from collections.abc import Mapping
from dataclasses import dataclass
from types import MappingProxyType
from typing import Any

from ai_gateway_core.logging import get_logger

logger = get_logger(__name__)


@dataclass(frozen=True)
class RatePolicy:
    key: str
    dimension: str
    requests: int
    window: int
    burst: int = 0
    strategy: str = "sliding_window"


@dataclass(frozen=True)
class _RuleSnapshot:
    cache_key: tuple[int, int]
    expires_at: float
    stale_until: float
    rules: tuple[Mapping[str, Any], ...]


class RatePolicyResolver:
    """Resolve runtime rate-limit rules into concrete limiter checks."""

    _SCOPE_PRECEDENCE = {
        "service": 10,
        "api_key": 20,
        "tenant": 30,
        "user": 40,
        "operation": 50,
        "global": 60,
    }
    _BURST_STRATEGIES = {"token_bucket", "sliding_window_with_burst"}

    def __init__(
        self,
        *,
        cache_ttl_seconds: float = 1.0,
        max_stale_seconds: float = 5.0,
    ) -> None:
        self.cache_ttl_seconds = max(float(cache_ttl_seconds), 0.0)
        self.max_stale_seconds = max(float(max_stale_seconds), 0.0)
        self._snapshot: _RuleSnapshot | None = None
        self._refresh_lock = asyncio.Lock()
        self._refresh_task: asyncio.Task[None] | None = None
        self._refresh_failure: tuple[tuple[int, int], float, Exception] | None = None
        self._refresh_failure_retry_seconds = 1.0
        self._epoch = 0

    def invalidate(self) -> None:
        self._epoch += 1
        self._snapshot = None
        self._refresh_failure = None
        if self._refresh_task is not None:
            self._refresh_task.cancel()
            self._refresh_task = None

    async def resolve(
        self,
        *,
        request: Any,
        user: Any,
        service_name: str,
        operation: str,
        service_config: Any | None,
    ) -> list[RatePolicy]:
        service_policy = self._service_config_policy(
            user=user,
            service_name=service_name,
            operation=operation,
            service_config=service_config,
        )
        if service_policy is not None:
            return [service_policy]

        rules = await self._load_rules(request)
        policies: list[RatePolicy] = []
        for rule in rules:
            policy = self._policy_from_rule(
                rule,
                request=request,
                user=user,
                service_name=service_name,
                operation=operation,
            )
            if policy is not None:
                policies.append(policy)
        return policies

    def _service_config_policy(
        self,
        *,
        user: Any,
        service_name: str,
        operation: str,
        service_config: Any | None,
    ) -> RatePolicy | None:
        if not (
            service_config
            and bool(getattr(service_config, "rate_limit_enabled", False))
            and int(getattr(service_config, "rate_limit_requests", 0) or 0) > 0
            and int(getattr(service_config, "rate_limit_window", 0) or 0) > 0
        ):
            return None

        tenant_scope = self._safe_segment(getattr(user, "tenant_id", "") or "public")
        subject = self._safe_segment(
            getattr(user, "user_id", "") or getattr(user, "ip", "") or "anonymous"
        )
        safe_operation = self._safe_segment(operation or "proxy")
        service = self._safe_segment(
            service_name or getattr(service_config, "service_id", "service")
        )
        return RatePolicy(
            key=f"ratelimit:service:{service}:{tenant_scope}:{subject}:{safe_operation}",
            dimension=f"service:{service}",
            requests=int(service_config.rate_limit_requests),
            window=int(service_config.rate_limit_window),
            burst=0,
            strategy="sliding_window",
        )

    async def _load_rules(self, request: Any) -> tuple[Mapping[str, Any], ...]:
        app_state = getattr(getattr(request, "app", None), "state", None)
        db = getattr(app_state, "database", None)
        cache_key = (id(db), self._epoch)
        now = time.monotonic()
        snapshot = self._snapshot
        if snapshot is not None and snapshot.cache_key == cache_key:
            if now < snapshot.expires_at:
                return snapshot.rules
            if now <= snapshot.stale_until:
                self._schedule_stale_refresh(request=request, cache_key=cache_key)
                return snapshot.rules
            # The stale safety budget is exhausted. Refresh synchronously via
            # the same singleflight lock; DB failure propagates so an obsolete
            # permissive policy cannot be served indefinitely.
            return await self._refresh_rules(request=request, cache_key=cache_key)

        return await self._refresh_rules(request=request, cache_key=cache_key)

    def _schedule_stale_refresh(
        self,
        *,
        request: Any,
        cache_key: tuple[int, int],
    ) -> None:
        task = self._refresh_task
        if task is not None and not task.done():
            return

        async def _refresh() -> None:
            try:
                await self._refresh_rules(request=request, cache_key=cache_key)
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                logger.debug("Failed to refresh rate policies: %s", exc)
            finally:
                if self._refresh_task is asyncio.current_task():
                    self._refresh_task = None

        try:
            loop = asyncio.get_running_loop()
            self._refresh_task = loop.create_task(_refresh())
        except RuntimeError as exc:
            logger.debug("Failed to schedule rate policy refresh: %s", exc)

    async def _refresh_rules(
        self,
        *,
        request: Any,
        cache_key: tuple[int, int],
    ) -> tuple[Mapping[str, Any], ...]:
        async with self._refresh_lock:
            snapshot = self._snapshot
            now = time.monotonic()
            if (
                snapshot is not None
                and snapshot.cache_key == cache_key
                and now < snapshot.expires_at
            ):
                return snapshot.rules
            failure = self._refresh_failure
            if (
                failure is not None
                and failure[0] == cache_key
                and now < failure[1]
            ):
                raise failure[2]

            app_state = getattr(getattr(request, "app", None), "state", None)
            db = getattr(app_state, "database", None)
            rules: list[dict[str, Any]] = []
            if db and getattr(db, "enabled", False) and hasattr(db, "get_rate_limits"):
                try:
                    raw_rules = await db.get_rate_limits()
                except Exception as exc:
                    if cache_key == (id(db), self._epoch):
                        self._refresh_failure = (
                            cache_key,
                            time.monotonic() + self._refresh_failure_retry_seconds,
                            exc,
                        )
                    raise
                rules = [self._coerce_rule(rule) for rule in raw_rules or []]
            else:
                runtime_rules = getattr(app_state, "rate_limit_rules", None) or []
                rules = [self._coerce_rule(rule) for rule in runtime_rules]

            compiled = tuple(
                MappingProxyType(dict(rule))
                for rule in sorted(rules, key=self._rule_sort_key)
            )
            if cache_key == (id(db), self._epoch):
                self._refresh_failure = None
                refreshed_at = time.monotonic()
                self._snapshot = _RuleSnapshot(
                    cache_key=cache_key,
                    expires_at=refreshed_at + self.cache_ttl_seconds,
                    stale_until=(
                        refreshed_at
                        + self.cache_ttl_seconds
                        + self.max_stale_seconds
                    ),
                    rules=compiled,
                )
            return compiled

    def _policy_from_rule(
        self,
        rule: Mapping[str, Any],
        *,
        request: Any,
        user: Any,
        service_name: str,
        operation: str,
    ) -> RatePolicy | None:
        if not bool(rule.get("enabled", True)):
            return None
        scope = str(rule.get("scope") or "").strip().lower()
        scope_id = str(rule.get("scope_id") or "").strip()
        if not self._rule_matches(
            scope=scope,
            scope_id=scope_id,
            request=request,
            user=user,
            service_name=service_name,
            operation=operation,
        ):
            return None

        requests = max(int(rule.get("requests") or 0), 1)
        window = max(int(rule.get("window") or rule.get("window_seconds") or 0), 1)
        burst = max(int(rule.get("burst") or 0), 0)
        strategy = str(rule.get("strategy") or "sliding_window").strip() or "sliding_window"
        effective_requests = requests + burst if strategy in self._BURST_STRATEGIES else requests
        dimension = self._dimension(scope, scope_id, service_name, operation)
        return RatePolicy(
            key=self._key_for_policy(
                scope=scope,
                scope_id=scope_id,
                dimension=dimension,
                user=user,
                operation=operation,
            ),
            dimension=dimension,
            requests=effective_requests,
            window=window,
            burst=burst,
            strategy=strategy,
        )

    def _rule_matches(
        self,
        *,
        scope: str,
        scope_id: str,
        request: Any,
        user: Any,
        service_name: str,
        operation: str,
    ) -> bool:
        if scope == "global":
            return True
        if scope == "service":
            return scope_id in {service_name, getattr(user, "service_id", "")}
        if scope == "api_key":
            state = getattr(request, "state", None)
            api_key_hash = str(getattr(state, "api_key_hash", "") or "")
            api_key_info = getattr(state, "api_key_info", None) or {}
            candidates = {
                api_key_hash,
                str(api_key_info.get("id") or ""),
                str(api_key_info.get("key_id") or ""),
                str(api_key_info.get("api_key_id") or ""),
            }
            return bool(scope_id and scope_id in candidates)
        if scope == "tenant":
            return bool(scope_id and scope_id == str(getattr(user, "tenant_id", "") or ""))
        if scope == "user":
            return bool(scope_id and scope_id == str(getattr(user, "user_id", "") or ""))
        if scope == "operation":
            return bool(scope_id and scope_id == str(operation or ""))
        return False

    def _dimension(
        self,
        scope: str,
        scope_id: str,
        service_name: str,
        operation: str,
    ) -> str:
        if scope == "global":
            return "global"
        if scope == "service":
            return f"service:{scope_id or service_name}"
        if scope == "tenant":
            return f"tenant:{scope_id}"
        if scope == "user":
            return f"user:{scope_id}"
        if scope == "operation":
            return f"operation:{scope_id or operation}"
        return scope

    def _key_for_policy(
        self,
        *,
        scope: str,
        scope_id: str,
        dimension: str,
        user: Any,
        operation: str,
    ) -> str:
        tenant = self._safe_segment(getattr(user, "tenant_id", "") or "public")
        subject = self._safe_segment(
            getattr(user, "user_id", "") or getattr(user, "ip", "") or "anonymous"
        )
        safe_operation = self._safe_segment(operation or "proxy")
        target = self._safe_segment(scope_id or dimension)
        return f"ratelimit:{scope}:{target}:{tenant}:{subject}:{safe_operation}"

    def _rule_sort_key(self, rule: Mapping[str, Any]) -> tuple[int, int]:
        scope = str(rule.get("scope") or "").strip().lower()
        priority = int(rule.get("priority") or 0)
        return (self._SCOPE_PRECEDENCE.get(scope, 999), priority)

    def _coerce_rule(self, rule: Any) -> dict[str, Any]:
        if hasattr(rule, "keys"):
            return dict(rule)
        if hasattr(rule, "model_dump"):
            return dict(rule.model_dump())
        if hasattr(rule, "__dict__"):
            return dict(rule.__dict__)
        return {}

    def _safe_segment(self, value: Any) -> str:
        normalized = str(value or "").strip() or "unknown"
        return normalized.replace(":", "_").replace("|", "_")
