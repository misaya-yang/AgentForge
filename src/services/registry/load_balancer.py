from __future__ import annotations

import random
from dataclasses import dataclass
from typing import Any

from ...models.request import UnifiedRequest


@dataclass
class ServiceInstance:
    instance_id: str
    base_url: str
    weight: int = 1
    is_healthy: bool = True
    metadata: dict[str, Any] | None = None


class RoundRobinStrategy:
    def __init__(self):
        self._counters: dict[str, int] = {}

    def select(self, service_id: str, instances: list[ServiceInstance]) -> ServiceInstance:
        idx = self._counters.get(service_id, 0) % len(instances)
        self._counters[service_id] = idx + 1
        return instances[idx]


class RandomStrategy:
    def select(self, service_id: str, instances: list[ServiceInstance]) -> ServiceInstance:
        return random.choice(instances)


class WeightedStrategy:
    def select(self, service_id: str, instances: list[ServiceInstance]) -> ServiceInstance:
        weights = [max(1, i.weight) for i in instances]
        return random.choices(instances, weights=weights, k=1)[0]


class ConsistentHashStrategy:
    def select(
        self,
        service_id: str,
        instances: list[ServiceInstance],
        request: UnifiedRequest | None = None,
    ) -> ServiceInstance:
        key = (
            (request.session_id or request.user_id or request.request_id) if request else service_id
        )
        idx = hash(key) % len(instances)
        return instances[idx]


class LoadBalancer:
    def __init__(self, strategy: str = "round_robin"):
        self.strategies = {
            "round_robin": RoundRobinStrategy(),
            "random": RandomStrategy(),
            "weighted": WeightedStrategy(),
            "consistent_hash": ConsistentHashStrategy(),
        }
        self.strategy_name = strategy

    async def select_instance(
        self,
        service_id: str,
        instances: list[ServiceInstance],
        request: UnifiedRequest | None = None,
    ) -> ServiceInstance:
        healthy = [i for i in instances if i.is_healthy]
        if not healthy:
            healthy = instances
        strategy = self.strategies.get(self.strategy_name, self.strategies["round_robin"])
        if isinstance(strategy, ConsistentHashStrategy):
            return strategy.select(service_id, healthy, request)
        return strategy.select(service_id, healthy)
