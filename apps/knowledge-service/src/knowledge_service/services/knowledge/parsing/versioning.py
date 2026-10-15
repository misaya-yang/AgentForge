"""Version-keyed caching primitives for parsing (PRD T4 item 3).

The cache key is ``(page content hash, backend name, backend version)`` —
a parser upgrade changes the version, which transparently invalidates cached
page IR and drives the canary re-ingestion pass without any explicit purge.
"""

from __future__ import annotations

import hashlib

PAGE_CACHE_KEY_VERSION = "1"


def page_cache_key(
    content_hash: str,
    page_number: int,
    backend_name: str,
    backend_version: str,
    parser_config_hash: str = "",
) -> str:
    """Stable key for one page's IR under one backend version."""
    raw = "|".join(
        [
            PAGE_CACHE_KEY_VERSION,
            content_hash,
            str(page_number),
            backend_name,
            backend_version,
            parser_config_hash,
        ]
    )
    return hashlib.sha256(raw.encode("utf-8")).hexdigest()


def cascade_bundle_version(stage_versions: list[tuple[str, str]]) -> str:
    """Aggregate version of an ordered cascade (name, version) pairs.

    Stored alongside a document's IR: when the tenant's configured cascade
    changes (new backend, upgraded parser), the bundle differs and the
    document becomes eligible for canary re-parse.
    """
    raw = "|".join(f"{name}={version}" for name, version in stage_versions)
    return hashlib.sha256(raw.encode("utf-8")).hexdigest()[:16]


def needs_reparse(stored_bundle: str | None, current_bundle: str) -> bool:
    """Whether a document parsed under ``stored_bundle`` must be re-parsed."""
    if not stored_bundle:
        return True
    return stored_bundle != current_bundle
