"""Versioned lexical retrieval configuration.

``lexical_v1`` is the existing hashed term-presence vector plus Qdrant IDF.
``bm25_v2`` is an opt-in shadow field backed by Qdrant's official BM25 model,
including term-frequency saturation and document-length normalization.
"""

from __future__ import annotations

import hashlib
import json
import math
from dataclasses import dataclass, replace
from importlib.metadata import PackageNotFoundError, version
from typing import Any

LEXICAL_V1 = "lexical_v1"
BM25_V2 = "bm25_v2"
LEXICAL_V1_FIELD = "bm25"
BM25_V2_FIELD = "bm25_v2"
BM25_V2_MODEL = "qdrant/bm25"
BM25_V2_ENCODER_IMPLEMENTATION = "qdrant-cluster-inference"
BM25_V2_ENCODER_CONTRACT_VERSION = "qdrant-bm25-document-v1"
# PostgreSQL is authoritative only for enabled L3 text segments in the
# dataset's immutable base collection. Image vectors and hierarchical L1/L2
# vectors may share a dataset (and, for same-dimension images, a collection),
# but they are not lexical backfill points.
BM25_V2_AUTHORITY_KIND = "postgres-segments-enabled-l3-text-base-v2"
COLLECTION_METADATA_KEY = "knowledge_lexical"
COLLECTION_SCOPE_METADATA_KEY = "knowledge_scope"
BM25_V2_BACKFILL_METADATA_KEY = "knowledge_bm25_v2_backfill"
BM25_V2_SCHEMA_FINGERPRINT_PAYLOAD_KEY = (
    "_lexical.bm25_v2_schema_fingerprint"
)
BM25_V2_FILTERING_FINGERPRINT_PAYLOAD_KEY = (
    "_lexical.filtering_profile_fingerprint"
)
REQUIRED_FILTER_PAYLOAD_INDEXES = ("tenant_id", "dataset_id")
STRICT_FILTER_PAYLOAD_INDEXES = (
    *REQUIRED_FILTER_PAYLOAD_INDEXES,
    "content_type",
    "level",
    "enabled",
    "document_id",
    "source_type",
    "language",
    BM25_V2_SCHEMA_FINGERPRINT_PAYLOAD_KEY,
    BM25_V2_FILTERING_FINGERPRINT_PAYLOAD_KEY,
)

try:
    QDRANT_CLIENT_VERSION = version("qdrant-client")
except PackageNotFoundError:  # pragma: no cover - import guard for tooling
    QDRANT_CLIENT_VERSION = "unavailable"

_ACTIVE_VERSIONS = frozenset({LEXICAL_V1, BM25_V2})
_TOKENIZERS = frozenset({"prefix", "whitespace", "word", "multilingual"})
_LANGUAGES = frozenset(
    {
        "none",
        "arabic",
        "azerbaijani",
        "basque",
        "bengali",
        "catalan",
        "chinese",
        "danish",
        "dutch",
        "english",
        "finnish",
        "french",
        "german",
        "greek",
        "hebrew",
        "hinglish",
        "hungarian",
        "indonesian",
        "italian",
        "japanese",
        "kazakh",
        "nepali",
        "norwegian",
        "portuguese",
        "romanian",
        "russian",
        "slovene",
        "spanish",
        "swedish",
        "tajik",
        "turkish",
    }
)
_BM25_V2_KEYS = frozenset(
    {
        "field",
        "model",
        "k",
        "b",
        "avg_len",
        "tokenizer",
        "language",
        "lowercase",
        "ascii_folding",
        "stopwords",
        "stemmer",
        "min_token_len",
        "max_token_len",
        "shadow_write_enabled",
        "filtering",
        "encoder_implementation",
        "encoder_contract_version",
        "qdrant_client_version",
    }
)


class LexicalConfigError(ValueError):
    """Raised when a dataset requests an unsafe or inconsistent lexical mode."""


def _finite_float(value: Any, *, name: str) -> float:
    if isinstance(value, bool):
        raise LexicalConfigError(f"{name} must be a finite number")
    try:
        result = float(value)
    except (TypeError, ValueError) as exc:
        raise LexicalConfigError(f"{name} must be a finite number") from exc
    if not math.isfinite(result):
        raise LexicalConfigError(f"{name} must be a finite number")
    return result


@dataclass(frozen=True, slots=True)
class Bm25V2Options:
    """Immutable BM25 encoding options shared by ingest and query."""

    k: float = 1.2
    b: float = 0.75
    avg_len: float = 256.0
    tokenizer: str = "multilingual"
    language: str = "none"
    lowercase: bool = True
    ascii_folding: bool = False
    min_token_len: int | None = None
    max_token_len: int | None = None

    def __post_init__(self) -> None:
        if self.k <= 0 or not math.isfinite(self.k):
            raise LexicalConfigError("bm25_v2.k must be finite and greater than 0")
        if not math.isfinite(self.b) or not 0 <= self.b <= 1:
            raise LexicalConfigError("bm25_v2.b must be between 0 and 1")
        if self.avg_len <= 0 or not math.isfinite(self.avg_len):
            raise LexicalConfigError("bm25_v2.avg_len must be finite and greater than 0")
        if self.tokenizer not in _TOKENIZERS:
            raise LexicalConfigError(
                "bm25_v2.tokenizer must be prefix|whitespace|word|multilingual"
            )
        if self.language not in _LANGUAGES:
            raise LexicalConfigError(
                "bm25_v2.language is not supported by the pinned qdrant BM25 contract"
            )
        if not isinstance(self.lowercase, bool):
            raise LexicalConfigError("bm25_v2.lowercase must be a boolean")
        if not isinstance(self.ascii_folding, bool):
            raise LexicalConfigError("bm25_v2.ascii_folding must be a boolean")
        for name, value in (
            ("min_token_len", self.min_token_len),
            ("max_token_len", self.max_token_len),
        ):
            if value is not None and (
                isinstance(value, bool) or not isinstance(value, int) or value <= 0
            ):
                raise LexicalConfigError(f"bm25_v2.{name} must be a positive integer")
        if (
            self.min_token_len is not None
            and self.max_token_len is not None
            and self.min_token_len > self.max_token_len
        ):
            raise LexicalConfigError("bm25_v2.min_token_len must not exceed max_token_len")

    @classmethod
    def from_dict(cls, value: Any) -> Bm25V2Options:
        data = value if isinstance(value, dict) else {}
        unknown = sorted(set(data) - _BM25_V2_KEYS - {"schema_fingerprint", "semantics"})
        if unknown:
            raise LexicalConfigError("unsupported bm25_v2 option(s): " + ", ".join(unknown))
        model = str(data.get("model") or BM25_V2_MODEL).strip().lower()
        field = str(data.get("field") or BM25_V2_FIELD).strip()
        if model != BM25_V2_MODEL:
            raise LexicalConfigError(f"bm25_v2.model must be {BM25_V2_MODEL}")
        if field != BM25_V2_FIELD:
            raise LexicalConfigError(f"bm25_v2.field must be {BM25_V2_FIELD}")
        fixed_contract = {
            "encoder_implementation": BM25_V2_ENCODER_IMPLEMENTATION,
            "encoder_contract_version": BM25_V2_ENCODER_CONTRACT_VERSION,
            "qdrant_client_version": QDRANT_CLIENT_VERSION,
        }
        for key, expected in fixed_contract.items():
            supplied = data.get(key)
            if supplied is not None and str(supplied) != expected:
                raise LexicalConfigError(f"bm25_v2.{key} must be {expected}")
        lowercase = data.get("lowercase", True)
        if not isinstance(lowercase, bool):
            raise LexicalConfigError("bm25_v2.lowercase must be a boolean")
        ascii_folding = data.get("ascii_folding", False)
        if not isinstance(ascii_folding, bool):
            raise LexicalConfigError("bm25_v2.ascii_folding must be a boolean")
        if data.get("stopwords") is not None or data.get("stemmer") is not None:
            raise LexicalConfigError(
                "bm25_v2 stopwords/stemmer are fixed by language and cannot be overridden"
            )

        token_lengths: dict[str, int | None] = {}
        for key in ("min_token_len", "max_token_len"):
            raw = data.get(key)
            if raw is None:
                token_lengths[key] = None
            elif isinstance(raw, bool) or not isinstance(raw, int):
                raise LexicalConfigError(f"bm25_v2.{key} must be a positive integer")
            else:
                token_lengths[key] = raw
        return cls(
            k=_finite_float(data.get("k", 1.2), name="bm25_v2.k"),
            b=_finite_float(data.get("b", 0.75), name="bm25_v2.b"),
            avg_len=_finite_float(data.get("avg_len", 256.0), name="bm25_v2.avg_len"),
            tokenizer=str(data.get("tokenizer") or "multilingual").strip().lower(),
            language=str(data.get("language") or "none").strip().lower(),
            lowercase=lowercase,
            ascii_folding=ascii_folding,
            min_token_len=token_lengths["min_token_len"],
            max_token_len=token_lengths["max_token_len"],
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "field": BM25_V2_FIELD,
            "model": BM25_V2_MODEL,
            "encoder_implementation": BM25_V2_ENCODER_IMPLEMENTATION,
            "encoder_contract_version": BM25_V2_ENCODER_CONTRACT_VERSION,
            "qdrant_client_version": QDRANT_CLIENT_VERSION,
            "k": self.k,
            "b": self.b,
            "avg_len": self.avg_len,
            "tokenizer": self.tokenizer,
            "language": self.language,
            "lowercase": self.lowercase,
            "ascii_folding": self.ascii_folding,
            # Lock server defaults into the schema fingerprint. Qdrant treats
            # these as ingest/query preprocessing options, so changing them is
            # a new sparse encoding even when the model name stays constant.
            "stopwords": None,
            "stemmer": None,
            "min_token_len": self.min_token_len,
            "max_token_len": self.max_token_len,
        }

    @property
    def fingerprint(self) -> str:
        canonical = json.dumps(self.to_dict(), sort_keys=True, separators=(",", ":"))
        return "sha256:" + hashlib.sha256(canonical.encode("utf-8")).hexdigest()


@dataclass(frozen=True, slots=True)
class FilteringProfile:
    """Safety profile for filtered retrieval on the v2 collection.

    The required fields are fixed so a dataset cannot weaken tenant/dataset
    filter readiness. Strict Qdrant filtering is opt-in during shadow rollout.
    """

    strict_unindexed_filtering: bool = False

    def __post_init__(self) -> None:
        if not isinstance(self.strict_unindexed_filtering, bool):
            raise LexicalConfigError(
                "bm25_v2.filtering.strict_unindexed_filtering must be a boolean"
            )

    @classmethod
    def from_dict(cls, value: Any) -> FilteringProfile:
        if value is None:
            return cls()
        if not isinstance(value, dict):
            raise LexicalConfigError("bm25_v2.filtering must be an object")
        unknown = sorted(
            set(value)
            - {
                "required_payload_indexes",
                "strict_unindexed_filtering",
                "profile_fingerprint",
            }
        )
        if unknown:
            raise LexicalConfigError(
                "unsupported bm25_v2.filtering option(s): " + ", ".join(unknown)
            )
        required = value.get("required_payload_indexes")
        if required is not None and (
            not isinstance(required, list) or tuple(required) != REQUIRED_FILTER_PAYLOAD_INDEXES
        ):
            raise LexicalConfigError(
                "bm25_v2.filtering.required_payload_indexes must be ['tenant_id', 'dataset_id']"
            )
        strict = value.get("strict_unindexed_filtering", False)
        if not isinstance(strict, bool):
            raise LexicalConfigError(
                "bm25_v2.filtering.strict_unindexed_filtering must be a boolean"
            )
        return cls(strict_unindexed_filtering=strict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "required_payload_indexes": list(REQUIRED_FILTER_PAYLOAD_INDEXES),
            "strict_unindexed_filtering": self.strict_unindexed_filtering,
        }

    @property
    def fingerprint(self) -> str:
        canonical = json.dumps(self.to_dict(), sort_keys=True, separators=(",", ":"))
        return "sha256:" + hashlib.sha256(canonical.encode("utf-8")).hexdigest()


@dataclass(frozen=True, slots=True)
class LexicalConfig:
    """Dataset selection plus the immutable BM25 v2 shadow schema."""

    active_version: str = LEXICAL_V1
    bm25_v2_shadow_write_enabled: bool = False
    bm25_v2: Bm25V2Options = Bm25V2Options()
    filtering: FilteringProfile = FilteringProfile()
    runtime_revision: int = 0
    configured: bool = False

    def __post_init__(self) -> None:
        if self.active_version not in _ACTIVE_VERSIONS:
            raise LexicalConfigError("retrieval.lexical.active_version must be lexical_v1|bm25_v2")
        if not isinstance(self.bm25_v2_shadow_write_enabled, bool):
            raise LexicalConfigError("bm25_v2.shadow_write_enabled must be a boolean")
        if self.active_version == BM25_V2 and not self.bm25_v2_shadow_write_enabled:
            raise LexicalConfigError("bm25_v2 reads require bm25_v2.shadow_write_enabled=true")
        if (
            isinstance(self.runtime_revision, bool)
            or not isinstance(self.runtime_revision, int)
            or self.runtime_revision < 0
        ):
            raise LexicalConfigError("bm25_v2 runtime revision must be a non-negative integer")

    @classmethod
    def from_index_config(cls, index_config: Any) -> LexicalConfig:
        index_data = index_config if isinstance(index_config, dict) else {}
        retrieval = index_data.get("retrieval")
        retrieval_data = retrieval if isinstance(retrieval, dict) else {}
        lexical = retrieval_data.get("lexical")
        if lexical is None:
            return cls()
        if not isinstance(lexical, dict):
            raise LexicalConfigError("retrieval.lexical must be an object")
        unknown = sorted(set(lexical) - {"active_version", "bm25_v2"})
        if unknown:
            raise LexicalConfigError(
                "unsupported retrieval.lexical option(s): " + ", ".join(unknown)
            )

        active_version = str(lexical.get("active_version") or LEXICAL_V1).strip().lower()
        bm25_data = lexical.get("bm25_v2")
        if bm25_data is None:
            bm25_data = {}
        if not isinstance(bm25_data, dict):
            raise LexicalConfigError("retrieval.lexical.bm25_v2 must be an object")
        shadow_write = bm25_data.get("shadow_write_enabled", False)
        if not isinstance(shadow_write, bool):
            raise LexicalConfigError("bm25_v2.shadow_write_enabled must be a boolean")
        return cls(
            active_version=active_version,
            bm25_v2_shadow_write_enabled=shadow_write,
            bm25_v2=Bm25V2Options.from_dict(bm25_data),
            filtering=FilteringProfile.from_dict(bm25_data.get("filtering")),
            configured=True,
        )

    @property
    def writes_bm25_v2(self) -> bool:
        return self.bm25_v2_shadow_write_enabled

    @property
    def reads_bm25_v2(self) -> bool:
        return self.active_version == BM25_V2

    @property
    def active_field(self) -> str:
        return BM25_V2_FIELD if self.reads_bm25_v2 else LEXICAL_V1_FIELD

    def with_runtime_selection(
        self,
        *,
        active_version: str,
        shadow_write_enabled: bool,
        filtering: FilteringProfile | None = None,
        runtime_revision: int | None = None,
    ) -> LexicalConfig:
        return replace(
            self,
            active_version=active_version,
            bm25_v2_shadow_write_enabled=shadow_write_enabled,
            filtering=filtering or self.filtering,
            runtime_revision=(
                self.runtime_revision if runtime_revision is None else runtime_revision
            ),
            configured=True,
        )

    def to_collection_metadata(self) -> dict[str, Any]:
        return {
            COLLECTION_METADATA_KEY: {
                "schema_version": 2,
                "runtime_revision": self.runtime_revision,
                "active_version": self.active_version,
                "legacy": {
                    "version": LEXICAL_V1,
                    "field": LEXICAL_V1_FIELD,
                    "semantics": "hashed_term_presence_plus_qdrant_idf",
                },
                "bm25_v2": {
                    **self.bm25_v2.to_dict(),
                    "shadow_write_enabled": self.bm25_v2_shadow_write_enabled,
                    "schema_fingerprint": self.bm25_v2.fingerprint,
                    "semantics": "qdrant_bm25_tf_idf_document_length_normalized",
                },
                "filtering": {
                    **self.filtering.to_dict(),
                    "profile_fingerprint": self.filtering.fingerprint,
                },
            }
        }

    @classmethod
    def from_collection_metadata(cls, metadata: Any) -> LexicalConfig | None:
        if not isinstance(metadata, dict):
            return None
        raw = metadata.get(COLLECTION_METADATA_KEY)
        if not isinstance(raw, dict) or int(raw.get("schema_version") or 0) != 2:
            return None
        bm25_data = raw.get("bm25_v2")
        if not isinstance(bm25_data, dict):
            raise LexicalConfigError("collection bm25_v2 metadata is missing")
        expected_fingerprint = str(bm25_data.get("schema_fingerprint") or "")
        options = Bm25V2Options.from_dict(bm25_data)
        if expected_fingerprint != options.fingerprint:
            raise LexicalConfigError("collection bm25_v2 metadata fingerprint mismatch")
        filtering_data = raw.get("filtering")
        if not isinstance(filtering_data, dict):
            raise LexicalConfigError("collection bm25_v2 filtering metadata is missing")
        filtering = FilteringProfile.from_dict(filtering_data)
        expected_filtering_fingerprint = str(filtering_data.get("profile_fingerprint") or "")
        if expected_filtering_fingerprint != filtering.fingerprint:
            raise LexicalConfigError("collection bm25_v2 filtering profile fingerprint mismatch")
        shadow_write = bm25_data.get("shadow_write_enabled", False)
        if not isinstance(shadow_write, bool):
            raise LexicalConfigError("collection bm25_v2 shadow_write_enabled must be a boolean")
        return cls(
            active_version=str(raw.get("active_version") or LEXICAL_V1),
            bm25_v2_shadow_write_enabled=shadow_write,
            bm25_v2=options,
            filtering=filtering,
            runtime_revision=int(raw.get("runtime_revision") or 0),
            configured=True,
        )
