"""Backend registry for the parser cascade (PRD T4 item 2, 后端注册).

Registration is by stable string name; construction goes through factory
callables that receive the per-tenant/dataset options dict from the cascade
config.  Import of this module has no side effects — the default backends are
registered explicitly via :func:`register_defaults`.
"""

from __future__ import annotations

from collections.abc import Callable

from .base import ParserBackend, ParserError

BackendFactory = Callable[..., ParserBackend]


class UnknownBackend(ParserError):
    """Config references a backend name that was never registered."""

    def __init__(self, name: str, known: list[str]) -> None:
        self.name = name
        self.known = known
        super().__init__(f"unknown parser backend {name!r} (registered: {', '.join(sorted(known))})")


class ParserRegistry:
    """Name → factory registry.  Instances are independent; tests build their own."""

    def __init__(self) -> None:
        self._factories: dict[str, BackendFactory] = {}

    def register(self, name: str, factory: BackendFactory) -> None:
        """Register (or replace) a backend factory under ``name``."""
        if not name or not callable(factory):
            raise ParserError("register() needs a non-empty name and a callable factory")
        self._factories[name] = factory

    def unregister(self, name: str) -> None:
        self._factories.pop(name, None)

    def is_registered(self, name: str) -> bool:
        return name in self._factories

    def names(self) -> list[str]:
        return sorted(self._factories)

    def create(self, name: str, **options) -> ParserBackend:
        """Instantiate a configured backend.  Raises UnknownBackend for typos."""
        factory = self._factories.get(name)
        if factory is None:
            raise UnknownBackend(name, self.names())
        return factory(**options)


def register_defaults(registry: ParserRegistry | None = None) -> ParserRegistry:
    """Register the PRD T4 candidate backends under their stable names."""
    from .backends import (  # local imports keep package import cheap
        legacy_ocr_vlm,
        mineru,
        paddleocr,
        qwen3_vl,
        text_layer,
    )

    reg = registry or ParserRegistry()
    reg.register(text_layer.TextLayerBackend.name, text_layer.TextLayerBackend)
    reg.register(legacy_ocr_vlm.LegacyOCRVLMBackend.name, legacy_ocr_vlm.LegacyOCRVLMBackend)
    reg.register(mineru.MinerUBackend.name, mineru.MinerUBackend)
    reg.register(paddleocr.PPStructureV3Backend.name, paddleocr.PPStructureV3Backend)
    reg.register(paddleocr.PaddleOCRVLBackend.name, paddleocr.PaddleOCRVLBackend)
    reg.register(qwen3_vl.GeneralVLMFallbackBackend.name, qwen3_vl.GeneralVLMFallbackBackend)
    return reg
