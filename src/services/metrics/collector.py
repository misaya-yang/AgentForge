"""
指标收集模块

提供：
- 请求指标（QPS、延迟、成功率）
- 服务指标（熔断器状态、限流触发）
- 资源指标（连接池、队列长度）
- Prometheus 格式导出
"""

from __future__ import annotations

import time
from collections import defaultdict
from dataclasses import dataclass, field
from typing import Any

from ai_gateway_core.logging import get_logger

logger = get_logger(__name__)


@dataclass
class MetricValue:
    """指标值"""

    value: float
    timestamp: float = field(default_factory=time.time)
    labels: dict[str, str] = field(default_factory=dict)


class Counter:
    """
    计数器

    单调递增的计数器，用于记录请求数、错误数等。
    """

    def __init__(self, name: str, description: str = "", labels: list[str] | None = None):
        self.name = name
        self.description = description
        self.label_names = labels or []
        self._values: dict[tuple, float] = defaultdict(float)

    def inc(self, value: float = 1.0, **labels) -> None:
        """增加计数"""
        label_key = self._make_label_key(labels)
        self._values[label_key] += value

    def _make_label_key(self, labels: dict[str, str]) -> tuple:
        """生成标签键"""
        return tuple(labels.get(name, "") for name in self.label_names)

    def get(self, **labels) -> float:
        """获取当前值"""
        label_key = self._make_label_key(labels)
        return self._values.get(label_key, 0.0)

    def collect(self) -> list[MetricValue]:
        """收集所有值"""
        result = []
        for label_key, value in self._values.items():
            labels = dict(zip(self.label_names, label_key, strict=False))
            result.append(MetricValue(value=value, labels=labels))
        return result


class Gauge:
    """
    仪表盘

    可增可减的数值，用于记录当前连接数、队列长度等。
    """

    def __init__(self, name: str, description: str = "", labels: list[str] | None = None):
        self.name = name
        self.description = description
        self.label_names = labels or []
        self._values: dict[tuple, float] = defaultdict(float)

    def set(self, value: float, **labels) -> None:
        """设置值"""
        label_key = self._make_label_key(labels)
        self._values[label_key] = value

    def inc(self, value: float = 1.0, **labels) -> None:
        """增加值"""
        label_key = self._make_label_key(labels)
        self._values[label_key] += value

    def dec(self, value: float = 1.0, **labels) -> None:
        """减少值"""
        label_key = self._make_label_key(labels)
        self._values[label_key] -= value

    def _make_label_key(self, labels: dict[str, str]) -> tuple:
        """生成标签键"""
        return tuple(labels.get(name, "") for name in self.label_names)

    def get(self, **labels) -> float:
        """获取当前值"""
        label_key = self._make_label_key(labels)
        return self._values.get(label_key, 0.0)

    def collect(self) -> list[MetricValue]:
        """收集所有值"""
        result = []
        for label_key, value in self._values.items():
            labels = dict(zip(self.label_names, label_key, strict=False))
            result.append(MetricValue(value=value, labels=labels))
        return result


class Histogram:
    """
    直方图

    记录值的分布，用于延迟分位数统计。
    """

    # 默认延迟桶边界（毫秒）
    DEFAULT_BUCKETS = (5, 10, 25, 50, 75, 100, 250, 500, 750, 1000, 2500, 5000, 7500, 10000)

    def __init__(
        self,
        name: str,
        description: str = "",
        labels: list[str] | None = None,
        buckets: tuple | None = None,
    ):
        self.name = name
        self.description = description
        self.label_names = labels or []
        self.buckets = buckets or self.DEFAULT_BUCKETS

        # 每个标签组合的桶计数
        self._bucket_counts: dict[tuple, dict[float, int]] = defaultdict(
            lambda: dict.fromkeys(self.buckets, 0)
        )
        # 总和
        self._sums: dict[tuple, float] = defaultdict(float)
        # 计数
        self._counts: dict[tuple, int] = defaultdict(int)
        # 所有值（用于计算分位数）
        self._values: dict[tuple, list[float]] = defaultdict(list)

    def observe(self, value: float, **labels) -> None:
        """记录一个观测值"""
        label_key = self._make_label_key(labels)

        # 更新桶
        for bucket in self.buckets:
            if value <= bucket:
                self._bucket_counts[label_key][bucket] += 1

        # 更新总和和计数
        self._sums[label_key] += value
        self._counts[label_key] += 1

        # 保存值（限制数量以避免内存问题）
        values = self._values[label_key]
        values.append(value)
        if len(values) > 10000:
            # 保留最近的值
            self._values[label_key] = values[-10000:]

    def _make_label_key(self, labels: dict[str, str]) -> tuple:
        """生成标签键"""
        return tuple(labels.get(name, "") for name in self.label_names)

    def percentile(self, p: float, **labels) -> float | None:
        """计算分位数"""
        label_key = self._make_label_key(labels)
        values = sorted(self._values.get(label_key, []))
        if not values:
            return None

        index = int(len(values) * p / 100)
        return values[min(index, len(values) - 1)]

    def avg(self, **labels) -> float | None:
        """计算平均值"""
        label_key = self._make_label_key(labels)
        count = self._counts.get(label_key, 0)
        if count == 0:
            return None
        return self._sums.get(label_key, 0) / count

    def collect(self) -> dict[str, Any]:
        """收集指标"""
        result = {
            "buckets": {},
            "sum": {},
            "count": {},
        }

        for label_key in self._counts:
            result["buckets"][label_key] = dict(self._bucket_counts[label_key])
            result["sum"][label_key] = self._sums[label_key]
            result["count"][label_key] = self._counts[label_key]

        return result


@dataclass
class RequestMetrics:
    """
    请求指标收集

    封装常用的请求相关指标。
    """

    # 请求计数
    requests_total: Counter = field(
        default_factory=lambda: Counter(
            "gateway_requests_total",
            "Total number of requests",
            labels=["method", "path", "status_code", "service_id"],
        )
    )

    # 请求延迟
    request_duration_ms: Histogram = field(
        default_factory=lambda: Histogram(
            "gateway_request_duration_ms",
            "Request duration in milliseconds",
            labels=["method", "path", "service_id"],
        )
    )

    # 错误计数
    errors_total: Counter = field(
        default_factory=lambda: Counter(
            "gateway_errors_total",
            "Total number of errors",
            labels=["method", "path", "error_code", "service_id"],
        )
    )

    # 限流触发
    rate_limit_triggered: Counter = field(
        default_factory=lambda: Counter(
            "gateway_rate_limit_triggered_total",
            "Total number of rate limit triggers",
            labels=["dimension", "service_id"],
        )
    )

    # 限流决策（允许/拒绝）
    rate_limit_decisions_total: Counter = field(
        default_factory=lambda: Counter(
            "gateway_rate_limit_decision_total",
            "Total number of rate limit decisions",
            labels=["dimension", "service_id", "result"],
        )
    )

    # 熔断器状态
    circuit_breaker_state: Gauge = field(
        default_factory=lambda: Gauge(
            "gateway_circuit_breaker_state",
            "Circuit breaker state (0=closed, 1=open, 2=half-open)",
            labels=["service_id"],
        )
    )

    # 活跃连接数
    active_connections: Gauge = field(
        default_factory=lambda: Gauge(
            "gateway_active_connections",
            "Number of active connections",
            labels=["service_id"],
        )
    )

    # Billing flush 失败分类 (Phase 0 hotfix)
    billing_flush_failures_total: Counter = field(
        default_factory=lambda: Counter(
            "gateway_billing_flush_failures_total",
            "Billing buffer flush failures by stage",
            labels=["stage", "error_type"],  # stage: callback | redis | database
        )
    )

    # Billing 最终丢弃 (Phase 0 hotfix)
    billing_records_dropped_total: Counter = field(
        default_factory=lambda: Counter(
            "gateway_billing_records_dropped_total",
            "Billing records permanently dropped after retries / DLQ",
            labels=["reason"],  # max_retries_exceeded | dead_letter_full
        )
    )

    telemetry_background_backlog: Gauge = field(
        default_factory=lambda: Gauge(
            "gateway_telemetry_background_backlog",
            "Non-critical request telemetry writes currently in flight",
        )
    )

    telemetry_records_dropped_total: Counter = field(
        default_factory=lambda: Counter(
            "gateway_telemetry_records_dropped_total",
            "Non-critical request telemetry records dropped before recording",
            labels=["reason"],
        )
    )

    telemetry_record_failures_total: Counter = field(
        default_factory=lambda: Counter(
            "gateway_telemetry_record_failures_total",
            "Request telemetry writes that failed or exceeded their deadline",
            labels=["priority", "reason"],
        )
    )

    def record_request(
        self,
        method: str,
        path: str,
        status_code: int,
        duration_ms: float,
        service_id: str = "",
    ) -> None:
        """记录请求指标"""
        self.requests_total.inc(
            method=method,
            path=path,
            status_code=str(status_code),
            service_id=service_id,
        )

        self.request_duration_ms.observe(
            duration_ms,
            method=method,
            path=path,
            service_id=service_id,
        )

        if status_code >= 400:
            self.errors_total.inc(
                method=method,
                path=path,
                error_code=str(status_code),
                service_id=service_id,
            )

    def record_rate_limit(self, dimension: str, service_id: str = "") -> None:
        """记录限流触发"""
        self.rate_limit_triggered.inc(dimension=dimension, service_id=service_id)

    def record_rate_limit_decision(
        self,
        dimension: str,
        service_id: str = "",
        result: str = "allowed",
    ) -> None:
        """记录限流决策结果（allowed/blocked）。"""
        self.rate_limit_decisions_total.inc(
            dimension=dimension,
            service_id=service_id,
            result=result,
        )

    def update_circuit_breaker(self, service_id: str, state: str) -> None:
        """更新熔断器状态"""
        state_value = {"closed": 0, "open": 1, "half_open": 2}.get(state, 0)
        self.circuit_breaker_state.set(state_value, service_id=service_id)

    def get_stats(self) -> dict[str, Any]:
        """获取统计信息"""
        return {
            "requests": {
                "total": sum(self.requests_total._values.values()),
                "by_status": {
                    label_key[2]: value for label_key, value in self.requests_total._values.items()
                },
            },
            "latency": {
                "p50": self.request_duration_ms.percentile(50),
                "p95": self.request_duration_ms.percentile(95),
                "p99": self.request_duration_ms.percentile(99),
                "avg": self.request_duration_ms.avg(),
            },
            "errors": {
                "total": sum(self.errors_total._values.values()),
            },
            "rate_limits": {
                "total": sum(self.rate_limit_triggered._values.values()),
                "decisions_total": sum(self.rate_limit_decisions_total._values.values()),
            },
        }


class MetricsCollector:
    """
    指标收集器

    管理所有指标的注册和收集。
    """

    _instance: MetricsCollector | None = None
    _lock: Any = None  # threading.Lock

    def __new__(cls) -> MetricsCollector:
        """线程安全的单例模式"""
        if cls._lock is None:
            import threading

            cls._lock = threading.Lock()

        if cls._instance is None:
            with cls._lock:
                # 双重检查
                if cls._instance is None:
                    instance = super().__new__(cls)
                    instance._counters = {}
                    instance._gauges = {}
                    instance._histograms = {}
                    instance.request_metrics = RequestMetrics()
                    instance._register_request_metrics()
                    instance.gateway_up = instance.register_gauge(
                        Gauge("gateway_up", "Gateway metrics endpoint availability")
                    )
                    instance.gateway_up.set(1)
                    cls._instance = instance
        return cls._instance

    def __init__(self):
        # 初始化已在 __new__ 中完成
        pass

    def register_counter(self, counter: Counter) -> Counter:
        """注册计数器"""
        self._counters[counter.name] = counter
        return counter

    def register_gauge(self, gauge: Gauge) -> Gauge:
        """注册仪表盘"""
        self._gauges[gauge.name] = gauge
        return gauge

    def register_histogram(self, histogram: Histogram) -> Histogram:
        """注册直方图"""
        self._histograms[histogram.name] = histogram
        return histogram

    def _register_request_metrics(self) -> None:
        """Register default request metrics for Prometheus export."""
        request_metrics = self.request_metrics
        self.register_counter(request_metrics.requests_total)
        self.register_counter(request_metrics.errors_total)
        self.register_counter(request_metrics.rate_limit_triggered)
        self.register_counter(request_metrics.rate_limit_decisions_total)
        self.register_counter(request_metrics.billing_flush_failures_total)
        self.register_counter(request_metrics.billing_records_dropped_total)
        self.register_counter(request_metrics.telemetry_records_dropped_total)
        self.register_counter(request_metrics.telemetry_record_failures_total)
        self.register_gauge(request_metrics.circuit_breaker_state)
        self.register_gauge(request_metrics.active_connections)
        self.register_gauge(request_metrics.telemetry_background_backlog)
        self.register_histogram(request_metrics.request_duration_ms)

    def to_prometheus(self) -> str:
        """
        导出 Prometheus 格式

        Returns:
            Prometheus 文本格式的指标
        """
        lines = []

        # 导出计数器
        for name, counter in self._counters.items():
            lines.append(f"# HELP {name} {counter.description}")
            lines.append(f"# TYPE {name} counter")
            for metric in counter.collect():
                label_str = self._format_labels(metric.labels)
                lines.append(f"{name}{label_str} {metric.value}")

        # 导出仪表盘
        for name, gauge in self._gauges.items():
            lines.append(f"# HELP {name} {gauge.description}")
            lines.append(f"# TYPE {name} gauge")
            for metric in gauge.collect():
                label_str = self._format_labels(metric.labels)
                lines.append(f"{name}{label_str} {metric.value}")

        # 导出直方图
        for name, histogram in self._histograms.items():
            lines.append(f"# HELP {name} {histogram.description}")
            lines.append(f"# TYPE {name} histogram")
            collected = histogram.collect()
            for label_key, buckets in collected["buckets"].items():
                base_labels = dict(zip(histogram.label_names, label_key, strict=False))
                for bucket, count in buckets.items():
                    labels = {**base_labels, "le": str(bucket)}
                    lines.append(f"{name}_bucket{self._format_labels(labels)} {count}")
            for label_key, value in collected["sum"].items():
                labels = dict(zip(histogram.label_names, label_key, strict=False))
                lines.append(f"{name}_sum{self._format_labels(labels)} {value}")
            for label_key, value in collected["count"].items():
                labels = dict(zip(histogram.label_names, label_key, strict=False))
                lines.append(f"{name}_count{self._format_labels(labels)} {value}")

        return "\n".join(lines)

    def _format_labels(self, labels: dict[str, str]) -> str:
        """格式化标签为 Prometheus 格式"""
        if not labels:
            return ""
        parts = [f'{k}="{v}"' for k, v in labels.items()]
        return "{" + ",".join(parts) + "}"


def get_metrics() -> MetricsCollector:
    """获取指标收集器单例"""
    return MetricsCollector()


def get_service_metrics() -> dict[str, Any]:
    """Gateway-owned internal HTTP metric definitions for explicit client injection."""
    collector = get_metrics()
    return {
        "duration": collector.register_histogram(
            Histogram(
                "service_call_duration_seconds",
                "Internal service call duration in seconds",
                labels=["service", "method", "status"],
                buckets=(0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10),
            )
        ),
        "total": collector.register_counter(
            Counter(
                "service_call_total",
                "Internal service calls",
                labels=["service", "method", "status"],
            )
        ),
        "inflight": collector.register_gauge(
            Gauge(
                "service_call_inflight",
                "Internal service calls in flight",
                labels=["service"],
            )
        ),
    }
