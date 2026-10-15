"""Bounded citations from guarded Runtime events; never arbitrary tool results."""
from __future__ import annotations

import json
import math
from typing import Any

from .source_access import _context_references, source_scope_allowed, visible_dataset_names


class ChatCitations:
    """Keep source identity separate from the bounded displayed excerpt.

    source_hash identifies the complete immutable source, not content[:2000].
    Runtime channel ownership is trusted; tool_call_id is not a signed proof.
    """

    def __init__(self) -> None:
        self.groups: dict[str, list[dict[str, Any]]] = {}
        self.seen: set[str] = set()
        self.documents: set[tuple[str, str]] = set()
        self.versions: set[tuple[str, str, int, str]] = set()
        self.datasets: set[str] = set()

    def add(self, data: dict[str, Any]) -> None:
        renderable = isinstance(data.get("tool_call_id"), str) and bool(data["tool_call_id"].strip())
        raw_chunks = data.get("chunks")
        if not isinstance(raw_chunks, list):
            raise ValueError("Invalid citation collection")
        for raw in raw_chunks:
            if not isinstance(raw, dict):
                raise ValueError("Invalid citation")
            documents, versions = _context_references([raw])
            if any(len(value) > 512 for ref in documents for value in ref):
                raise ValueError("Citation identity exceeds limit")
            self.documents.update(documents)
            self.versions.update(versions)
            self.datasets.update(dataset for dataset, _ in documents)
            if len(self.documents) + len(self.versions) + len(self.datasets) > 256:
                raise ValueError("Citation authority reference limit exceeded")
            if not renderable:
                continue
            metadata = raw.get("metadata") if isinstance(raw.get("metadata"), dict) else {}
            identity: dict[str, Any] = {}
            for key in ("dataset_id", "document_id", "segment_id", "source_version", "source_hash"):
                top, nested = raw.get(key), metadata.get(key)
                if top is not None and nested is not None and top != nested:
                    raise ValueError("Conflicting citation identity")
                value = top if top is not None else nested
                if value is not None:
                    identity[key] = value
            dataset = identity.get("dataset_id")
            document = identity.get("document_id")
            if not all(isinstance(value, str) and value and value == value.strip()
                       for value in (dataset, document)):
                raise ValueError("Missing citation identity")
            segment = identity.get("segment_id")
            if segment is not None and (not isinstance(segment, str) or not segment.strip()):
                raise ValueError("Invalid segment identity")
            content = raw.get("content")
            if not isinstance(content, str):
                raise ValueError("Missing citation excerpt")
            # Full-source versions are reauthorized below using the existing owner proxy.
            identity.pop("dataset_id")
            chunk: dict[str, Any] = {"content": content[:2000], "metadata": identity}
            score = raw.get("score")
            if type(score) in (int, float) and math.isfinite(score):
                chunk["score"] = score
            key = json.dumps([dataset, chunk], sort_keys=True, allow_nan=False)
            if key in self.seen:
                continue
            if dataset not in self.groups and len(self.groups) >= 20:
                continue
            group = self.groups.setdefault(dataset, [])
            if len(group) >= 20:
                continue
            self.seen.add(key)
            group.append(chunk)

    async def project(self, request, user) -> list[dict[str, Any]] | None:
        if not self.documents and not self.versions and not self.datasets:
            return []
        names = await visible_dataset_names(request, user)
        if not await source_scope_allowed(
            request, user, frozenset(self.datasets), frozenset(self.documents),
            visible_datasets=names, versioned_refs=frozenset(self.versions),
        ):
            return None
        return [{"dataset_id": dataset, "dataset_name": names[dataset], "chunks": chunks}
                for dataset, chunks in self.groups.items()]
