"""Fail-closed translation for Knowledge retrieval dependency errors."""

from collections.abc import Collection, Mapping
from typing import Any, NoReturn

from ...core.observability.logging import get_logger

logger = get_logger(__name__)


def require_recall_result(errors: Collection[str], candidates: Mapping[str, Any]) -> None:
    """Distinguish a dependency outage from a valid empty search result."""

    if errors and not candidates:
        logger.error("All Knowledge recall paths failed: %s", sorted(errors))
        raise RuntimeError("knowledge retrieval dependencies failed")


def raise_batch_failure(error: Exception) -> NoReturn:
    logger.exception("[retrieve_batch] Global retrieval failed")
    raise RuntimeError("knowledge batch retrieval failed") from error
