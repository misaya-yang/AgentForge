"""Dataset management service for knowledge base.

Handles dataset CRUD operations, permissions, and configuration.
Migrated from KnowledgeService as part of Phase 2 refactoring.
"""

from __future__ import annotations

import asyncio
import copy
import hashlib
import json
import math
import uuid
import weakref
from datetime import datetime
from typing import TYPE_CHECKING, Any
from urllib.parse import urlsplit, urlunsplit

from ...config.settings import Settings
from ...core.auth.user_resolver import UserContext
from ...core.exceptions import PermissionDeniedError, ValidationFailedError
from ...core.observability.logging import get_logger
from ...persistence.database import (
    DatabaseStorage,
    IndexLeaseUnavailableError,
    dataset_index_deletion_fence,
    dataset_ingestion_identity,
    index_config_has_reserved_deletion_fence,
    make_dataset_index_deletion_fence,
)
from ...persistence.datasets import NON_INGESTION_INDEX_CONFIG_KEYS
from .chunking import (
    ChunkingConfig,
    flatten_chunks,
    process_document,
    validate_persisted_chunking_config,
)
from .common import ensure_dict as _ensure_dict
from .common import maybe_await
from .common import permission_rank as _permission_rank
from .lexical_config import LEXICAL_V1, LexicalConfig, LexicalConfigError
from .parsing.config_validation import resolve_parsing_config
from .query_observability import (
    QueryObservationConflictError,
    decode_query_cursor,
    encode_query_cursor,
)
from .query_preset_config import parse_query_preset_settings

if TYPE_CHECKING:
    from .knowledge_service import KnowledgeService

logger = get_logger(__name__)

_RETRIEVAL_FINGERPRINT_FIELDS = frozenset(
    {
        "adaptive_weights",
        "alpha",
        "bm25_weight",
        "candidate_top_k",
        "dense_weight",
        "enforce_config",
        "fusion",
        "fusion_method",
        "hyde",
        "keyword_candidate_k",
        "keyword_top_k",
        "lexical",
        "lock",
        "locked",
        "mmr",
        "mmr_lambda",
        "mmr_threshold",
        "mode",
        "multi_query_expansion",
        "native_hybrid",
        "parent_child",
        "query_rewrite",
        "rerank",
        "rerank_model",
        "rerank_top_n",
        "rrf_k",
        "rrf_weights",
        "score_threshold",
        "structural_routing",
        "summary_index",
        "top_k",
        "vector_top_k",
    }
)
_RETRIEVAL_NESTED_FINGERPRINT_FIELDS = {
    "fusion": frozenset(
        {
            "strategy",
            "method",
            "alpha",
            "rrf_k",
            "rrf_weights",
            "dense_weight",
            "bm25_weight",
        }
    ),
    "rerank": frozenset({"enabled", "provider", "model", "top_n"}),
    "mmr": frozenset({"enabled", "lambda", "threshold"}),
    "rrf_weights": frozenset({"vector", "keyword", "dense", "bm25"}),
    # T9 dataset switches alter retrieval evidence, so their knobs must change
    # the cache fingerprint (boolean shorthand survives via the scalar branch).
    "parent_child": frozenset({"enabled", "return_mode", "fanout_top_k"}),
    "summary_index": frozenset({"enabled", "prepend_summary"}),
    "structural_routing": frozenset({"enabled", "mode", "boost", "min_affinity"}),
    # T2-8 flags are inert today, but registering them keeps the cache version
    # key ready for the day the eval gate promotes a real behaviour change.
    "query_rewrite": frozenset({"enabled", "preset"}),
    "multi_query_expansion": frozenset({"enabled", "preset"}),
    "hyde": frozenset({"enabled"}),
}
_EMBEDDING_FINGERPRINT_FIELDS = frozenset({"base_url", "dimension", "max_concurrent"})
_DATASET_CONFIG_FIELDS = frozenset(
    {
        "embedding_provider",
        "embedding_model",
        "embedding_dimension",
        "embedding_config",
        "index_config",
        "collection_name",
    }
)
_SERVER_OWNED_DATASET_CONFIG_ALIASES = frozenset(
    {
        "apikey",
        "key",
        "accesskey",
        "secret",
        "secretkey",
        "token",
        "bearertoken",
        "authorization",
        "auth",
        "credentials",
        "headers",
        "baseurl",
        "endpoint",
        "endpointurl",
        "apibase",
        "apiurl",
        "url",
        "host",
    }
)
_SERVER_OWNED_ALIAS_SUFFIXES = (
    "apikey",
    "accesskey",
    "secret",
    "secretkey",
    "bearertoken",
    "authtoken",
    "accesstoken",
    "refreshtoken",
    "authorization",
    "credentials",
    "headers",
    "baseurl",
    "endpoint",
    "endpointurl",
    "apibase",
    "apiurl",
    "url",
    "host",
)
_MULTIMODAL_DATASET_PROVIDERS = frozenset(
    {
        "unified_multimodal",
        "unified",
        "cross_modal",
        "dashscope_multimodal",
        "aliyun_multimodal",
        "multimodal",
    }
)


def _contains_server_owned_dataset_config(value: Any) -> bool:
    if isinstance(value, dict):
        for key, nested in value.items():
            normalized = "".join(
                character for character in str(key).lower() if character.isalnum()
            )
            if normalized in _SERVER_OWNED_DATASET_CONFIG_ALIASES or normalized.endswith(
                _SERVER_OWNED_ALIAS_SUFFIXES
            ):
                return True
            if _contains_server_owned_dataset_config(nested):
                return True
    elif isinstance(value, (list, tuple)):
        return any(_contains_server_owned_dataset_config(item) for item in value)
    return False


def _require_server_owned_dataset_embedding_config(value: Any) -> None:
    if _contains_server_owned_dataset_config(_ensure_dict(value)):
        raise ValidationFailedError(
            "embedding_config credentials and endpoints are server-owned"
        )


def _require_server_owned_dataset_rerank_config(index_config: Any) -> None:
    retrieval = _ensure_dict(_ensure_dict(index_config).get("retrieval"))
    rerank = _ensure_dict(retrieval.get("rerank"))
    if _contains_server_owned_dataset_config(rerank):
        raise ValidationFailedError(
            "rerank credentials, headers, and endpoints are server-owned"
        )


def _redact_server_owned_dataset_config(value: Any) -> Any:
    """Deep-copy one response tree while masking every known secret alias."""

    if isinstance(value, dict):
        result: dict[str, Any] = {}
        for key, nested in value.items():
            normalized = "".join(
                character for character in str(key).lower() if character.isalnum()
            )
            sensitive = normalized in _SERVER_OWNED_DATASET_CONFIG_ALIASES or (
                normalized.endswith(_SERVER_OWNED_ALIAS_SUFFIXES)
            )
            if sensitive:
                result[key] = "*****"
            else:
                result[key] = _redact_server_owned_dataset_config(nested)
        return result
    if isinstance(value, list):
        return [_redact_server_owned_dataset_config(item) for item in value]
    if isinstance(value, tuple):
        return tuple(_redact_server_owned_dataset_config(item) for item in value)
    return copy.deepcopy(value)


def _dataset_requests_multimodal_profile(dataset: dict[str, Any]) -> bool:
    from .embedding import is_multimodal_embedding_model

    provider = str(dataset.get("embedding_provider") or "").strip().lower()
    model = str(dataset.get("embedding_model") or "").strip().lower()
    index_config = _ensure_dict(dataset.get("index_config"))
    embedding_config = _ensure_dict(dataset.get("embedding_config"))
    return bool(
        provider in _MULTIMODAL_DATASET_PROVIDERS
        or is_multimodal_embedding_model(model)
        or index_config.get("multimodal_enabled")
        or index_config.get("enable_multimodal")
        or embedding_config.get("multimodal_enabled")
        or embedding_config.get("enable_multimodal")
    )


def _require_multimodal_dataset_disabled(dataset: dict[str, Any]) -> None:
    if _dataset_requests_multimodal_profile(dataset):
        raise ValidationFailedError(
            "multimodal datasets are disabled until one verified profile owns "
            "creation, ingestion, retrieval hydration, and request-time URL signing"
        )


def _require_valid_embedding_dimension(payload: Any) -> None:
    value = _ensure_dict(payload)
    if "embedding_dimension" not in value:
        return
    dimension = value.get("embedding_dimension")
    if (
        isinstance(dimension, bool)
        or not isinstance(dimension, int)
        or not 1 <= dimension <= 8192
    ):
        raise ValidationFailedError(
            "embedding_dimension must be an integer between 1 and 8192"
        )


def _require_bounded_retrieval_integer(
    key: str,
    value: Any,
    *,
    minimum: int,
    maximum: int,
) -> None:
    if value is None:
        return
    if (
        isinstance(value, bool)
        or not isinstance(value, int)
        or not minimum <= value <= maximum
    ):
        raise ValidationFailedError(
            f"retrieval {key} must be an integer between {minimum} and {maximum}"
        )


def _require_bounded_retrieval_number(
    key: str,
    value: Any,
    *,
    minimum: float,
    maximum: float,
) -> float | None:
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValidationFailedError(
            f"retrieval {key} must be a finite number between {minimum} and {maximum}"
        )
    numeric = float(value)
    if not math.isfinite(numeric) or not minimum <= numeric <= maximum:
        raise ValidationFailedError(
            f"retrieval {key} must be a finite number between {minimum} and {maximum}"
        )
    return numeric


def _validate_persisted_retrieval_node(
    value: Any,
    *,
    path: tuple[str, ...] = (),
) -> None:
    if isinstance(value, dict):
        if "dense_weight" in value and "bm25_weight" in value:
            dense = _require_bounded_retrieval_number(
                "dense_weight",
                value.get("dense_weight"),
                minimum=0.0,
                maximum=1.0,
            )
            bm25 = _require_bounded_retrieval_number(
                "bm25_weight",
                value.get("bm25_weight"),
                minimum=0.0,
                maximum=1.0,
            )
            if dense == 0.0 and bm25 == 0.0:
                raise ValidationFailedError(
                    "retrieval dense_weight and bm25_weight cannot both be zero"
                )
        for raw_key, nested in value.items():
            key = str(raw_key).strip().lower().replace("-", "_")
            parent = path[-1] if path else ""

            if key == "top_k":
                maximum = 1000 if parent in {"vector", "keyword", "rerank"} else 100
                _require_bounded_retrieval_integer(
                    key,
                    nested,
                    minimum=1,
                    maximum=maximum,
                )
            elif key in {
                "vector_top_k",
                "keyword_top_k",
                "rerank_top_k",
                "rerank_top_n",
            } or (
                key == "top_n" and parent == "rerank"
            ):
                _require_bounded_retrieval_integer(
                    key,
                    nested,
                    minimum=1,
                    maximum=1000,
                )
            elif key in {"candidate_top_k", "candidate_k", "vector_candidate_k"}:
                _require_bounded_retrieval_integer(
                    key,
                    nested,
                    minimum=1,
                    maximum=2000,
                )
            elif key in {
                "keyword_candidate_k",
                "keyword_pool_k",
                "keyword_pool",
                "candidate_pool_size",
            }:
                _require_bounded_retrieval_integer(
                    key,
                    nested,
                    minimum=1,
                    maximum=500,
                )
            elif key == "rrf_k":
                _require_bounded_retrieval_integer(
                    key,
                    nested,
                    minimum=1,
                    maximum=10_000,
                )
            elif (
                key
                in {
                    "dense_weight",
                    "bm25_weight",
                    "vector_weight",
                    "keyword_weight",
                    "alpha",
                    "mmr_lambda",
                    "lambda",
                    "lambda_mult",
                    "vlm_rerank_weight",
                    "score_threshold",
                    "similarity_threshold",
                    "mmr_threshold",
                    "threshold",
                    "image_score_threshold",
                    "text_score_threshold",
                }
                or key.endswith("_weight")
                or key.endswith("_threshold")
            ):
                _require_bounded_retrieval_number(
                    key,
                    nested,
                    minimum=0.0,
                    maximum=1.0,
                )
            elif key == "bm25_k1":
                _require_bounded_retrieval_number(
                    key,
                    nested,
                    minimum=0.0,
                    maximum=3.0,
                )
            elif key == "bm25_b":
                _require_bounded_retrieval_number(
                    key,
                    nested,
                    minimum=0.0,
                    maximum=1.0,
                )
            elif key == "rrf_weights" and isinstance(nested, dict):
                weights = [
                    _require_bounded_retrieval_number(
                        f"rrf_weights.{weight_key}",
                        weight,
                        minimum=0.0,
                        maximum=1.0,
                    )
                    for weight_key, weight in nested.items()
                ]
                if weights and not any(weight and weight > 0.0 for weight in weights):
                    raise ValidationFailedError(
                        "retrieval rrf_weights must include a positive weight"
                    )

            _validate_persisted_retrieval_node(
                nested,
                path=(*path, key),
            )
        return
    if isinstance(value, (list, tuple)):
        for item in value:
            _validate_persisted_retrieval_node(item, path=path)


def _require_bounded_persisted_retrieval_config(index_config: Any) -> None:
    config = _ensure_dict(index_config)
    if "retrieval" not in config:
        return
    retrieval = config.get("retrieval")
    if not isinstance(retrieval, dict):
        raise ValidationFailedError("index_config.retrieval must be an object")
    _validate_persisted_retrieval_node(retrieval)
    # T2-8: same parser the retrieval pipeline uses at read time, so a config
    # that would fail a query can never be persisted (reject/accept parity).
    parse_query_preset_settings(retrieval)


def _require_safe_persisted_chunking_config(index_config: Any) -> None:
    config = _ensure_dict(index_config)
    if "chunking" not in config:
        return
    validate_persisted_chunking_config(config.get("chunking"))


def _secret_free_base_url(value: Any) -> str | None:
    """Keep endpoint identity while dropping URL userinfo, query, and fragment."""

    if not isinstance(value, str) or not value.strip():
        return None
    try:
        parsed = urlsplit(value.strip())
        hostname = parsed.hostname
        port = parsed.port
    except ValueError:
        return None
    if not parsed.scheme or not hostname:
        return None
    rendered_host = f"[{hostname}]" if ":" in hostname else hostname
    netloc = f"{rendered_host}:{port}" if port is not None else rendered_host
    return urlunsplit((parsed.scheme.lower(), netloc, parsed.path, "", ""))


def _safe_config_projection(
    value: Any,
    *,
    allowed: frozenset[str],
    nested: dict[str, frozenset[str]] | None = None,
) -> dict[str, Any]:
    """Return a canonical allowlisted config without credential-bearing fields."""

    if not isinstance(value, dict):
        return {}
    result: dict[str, Any] = {}
    nested = nested or {}
    for key in sorted(allowed):
        if key not in value:
            continue
        child = value[key]
        if key in nested and isinstance(child, dict):
            result[key] = _safe_config_projection(child, allowed=nested[key])
        elif child is None or isinstance(child, (bool, int, float, str)):
            result[key] = child
    return result


def _retrieval_effective_dataset_config(dataset: dict[str, Any]) -> dict[str, Any]:
    """Project only non-secret Dataset settings that can alter retrieval evidence."""

    index_config = _ensure_dict(dataset.get("index_config"))
    embedding = _safe_config_projection(
        _ensure_dict(dataset.get("embedding_config")),
        allowed=_EMBEDDING_FINGERPRINT_FIELDS,
    )
    safe_base_url = _secret_free_base_url(embedding.get("base_url"))
    if safe_base_url is None:
        embedding.pop("base_url", None)
    else:
        embedding["base_url"] = safe_base_url
    retrieval = _ensure_dict(index_config.get("retrieval"))
    safe_retrieval = _safe_config_projection(
        retrieval,
        allowed=_RETRIEVAL_FINGERPRINT_FIELDS,
        nested=_RETRIEVAL_NESTED_FINGERPRINT_FIELDS,
    )
    fusion = _ensure_dict(retrieval.get("fusion"))
    nested_rrf_weights = _safe_config_projection(
        _ensure_dict(fusion.get("rrf_weights")),
        allowed=_RETRIEVAL_NESTED_FINGERPRINT_FIELDS["rrf_weights"],
    )
    if nested_rrf_weights:
        safe_retrieval.setdefault("fusion", {})["rrf_weights"] = nested_rrf_weights
    try:
        lexical = LexicalConfig.from_index_config(index_config)
    except LexicalConfigError:
        # Invalid legacy rows must still produce a stable, secret-free
        # fingerprint instead of crashing catalog reads.
        safe_retrieval["lexical"] = {"invalid": True}
    else:
        if lexical.configured:
            safe_retrieval["lexical"] = {
                "active_version": lexical.active_version,
                "bm25_v2": {
                    **lexical.bm25_v2.to_dict(),
                    "shadow_write_enabled": lexical.bm25_v2_shadow_write_enabled,
                    "schema_fingerprint": lexical.bm25_v2.fingerprint,
                    "filtering": {
                        **lexical.filtering.to_dict(),
                        "profile_fingerprint": lexical.filtering.fingerprint,
                    },
                },
            }
    return {
        "retrieval": safe_retrieval,
        "multimodal_enabled": bool(
            index_config.get("multimodal_enabled") or index_config.get("enable_multimodal")
        ),
        "embedding": embedding,
    }


def _dataset_revision_fingerprint(
    dataset: dict[str, Any],
) -> str | None:
    """Hash the authoritative content revision plus retrieval-effective config."""

    revision = dataset.get("content_revision")
    if not isinstance(revision, int) or revision < 0:
        return None
    payload = {
        "dataset_id": str(dataset.get("dataset_id") or ""),
        "content_revision": revision,
        "embedding_provider": dataset.get("embedding_provider"),
        "embedding_model": dataset.get("embedding_model"),
        "embedding_dimension": dataset.get("embedding_dimension"),
        "needs_reindex": dataset.get("needs_reindex"),
        "collection_name": dataset.get("collection_name"),
        "retrieval_effective_config": _retrieval_effective_dataset_config(dataset),
    }
    canonical = json.dumps(
        payload,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    )
    return "sha256:" + hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def _lexical_transition_identity(dataset: dict[str, Any] | None) -> str:
    """Canonical fields whose concurrent change invalidates a transition."""

    value = dataset or {}
    return json.dumps(
        {
            "index_config": value.get("index_config") or {},
            "collection_name": value.get("collection_name") or "",
            "embedding_dimension": int(value.get("embedding_dimension") or 0),
            "tenant_id": value.get("tenant_id") or "",
            "content_revision": value.get("content_revision"),
        },
        sort_keys=True,
        separators=(",", ":"),
    )


def _dataset_config_projection(dataset: dict[str, Any]) -> dict[str, Any]:
    """Fields that must remain stable during a retrieval-config patch."""

    return {
        "embedding_provider": dataset.get("embedding_provider"),
        "embedding_model": dataset.get("embedding_model"),
        "embedding_dimension": dataset.get("embedding_dimension"),
        "embedding_config": _ensure_dict(dataset.get("embedding_config")),
        "index_config": _ensure_dict(dataset.get("index_config")),
        "collection_name": dataset.get("collection_name"),
    }


def _ingestion_index_config(index_config: Any) -> dict[str, Any]:
    """Project only dataset fields that change stored index generations.

    Query-time retrieval tuning (including shadow-only lexical selection) is
    reconciled separately and does not change chunk boundaries or dense vector
    identity.
    """

    return {
        key: value
        for key, value in _ensure_dict(index_config).items()
        if key not in NON_INGESTION_INDEX_CONFIG_KEYS
    }


def _require_not_guest(user: UserContext) -> None:
    if not user.is_authenticated or "guest" in (user.roles or []):
        raise PermissionDeniedError("Authentication required")


def _dataset_permission_short_circuit(
    dataset: dict[str, Any], user: UserContext, *, allow_archived_owner: bool = False,
) -> tuple[bool, str | None]:
    """Resolve decisions that never read grants; False means a grant read is needed."""
    if dataset.get("is_archived") and not allow_archived_owner:
        return True, None
    if not user.is_authenticated:
        return True, "viewer" if str(dataset.get("visibility") or "").lower() == "public" else None
    dataset_tenant_id = str(dataset.get("tenant_id") or "").strip()
    user_tenant_id = str(user.tenant_id or "").strip()
    same_tenant = bool(dataset_tenant_id and user_tenant_id and dataset_tenant_id == user_tenant_id)
    if same_tenant and (user.tier == "admin" or "admin" in (user.roles or [])):
        return True, "owner"
    created_by = str(dataset.get("created_by") or "")
    if created_by and created_by == user.user_id:
        return True, "owner"
    return False, None


class DatasetService:
    """Service for managing knowledge base datasets.

    Accepts a ``_ks`` (parent KnowledgeService) reference for shared resources
    like ``vector_store`` and ``embedding_manager``.  Set post-init by the
    parent because vector_store is created after sub-service construction.
    """

    _ks: KnowledgeService | None

    def __init__(self, settings: Settings, database: DatabaseStorage):
        self.settings = settings
        self.db = database
        self._ks = None  # Set post-init by KnowledgeService
        # Retain per-dataset serialization while a request/waiter owns a lock,
        # without permanently retaining attacker-controlled dataset IDs.
        self._transition_locks: weakref.WeakValueDictionary[str, asyncio.Lock] = (
            weakref.WeakValueDictionary()
        )

    # ========================================================================
    # Dataset CRUD
    # ========================================================================

    async def list_datasets(self, user: UserContext) -> list[dict[str, Any]]:
        datasets = await self.db.list_datasets(
            tenant_id=user.tenant_id, include_public=True, limit=200, offset=0
        )
        return await self._visible_dataset_rows(user, datasets)

    async def list_datasets_page(
        self, user: UserContext, *, limit: int = 200, cursor: str | None = None,
        archived_only: bool = False,
    ) -> dict[str, Any]:
        if archived_only:
            _require_not_guest(user)
        if type(limit) is not int or not 1 <= limit <= 200:
            raise ValidationFailedError("dataset page limit must be in 1..200")
        kwargs: dict[str, Any] = {}
        if cursor is not None:
            try:
                if len(cursor) > 1024:
                    raise ValueError
                created_at, dataset_id = decode_query_cursor(cursor)
                if len(dataset_id) > 255:
                    raise ValueError
                kwargs = {"before_created_at": created_at, "before_dataset_id": dataset_id}
            except (ValueError, RecursionError) as exc:
                raise ValidationFailedError("invalid dataset pagination cursor") from exc
        if archived_only:
            kwargs["archived_only"] = True
        rows = await self.db.list_datasets(
            tenant_id=user.tenant_id, include_public=not archived_only,
            limit=limit + 1, offset=0, **kwargs,
        )
        page = rows[:limit]
        next_cursor = None
        if len(rows) > limit:
            last = page[-1]
            created_at = last["created_at"]
            if isinstance(created_at, str):
                created_at = datetime.fromisoformat(created_at)
            next_cursor = encode_query_cursor(created_at, str(last["dataset_id"]))
        # Advance on the scanned row, including pages whose rows ACL filters out.
        return {
            "items": await self._visible_dataset_rows(
                user, page, required="owner" if archived_only else "viewer",
                batch_permissions=True,
            ),
            "next_cursor": next_cursor,
        }

    async def _visible_dataset_rows(
        self, user: UserContext, datasets: list[dict[str, Any]],
        *, required: str = "viewer", batch_permissions: bool = False,
    ) -> list[dict[str, Any]]:
        permissions = None
        if batch_permissions:
            # A catalog page observes grants in one statement snapshot. Resource
            # operations keep their independent current-authorization reads.
            if len(datasets) > 200:
                raise ValidationFailedError("dataset permission page exceeds 200 rows")
            needing_grants = [ds for ds in datasets if not _dataset_permission_short_circuit(
                ds, user, allow_archived_owner=required == "owner",
            )[0]]
            permissions = {}
            if needing_grants:
                tenant = str(user.tenant_id or "").strip()
                role_ids = [ds["dataset_id"] for ds in needing_grants
                            if tenant and str(ds.get("tenant_id") or "").strip() == tenant]
                permissions = await self.db.get_dataset_permissions_batch(
                    [ds["dataset_id"] for ds in needing_grants],
                    user_id=user.user_id, role_dataset_ids=role_ids, roles=user.roles or [],
                )
        visible: list[dict[str, Any]] = []
        for ds in datasets:
            kwargs = {} if permissions is None else {"grants": permissions.get(ds["dataset_id"], {})}
            perm = await self._effective_dataset_permission(
                ds, user, allow_archived_owner=required == "owner", **kwargs,
            )
            if _permission_rank(perm) >= _permission_rank(required):
                ds = dict(ds)
                ds["my_permission"] = perm
                visible.append(ds)

        if visible:
            dataset_ids = [ds["dataset_id"] for ds in visible]
            try:
                stats_batch = await self.db.get_datasets_statistics_batch(dataset_ids)
                for ds in visible:
                    ds_id = ds["dataset_id"]
                    ds_stats = stats_batch.get(ds_id, {})
                    ds["statistics"] = {
                        "document_count": ds_stats.get("document_count", 0),
                        "segment_count": ds_stats.get("segment_count", 0),
                        "available_document_count": ds_stats.get(
                            "available_document_count", 0
                        ),
                        "available_segment_count": ds_stats.get(
                            "available_segment_count", 0
                        ),
                        "word_count": ds_stats.get("word_count", 0),
                        "hit_count": ds_stats.get("hit_count", 0),
                    }
            except Exception as e:
                logger.warning(f"Failed to fetch batch statistics: {e}")
                for ds in visible:
                    ds["statistics"] = {
                        "document_count": 0,
                        "segment_count": 0,
                        "available_document_count": 0,
                        "available_segment_count": 0,
                        "word_count": 0,
                        "hit_count": 0,
                    }

            for ds in visible:
                fingerprint = _dataset_revision_fingerprint(
                    ds,
                )
                if fingerprint is not None:
                    ds["revision_fingerprint"] = fingerprint

        return [self._redact_dataset_secrets(ds) for ds in visible]

    async def preview_chunking(
        self, user: UserContext, dataset_id: str, text: str, config: dict[str, Any] | None = None
    ) -> list[dict[str, Any]]:
        if config is not None:
            validate_persisted_chunking_config(config)
        if dataset_id != "temp_preview":
            await self.require_dataset_access(user, dataset_id, required="viewer")

        chunking_config: ChunkingConfig
        if config:
            chunking_config = ChunkingConfig.from_dict(config)
        else:
            if dataset_id == "temp_preview":
                chunking_config = ChunkingConfig()
            else:
                dataset = await self._get_dataset_or_404(dataset_id)
                index_config = _ensure_dict(dataset.get("index_config"))
                chunking_config = ChunkingConfig.from_dict(index_config.get("chunking", {}))

        doc_id = f"preview_{uuid.uuid4().hex[:8]}"
        chunks = await asyncio.to_thread(process_document, text, chunking_config, doc_id)
        flat_chunks = flatten_chunks(chunks)

        return [
            {
                "content": c.text,
                "token_count": c.token_count,
                "char_count": c.char_count,
                "metadata": c.metadata,
            }
            for c in flat_chunks
        ]

    async def create_dataset(self, user: UserContext, data: dict[str, Any]) -> dict[str, Any]:
        _require_not_guest(user)

        _require_valid_embedding_dimension(data)
        _require_server_owned_dataset_embedding_config(data.get("embedding_config"))
        _require_server_owned_dataset_rerank_config(data.get("index_config"))
        _require_bounded_persisted_retrieval_config(data.get("index_config"))
        _require_safe_persisted_chunking_config(data.get("index_config"))
        resolve_parsing_config(_ensure_dict(data.get("index_config")))
        _require_multimodal_dataset_disabled(data)

        dataset_id = str(data.get("dataset_id") or "").strip()
        if not dataset_id:
            dataset_id = f"kb_{uuid.uuid4().hex[:12]}"

        # ``dataset_id`` is a public API input for backward compatibility, but
        # creation must never turn into an update of another tenant's dataset.
        if await self.db.dataset_exists(dataset_id):
            raise ValidationFailedError("dataset_id already exists")

        embedding_provider = str(data.get("embedding_provider") or "dashscope")
        embedding_model = str(data.get("embedding_model") or "text-embedding-v4")
        embedding_dimension = int(data.get("embedding_dimension") or 1024)

        collection_name = str(data.get("collection_name") or "").strip() or None
        visibility = str(data.get("visibility") or "private")

        embedding_config = _ensure_dict(data.get("embedding_config"))
        index_config = _ensure_dict(data.get("index_config"))
        if index_config_has_reserved_deletion_fence(data.get("index_config")):
            raise ValidationFailedError(
                "index_config contains a reserved lifecycle field"
            )
        try:
            lexical_config = LexicalConfig.from_index_config(index_config)
        except LexicalConfigError as exc:
            raise ValidationFailedError(str(exc)) from exc
        if lexical_config.reads_bm25_v2:
            raise ValidationFailedError(
                "bm25_v2 active cutover is hard-disabled; use lexical_v1 with "
                "bm25_v2 shadow writes"
            )
        from .embedding import BaseEmbedding, create_embedding

        embedder: BaseEmbedding | None = None
        dim: int = 0
        collection: str = ""
        try:
            econf = await maybe_await(
                self._ks._resolve_embedding_config(
                    provider=embedding_provider,
                    model=embedding_model,
                    embedding_config=embedding_config,
                    tenant_id=str(user.tenant_id or ""),
                )
            )
            embedder = create_embedding(econf, dimension=embedding_dimension)
            if embedder._dimension is None:
                await asyncio.wait_for(
                    embedder.embed_query("test"),
                    timeout=float(econf.timeout_seconds) + 5.0,
                )
            dim = embedder._dimension or 1024
            desired_collection = self._ks.vector_store.make_collection_name(
                dataset_id=dataset_id,
                dimension=dim,
                collection_name=collection_name,
            )
            if await self.db.collection_name_in_use(desired_collection):
                raise ValidationFailedError("collection_name already in use")
            collection = await self._ks.vector_store.ensure_collection(
                dataset_id=dataset_id,
                dimension=dim,
                collection_name=collection_name,
                allow_existing=False,
                bootstrap_unbound_dataset=True,
                tenant_id=user.tenant_id or "",
                allow_lexical_transition=lexical_config.configured,
                **(
                    {"lexical_config": lexical_config}
                    if lexical_config.configured
                    else {}
                ),
            )
        except Exception as exc:
            raise ValidationFailedError(f"Failed to create dataset index: {exc}") from exc
        finally:
            if embedder:
                await embedder.close()

        dataset = {
            "dataset_id": dataset_id,
            "name": str(data.get("name") or dataset_id),
            "description": str(data.get("description") or ""),
            "tenant_id": user.tenant_id or "",
            "visibility": visibility,
            "embedding_provider": embedding_provider,
            "embedding_model": embedding_model,
            "embedding_dimension": dim,
            "embedding_config": embedding_config,
            "index_config": index_config,
            "collection_name": collection,
            "created_by": user.user_id,
        }

        try:
            created = await self.db.create_dataset_with_owner(dataset, user.user_id)
        except Exception:
            await self._delete_unclaimed_collection(collection)
            raise
        if not created:
            # A concurrent request may have claimed the same ID or collection
            # after the preflight checks. Never fall back to the update/upsert
            # path, and remove the collection only when no dataset owns it.
            await self._delete_unclaimed_collection(collection)
            raise ValidationFailedError("dataset_id already exists")
        return self._redact_dataset_secrets(await self._get_dataset_or_404(dataset_id))

    async def update_dataset(
        self, user: UserContext, dataset_id: str, patch: dict[str, Any]
    ) -> dict[str, Any]:
        lock = self._transition_locks.setdefault(dataset_id, asyncio.Lock())
        async with lock:
            return await self._update_dataset_locked(user, dataset_id, patch)

    async def _update_dataset_locked(
        self, user: UserContext, dataset_id: str, patch: dict[str, Any]
    ) -> dict[str, Any]:
        _require_valid_embedding_dimension(patch)
        if "embedding_config" in (patch or {}):
            _require_server_owned_dataset_embedding_config(
                patch.get("embedding_config")
            )
        if "index_config" in (patch or {}):
            _require_server_owned_dataset_rerank_config(patch.get("index_config"))
            _require_bounded_persisted_retrieval_config(patch.get("index_config"))
            _require_safe_persisted_chunking_config(patch.get("index_config"))
            resolve_parsing_config(_ensure_dict(patch.get("index_config")))
        dataset = await self.require_dataset_access(user, dataset_id, required="owner")
        try:
            deletion_fence = dataset_index_deletion_fence(dataset)
        except RuntimeError as exc:
            raise ValidationFailedError(str(exc)) from exc
        if deletion_fence is not None:
            raise ValidationFailedError(
                "dataset index deletion is pending; dataset updates are unavailable"
            )

        mutable = {
            "name",
            "description",
            "visibility",
            "embedding_provider",
            "embedding_model",
            "embedding_dimension",
            "embedding_config",
            "index_config",
        }
        filtered = {k: v for k, v in (patch or {}).items() if k in mutable}
        if "index_config" in filtered and index_config_has_reserved_deletion_fence(
            filtered["index_config"]
        ):
            raise ValidationFailedError(
                "index_config contains a reserved lifecycle field"
            )
        if not filtered:
            return dataset

        embedding_keys = {
            "embedding_provider",
            "embedding_model",
            "embedding_dimension",
            "embedding_config",
        }
        updated = dict(dataset)
        updated.update(filtered)
        if {
            "embedding_provider",
            "embedding_model",
            "embedding_config",
            "index_config",
        }.intersection(filtered):
            _require_multimodal_dataset_disabled(updated)
        embedding_changed = any(
            (
                _ensure_dict(updated.get(field))
                != _ensure_dict(dataset.get(field))
                if field == "embedding_config"
                else updated.get(field) != dataset.get(field)
            )
            for field in embedding_keys
        )
        indexing_config_changed = (
            "index_config" in filtered
            and _ingestion_index_config(updated.get("index_config"))
            != _ingestion_index_config(dataset.get("index_config"))
        )
        if embedding_changed or indexing_config_changed:
            docs = await self.db.list_documents(dataset_id=dataset_id, limit=1, offset=0)
            if docs:
                embedding_identity_changed = any(
                    updated.get(field) != dataset.get(field)
                    for field in (
                        "embedding_provider",
                        "embedding_model",
                        "embedding_dimension",
                    )
                )
                if embedding_identity_changed:
                    # T3: a model/dimension change is not an in-place edit once
                    # vectors exist — it is a blue-green migration. Point the
                    # operator at the migration endpoint instead of a dead end.
                    raise ValidationFailedError(
                        "Cannot change embedding or ingestion index identity when "
                        "documents exist; an embedding provider/model/dimension "
                        "change must go through a blue-green migration — POST "
                        f"/api/v1/knowledge/datasets/{dataset_id}"
                        "/embedding-migration/start"
                    )
                raise ValidationFailedError(
                    "Cannot change embedding or ingestion index identity when documents "
                    "exist; create a reindexed generation"
                )

        try:
            lexical_config = LexicalConfig.from_index_config(
                _ensure_dict(updated.get("index_config"))
            )
        except LexicalConfigError as exc:
            raise ValidationFailedError(str(exc)) from exc
        if lexical_config.reads_bm25_v2:
            raise ValidationFailedError(
                "bm25_v2 active cutover is hard-disabled; use lexical_v1 with "
                "bm25_v2 shadow writes"
            )
        if embedding_keys.intersection(filtered.keys()):
            from .embedding import BaseEmbedding, create_embedding

            embedder: BaseEmbedding | None = None
            dim: int = 0
            try:
                econf = await maybe_await(
                    self._ks._resolve_embedding_config(
                        provider=str(updated.get("embedding_provider") or "local"),
                        model=str(updated.get("embedding_model") or "hash-384"),
                        embedding_config=_ensure_dict(updated.get("embedding_config")),
                        tenant_id=str(updated.get("tenant_id") or ""),
                    )
                )
                embedder = create_embedding(
                    econf, dimension=int(updated.get("embedding_dimension") or 0) or None
                )
                if embedder._dimension is None:
                    await asyncio.wait_for(
                        embedder.embed_query("test"),
                        timeout=float(econf.timeout_seconds) + 5.0,
                    )
                dim = embedder._dimension or 1024
                collection = await self._ks.vector_store.ensure_collection(
                    dataset_id=dataset_id,
                    dimension=dim,
                    collection_name=str(updated.get("collection_name") or "") or None,
                    tenant_id=str(updated.get("tenant_id") or ""),
                    **(
                        {"lexical_config": lexical_config}
                        if lexical_config.configured
                        else {}
                    ),
                )
                updated["embedding_dimension"] = dim
                updated["collection_name"] = collection
            except Exception as exc:
                raise ValidationFailedError(
                    f"Failed to update dataset embedding/index: {exc}"
                ) from exc
            finally:
                if embedder:
                    await embedder.close()

        if "index_config" in filtered and not embedding_keys.intersection(filtered):
            collection_name = str(updated.get("collection_name") or "")
            dimension = int(updated.get("embedding_dimension") or 0)
            if not collection_name or dimension <= 0:
                raise ValidationFailedError(
                    "Dataset collection/dimension is missing; re-index before lexical rollout"
                )
            try:
                old_lexical = LexicalConfig.from_index_config(
                    _ensure_dict(dataset.get("index_config"))
                )
                if lexical_config.writes_bm25_v2 and not old_lexical.writes_bm25_v2:
                    # Prepare only the dormant shadow schema before the DB
                    # commit. Active selection and strict mode are published
                    # only by post-commit authoritative reconciliation.
                    shadow_preflight = lexical_config.with_runtime_selection(
                        active_version=LEXICAL_V1,
                        shadow_write_enabled=True,
                        filtering=lexical_config.filtering,
                    )
                    await self._ks.vector_store.ensure_collection(
                        dataset_id=dataset_id,
                        dimension=dimension,
                        collection_name=collection_name,
                        lexical_config=shadow_preflight,
                        tenant_id=str(updated.get("tenant_id") or ""),
                        allow_lexical_transition=True,
                    )
            except Exception as exc:
                raise ValidationFailedError(
                    f"Failed to prepare dataset lexical shadow index: {exc}"
                ) from exc

        persisted_fields = {field: updated.get(field) for field in filtered}
        if embedding_keys.intersection(filtered):
            persisted_fields["embedding_dimension"] = updated.get(
                "embedding_dimension"
            )
            persisted_fields["collection_name"] = updated.get("collection_name")

        changed_config_fields = _DATASET_CONFIG_FIELDS.intersection(
            persisted_fields
        )
        identity_changed = (
            dataset_ingestion_identity(updated)
            != dataset_ingestion_identity(dataset)
        )
        patch_kwargs: dict[str, Any] = {
            "expected_config": (
                _dataset_config_projection(dataset)
                if changed_config_fields
                else None
            )
        }
        if identity_changed:
            patch_kwargs["require_no_documents"] = True
        try:
            saved = await self.db.patch_dataset_fields(
                dataset_id,
                persisted_fields,
                **patch_kwargs,
            )
        except Exception:
            if "index_config" in filtered:
                # Schema preparation may have added a dormant v2 field. Restore
                # the DB-authoritative runtime selection; immutable shadow
                # schema/data are intentionally retained.
                try:
                    await self._reconcile_lexical_selection(dataset_id)
                except Exception:
                    logger.exception(
                        "Failed to reconcile lexical selection after dataset save failure"
                    )
            raise

        if saved is None:
            if "index_config" in filtered or embedding_keys.intersection(filtered):
                try:
                    await self._reconcile_lexical_selection(dataset_id)
                except Exception:
                    logger.exception(
                        "Failed to reconcile lexical selection after concurrent dataset update"
                    )
                candidate_collection = str(updated.get("collection_name") or "")
                if candidate_collection != str(dataset.get("collection_name") or ""):
                    await self._delete_unclaimed_collection(candidate_collection)
            raise ValidationFailedError(
                "dataset configuration changed concurrently; retry"
            )

        if "index_config" in filtered:
            try:
                await self._reconcile_lexical_selection(dataset_id)
            except Exception as exc:
                compensation_error: Exception | None = None
                try:
                    rollback_changes = {
                        field: dataset.get(field)
                        for field in changed_config_fields
                    }
                    rolled_back = await self.db.patch_dataset_fields(
                        dataset_id,
                        rollback_changes,
                        expected_config=_dataset_config_projection(saved),
                        **(
                            {"require_no_documents": True}
                            if identity_changed
                            else {}
                        ),
                    )
                    if rolled_back is None:
                        # Another configuration update won. Never overwrite it
                        # with this request's stale rollback; converge Qdrant
                        # to the latest authoritative row.
                        await self._reconcile_lexical_selection(dataset_id)
                    else:
                        await self._reconcile_lexical_selection(dataset_id)
                        candidate_collection = str(
                            updated.get("collection_name") or ""
                        )
                        if candidate_collection != str(
                            dataset.get("collection_name") or ""
                        ):
                            await self._delete_unclaimed_collection(
                                candidate_collection
                            )
                except Exception as rollback_exc:
                    compensation_error = rollback_exc
                if compensation_error is not None:
                    raise ValidationFailedError(
                        "Lexical runtime reconciliation failed and compensating "
                        "rollback did not converge; bm25_v2 remains fail-closed: "
                        f"{type(compensation_error).__name__}"
                    ) from exc
                raise ValidationFailedError(
                    "Lexical runtime reconciliation failed; dataset selection was "
                    "compensated to its previous configuration"
                ) from exc
        return self._redact_dataset_secrets(await self._get_dataset_or_404(dataset_id))

    async def _reconcile_lexical_selection(self, dataset_id: str) -> None:
        """Converge Qdrant to the latest DB selection under concurrent updates."""

        for _attempt in range(3):
            authoritative = await self.db.get_dataset(dataset_id)
            if not authoritative:
                raise ValidationFailedError("Dataset disappeared during lexical transition")
            try:
                config = LexicalConfig.from_index_config(
                    _ensure_dict(authoritative.get("index_config"))
                )
            except LexicalConfigError as exc:
                raise ValidationFailedError(str(exc)) from exc
            collection_name = str(authoritative.get("collection_name") or "")
            dimension = int(authoritative.get("embedding_dimension") or 0)
            if not collection_name or dimension <= 0:
                raise ValidationFailedError(
                    "Dataset collection/dimension is missing during lexical reconciliation"
                )
            identity_before = _lexical_transition_identity(authoritative)
            await self._ks.vector_store.ensure_collection(
                dataset_id=dataset_id,
                dimension=dimension,
                collection_name=collection_name,
                lexical_config=config,
                tenant_id=str(authoritative.get("tenant_id") or ""),
                allow_lexical_transition=True,
                authority_content_revision=authoritative.get("content_revision"),
            )
            observed = await self.db.get_dataset(dataset_id)
            identity_after = _lexical_transition_identity(observed)
            if identity_before == identity_after:
                return
        raise ValidationFailedError(
            "Concurrent lexical updates did not converge after three reconciliations"
        )

    async def set_dataset_archived(
        self,
        user: UserContext,
        dataset_id: str,
        *,
        archived: bool,
        reason: str | None = None,
    ) -> dict[str, Any]:
        """Owner-only reversible visibility transition for an existing dataset."""

        _require_not_guest(user)
        if type(archived) is not bool:
            raise ValidationFailedError("archived must be a boolean")
        normalized_reason = str(reason or "").strip() or None
        if normalized_reason and len(normalized_reason) > 500:
            raise ValidationFailedError("archive reason exceeds 500 characters")
        dataset = await self.db.get_dataset(dataset_id, include_archived=True)
        if not dataset:
            raise ValidationFailedError("dataset not found")
        permission = await self._effective_dataset_permission(
            dataset, user, allow_archived_owner=True,
        )
        if permission != "owner":
            raise PermissionDeniedError("Dataset owner permission required")
        lease_factory = getattr(self.db, "dataset_index_delete_lease", None)
        if not callable(lease_factory):
            raise ValidationFailedError("dataset archive index lease is unavailable")
        try:
            async with lease_factory(dataset_id) as lease_connection:
                updated = await self.db.set_dataset_archived(
                    dataset_id,
                    archived=archived,
                    user_id=user.user_id,
                    tenant_id=user.tenant_id,
                    roles=list(user.roles or []),
                    tenant_admin=(user.tier == "admin" or "admin" in (user.roles or [])),
                    reason=normalized_reason,
                    connection=lease_connection,
                )
        except IndexLeaseUnavailableError:
            raise
        except PermissionError as exc:
            raise PermissionDeniedError("Dataset owner permission required") from exc
        except RuntimeError as exc:
            raise ValidationFailedError(str(exc)) from exc
        if not updated:
            raise ValidationFailedError("dataset not found")
        result = self._redact_dataset_secrets(updated)
        result["my_permission"] = "owner"
        return result

    async def delete_dataset(
        self,
        user: UserContext,
        dataset_id: str,
        *,
        deletion_confirmed: bool = False,
        reason: str | None = None,
    ) -> bool:
        dataset = await self.require_dataset_access(user, dataset_id, required="owner")
        deletion_target = make_dataset_index_deletion_fence(
            "dataset_delete",
            dataset_id,
        )
        try:
            deletion_fence = dataset_index_deletion_fence(dataset)
        except RuntimeError as exc:
            raise ValidationFailedError(str(exc)) from exc
        if deletion_fence is not None and deletion_fence != deletion_target:
            raise ValidationFailedError(
                "another dataset index deletion target is already pending"
            )

        if not bool(getattr(user, "is_authenticated", False)):
            raise PermissionDeniedError("Authentication required")

        if deletion_confirmed is not True:
            raise PermissionDeniedError("Verified Gateway deletion confirmation required")

        lease_factory = getattr(self.db, "dataset_index_delete_lease", None)
        set_fence = getattr(self.db, "set_dataset_index_deletion_fence", None)
        delete_collections = getattr(
            self._ks.vector_store,
            "delete_dataset_collections",
            None,
        )
        list_document_ids = getattr(
            self.db,
            "list_document_ids_by_dataset",
            None,
        )
        storage = getattr(self._ks, "image_storage_service", None)
        delete_document_assets = getattr(storage, "delete_document_assets", None)
        if not all(
            callable(value)
            for value in (
                lease_factory,
                set_fence,
                delete_collections,
                list_document_ids,
                delete_document_assets,
            )
        ):
            raise ValidationFailedError(
                "dataset deletion is unavailable without the index lifecycle fence"
            )

        async with lease_factory(dataset_id) as lease_connection:
            try:
                authoritative, _marker_created = await set_fence(
                    dataset_id,
                    operation="dataset_delete",
                    target_id=dataset_id,
                    connection=lease_connection,
                )
            except RuntimeError as exc:
                raise ValidationFailedError(str(exc)) from exc
            tenant_id = str(authoritative.get("tenant_id") or "").strip()
            if not tenant_id or tenant_id != str(dataset.get("tenant_id") or "").strip():
                raise ValidationFailedError("dataset tenant identity changed during deletion")
            document_ids = await list_document_ids(
                dataset_id,
                connection=lease_connection,
            )
            for document_id in document_ids:
                await delete_document_assets(
                    tenant_id=tenant_id,
                    document_id=str(document_id),
                )
            collection = str(authoritative.get("collection_name") or "").strip()
            await delete_collections(
                tenant_id=tenant_id,
                dataset_id=dataset_id,
                authoritative_collection_names=[collection] if collection else [],
                lifecycle_lease_held=True,
            )
            deleted = await self.db.delete_dataset(
                dataset_id,
                deleted_by=user.user_id,
                delete_reason=reason,
                connection=lease_connection,
            )
            if not deleted:
                raise ValidationFailedError(
                    "dataset database deletion failed; index deletion fence remains"
                )
        if deleted:
            try:
                await self.db.log_audit(
                    event_type="knowledge.dataset",
                    action="delete",
                    status="success",
                    user_id=user.user_id,
                    tenant_id=user.tenant_id,
                    resource_type="dataset",
                    resource_id=dataset_id,
                    request_summary={
                        "delete_mode": "soft",
                        "reason": reason or "",
                    },
                )
            except Exception:
                logger.warning(
                    "Failed to write audit log for dataset deletion %s",
                    dataset_id,
                    exc_info=True,
                )
        return deleted

    # ========================================================================
    # Query observability and feedback
    # ========================================================================

    async def list_query_history(
        self,
        user: UserContext,
        dataset_id: str,
        *,
        limit: int = 50,
        zero_results: bool | None = None,
        mode: str | None = None,
        cursor: str | None = None,
    ) -> dict[str, Any]:
        _require_not_guest(user)
        dataset = await self.require_dataset_access(user, dataset_id, required="editor")
        normalized_mode = str(mode or "").strip().lower() or None
        if normalized_mode not in {None, "dense", "bm25", "hybrid", "keyword", "vector"}:
            raise ValidationFailedError("unsupported query-history mode")
        cursor_created_at = None
        cursor_id = None
        if cursor:
            try:
                cursor_created_at, cursor_id = decode_query_cursor(cursor)
            except ValueError as exc:
                raise ValidationFailedError(str(exc)) from exc
        page_limit = max(1, min(int(limit), 100))
        rows = await self.db.list_dataset_queries(
            dataset_id=dataset_id,
            tenant_id=str(dataset.get("tenant_id") or ""),
            limit=page_limit + 1,
            zero_results=zero_results,
            mode=normalized_mode,
            cursor_created_at=cursor_created_at,
            cursor_id=cursor_id,
        )
        has_more = len(rows) > page_limit
        items = rows[:page_limit]
        next_cursor = None
        if has_more and items:
            next_cursor = encode_query_cursor(items[-1]["created_at"], str(items[-1]["id"]))
        return {"queries": items, "next_cursor": next_cursor, "has_more": has_more}

    async def upsert_query_feedback(
        self,
        user: UserContext,
        dataset_id: str,
        *,
        trace_id: str,
        query_fingerprint: str,
        target_type: str,
        target_id: str,
        rating: str,
        reason_code: str,
        comment: str | None,
    ) -> dict[str, Any]:
        _require_not_guest(user)
        dataset = await self.require_dataset_access(user, dataset_id, required="viewer")
        tenant_id = str(dataset.get("tenant_id") or "").strip()
        persisted_fingerprint = await self.db.get_dataset_query_fingerprint(
            dataset_id=dataset_id,
            tenant_id=tenant_id,
            trace_id=trace_id,
        )
        if persisted_fingerprint and persisted_fingerprint != query_fingerprint:
            raise QueryObservationConflictError(
                "query fingerprint does not match the persisted trace"
            )
        feedback = await self.db.upsert_dataset_query_feedback(
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            trace_id=trace_id,
            query_fingerprint=query_fingerprint,
            target_type=target_type,
            target_id=target_id,
            rating=rating,
            reason_code=reason_code,
            comment=str(comment or "").strip() or None,
            created_by=str(user.user_id),
        )
        if not feedback:
            raise RuntimeError("query feedback persistence is unavailable")
        return feedback

    async def list_query_feedback(
        self,
        user: UserContext,
        dataset_id: str,
        *,
        limit: int = 50,
        rating: str | None = None,
        reason_code: str | None = None,
        target_type: str | None = None,
        trace_id: str | None = None,
        cursor: str | None = None,
    ) -> dict[str, Any]:
        _require_not_guest(user)
        dataset = await self.require_dataset_access(user, dataset_id, required="editor")
        cursor_created_at = None
        cursor_id = None
        if cursor:
            try:
                cursor_created_at, cursor_id = decode_query_cursor(cursor)
            except ValueError as exc:
                raise ValidationFailedError(str(exc)) from exc
        page_limit = max(1, min(int(limit), 100))
        rows = await self.db.list_dataset_query_feedback(
            tenant_id=str(dataset.get("tenant_id") or ""),
            dataset_id=dataset_id,
            limit=page_limit + 1,
            rating=rating,
            reason_code=reason_code,
            target_type=target_type,
            trace_id=trace_id,
            cursor_created_at=cursor_created_at,
            cursor_id=cursor_id,
        )
        has_more = len(rows) > page_limit
        items = rows[:page_limit]
        next_cursor = None
        if has_more and items:
            next_cursor = encode_query_cursor(
                items[-1]["created_at"], str(items[-1]["feedback_id"])
            )
        return {"feedback": items, "next_cursor": next_cursor, "has_more": has_more}

    # ========================================================================
    # Permissions
    # ========================================================================

    async def list_dataset_permissions(
        self, user: UserContext, dataset_id: str
    ) -> list[dict[str, Any]]:
        await self.require_dataset_access(user, dataset_id, required="owner")
        return await self.db.list_dataset_permissions(dataset_id)

    async def grant_dataset_permission(
        self,
        user: UserContext,
        dataset_id: str,
        subject_type: str,
        subject_id: str,
        permission: str,
    ) -> None:
        await self.require_dataset_access(user, dataset_id, required="owner")
        if subject_type not in {"user", "role"}:
            raise ValidationFailedError("subject_type must be user or role")
        if permission not in {"owner", "editor", "viewer"}:
            raise ValidationFailedError("permission must be owner/editor/viewer")
        await self.db.grant_dataset_permission(dataset_id, subject_type, subject_id, permission)

    async def revoke_dataset_permission(
        self, user: UserContext, dataset_id: str, subject_type: str, subject_id: str
    ) -> bool:
        await self.require_dataset_access(user, dataset_id, required="owner")
        return await self.db.revoke_dataset_permission(dataset_id, subject_type, subject_id)

    # ========================================================================
    # Helpers (shared with KnowledgeService callers)
    # ========================================================================

    async def _get_dataset_or_404(self, dataset_id: str) -> dict[str, Any]:
        dataset = await self.db.get_dataset(dataset_id)
        if not dataset:
            raise ValidationFailedError("dataset not found")
        return dataset

    async def _delete_unclaimed_collection(self, collection_name: str) -> None:
        """Best-effort compensation after an insert-only dataset create fails."""
        if not collection_name:
            return
        try:
            if not await self.db.collection_name_in_use(collection_name):
                await self._ks.vector_store.delete_collection(collection_name)
        except Exception:
            # A cleanup failure must not hide the original database failure.
            # Retaining an unbound collection is safer than deleting one whose
            # ownership could not be verified.
            logger.warning(
                "Failed to clean up unclaimed vector collection %s",
                collection_name,
                exc_info=True,
            )

    async def _effective_dataset_permission(
        self, dataset: dict[str, Any], user: UserContext,
        *, allow_archived_owner: bool = False,
        grants: dict[tuple[str, str], dict[str, Any]] | None = None,
    ) -> str | None:
        resolved, permission = _dataset_permission_short_circuit(
            dataset, user, allow_archived_owner=allow_archived_owner,
        )
        if resolved:
            return permission
        dataset_tenant_id = str(dataset.get("tenant_id") or "").strip()
        user_tenant_id = str(user.tenant_id or "").strip()
        same_tenant = bool(
            dataset_tenant_id and user_tenant_id and dataset_tenant_id == user_tenant_id
        )

        rec = (grants.get(("user", user.user_id)) if grants is not None else
               await self.db.get_dataset_permission(dataset.get("dataset_id"), "user", user.user_id))
        best = str(rec.get("permission")) if rec else None

        for role in (user.roles or []) if same_tenant else []:
            r = (grants.get(("role", role)) if grants is not None else
                 await self.db.get_dataset_permission(dataset.get("dataset_id"), "role", role))
            p = str(r.get("permission")) if r else None
            if _permission_rank(p) > _permission_rank(best):
                best = p

        if best:
            return best

        visibility = str(dataset.get("visibility") or "private").lower()
        if visibility == "public":
            return "viewer"
        if visibility == "tenant" and same_tenant:
            return "viewer"

        return None

    async def require_dataset_access(
        self, user: UserContext, dataset_id: str, required: str = "viewer"
    ) -> dict[str, Any]:
        dataset = await self._get_dataset_or_404(dataset_id)
        perm = await self._effective_dataset_permission(dataset, user)
        if _permission_rank(perm) < _permission_rank(required):
            raise PermissionDeniedError(
                f"Missing dataset permission: {required} (current={perm or 'none'})"
            )
        return dataset

    async def authorize_datasets(
        self, user: UserContext, dataset_ids: list[str], required: str = "viewer"
    ) -> list[str]:
        """Batch ACL filter backing the gateway's agent-runtime authz call.

        PRD T8.2: the gateway resolves agent KB bindings through the KS
        internal authorize endpoint instead of reading KB tables directly.
        Returns the subset of ``dataset_ids`` this identity may access at the
        ``required`` level, preserving request order, de-duplicated. The
        effective-permission contract is exactly require_dataset_access's —
        one ACL source of truth for KB. Fail-closed: unknown, soft-deleted,
        or denied ids simply never appear in the result.
        """

        allowed: list[str] = []
        seen: set[str] = set()
        required_rank = _permission_rank(required)
        for raw_id in dataset_ids or []:
            dataset_id = str(raw_id or "").strip()
            if not dataset_id or dataset_id in seen:
                continue
            seen.add(dataset_id)
            dataset = await self.db.get_dataset(dataset_id)
            if not dataset:
                continue
            perm = await self._effective_dataset_permission(dataset, user)
            if _permission_rank(perm) >= required_rank:
                allowed.append(dataset_id)
        return allowed

    def _redact_dataset_secrets(self, dataset: dict[str, Any]) -> dict[str, Any]:
        ds = copy.deepcopy(dataset or {})
        ds["embedding_config"] = _redact_server_owned_dataset_config(
            _ensure_dict(ds.get("embedding_config"))
        )
        ds["index_config"] = _redact_server_owned_dataset_config(
            _ensure_dict(ds.get("index_config"))
        )
        return ds

    def sanitize_dataset_for_response(self, dataset: dict[str, Any]) -> dict[str, Any]:
        return self._redact_dataset_secrets(dataset)
