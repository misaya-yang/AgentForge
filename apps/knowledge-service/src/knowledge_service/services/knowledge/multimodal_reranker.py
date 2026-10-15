"""
Multimodal Reranker using Vision-Language Models.

Part of P3: Multimodal RAG Full-Chain Optimization

Uses DashScope VLM (qwen-vl-max) to score image-query relevance,
enabling cross-modal reranking for multimodal RAG.

Strategy:
1. For image candidates: Use VLM to evaluate relevance to text query
2. For text candidates with images: Consider associated images in scoring
3. Combine multimodal scores with original retrieval scores
"""

from __future__ import annotations

import asyncio
import logging
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from .vlm_service import DashScopeVLMService

logger = logging.getLogger(__name__)


@dataclass
class RerankCandidate:
    """Candidate for multimodal reranking."""

    segment_id: str
    text: str | None = None
    image_url: str | None = None
    image_bytes: bytes | None = None
    media_type: str = "text"  # "text" | "image"
    original_score: float = 0.0
    rerank_score: float = 0.0
    metadata: dict[str, Any] = field(default_factory=dict)


class MultimodalReranker:
    """
    Multimodal reranker using Vision-Language Models.

    For image candidates, uses VLM to evaluate query relevance:
    - Generate relevance assessment between image and query
    - Output a score [0, 1] indicating how well the image answers the query

    For text candidates with associated images:
    - Optionally boost scores if images are highly relevant
    - Use VLM to verify image-text coherence

    Configuration:
        vlm_service: DashScopeVLMService instance for image analysis
        max_concurrent: Maximum concurrent VLM calls
        timeout_seconds: Timeout for each VLM call
        image_weight: How much to weight image scores vs original scores
    """

    # Prompt for evaluating image-query relevance (optimized for VLM scoring)
    RELEVANCE_PROMPT = """你是一个专业的图文相关性评估专家。请评估这张图片与用户查询的相关性。

用户查询: {query}
{description_context}
评分标准:
1.0: 图片完美回答用户问题，包含所有查询要素
0.9: 图片直接回答问题，内容高度相关
0.7-0.8: 图片部分相关，包含部分查询信息
0.5-0.6: 图片有一定相关性，但信息不完整
0.3-0.4: 图片略微相关，只有少量关联内容
0.1-0.2: 图片与查询关系很弱
0.0: 图片与查询完全无关

请只输出一个0到1之间的数字作为评分，不要输出任何解释或其他内容。"""

    # Alternative English prompt (optimized for VLM scoring)
    RELEVANCE_PROMPT_EN = """You are a professional image-query relevance expert. Evaluate how relevant this image is to the user's query.

User Query: {query}
{description_context}
Scoring criteria:
1.0: Image perfectly answers the query with all required elements
0.9: Image directly answers the question with highly relevant content
0.7-0.8: Image is partially relevant, contains some query information
0.5-0.6: Image has some relevance but incomplete information
0.3-0.4: Image is slightly relevant with minimal connection
0.1-0.2: Image has very weak relation to the query
0.0: Image is completely irrelevant

Output ONLY a single number between 0 and 1 as the score, nothing else."""

    def __init__(
        self,
        vlm_service: DashScopeVLMService | None = None,
        max_concurrent: int = 3,
        timeout_seconds: float = 30.0,
        image_weight: float = 0.4,
        use_english_prompt: bool = False,
        image_storage_service: Any | None = None,
    ):
        """
        Initialize MultimodalReranker.

        Args:
            vlm_service: VLM service for image analysis. If None, image scoring is skipped.
            max_concurrent: Maximum concurrent VLM calls (default 3)
            timeout_seconds: Timeout per VLM call (default 30s)
            image_weight: Weight for image score when combining with original (default 0.4)
            use_english_prompt: Use English prompt instead of Chinese (default False)
            image_storage_service: Optional storage service for loading images from S3/OSS
        """
        self.vlm_service = vlm_service
        self.max_concurrent = max_concurrent
        self.timeout = timeout_seconds
        self.image_weight = image_weight
        self._semaphore = asyncio.Semaphore(max_concurrent)
        self.prompt_template = (
            self.RELEVANCE_PROMPT_EN if use_english_prompt else self.RELEVANCE_PROMPT
        )
        self.image_storage_service = image_storage_service

    async def rerank(
        self,
        query: str,
        candidates: list[RerankCandidate],
        top_k: int = 10,
        rerank_images_only: bool = False,
        score_threshold: float = 0.0,
        score_weight: float | None = None,
    ) -> list[RerankCandidate]:
        """
        Rerank candidates using multimodal scoring.

        Implements smart routing: only image candidates are sent to VLM for scoring,
        while text candidates retain their original scores. This optimizes VLM usage
        and reduces latency.

        Score fusion formula:
            final_score = original_score * (1 - score_weight) + vlm_score * score_weight

        Args:
            query: User query text
            candidates: List of candidates to rerank
            top_k: Return top-k results after reranking
            rerank_images_only: If True, only rerank image candidates (text keeps original score)
            score_threshold: Minimum score to include in results
            score_weight: Weight for VLM score in fusion [0.0, 1.0]. If None, uses
                         self.image_weight from initialization. Higher values give
                         more weight to VLM relevance scores.

        Returns:
            Reranked candidates sorted by score (descending).
            Each candidate's metadata will contain:
            - vlm_score: Raw VLM relevance score [0.0, 1.0] (for images only)
            - original_score: Original retrieval score before reranking
        """
        if not candidates:
            return []

        # Use provided score_weight or fall back to instance default
        effective_weight = score_weight if score_weight is not None else self.image_weight

        # Separate image and text candidates - VLM is only called for images
        image_candidates = [c for c in candidates if c.media_type == "image"]
        text_candidates = [c for c in candidates if c.media_type == "text"]

        logger.info(
            f"Multimodal rerank: {len(image_candidates)} images, "
            f"{len(text_candidates)} text candidates (weight={effective_weight:.2f})"
        )

        # Score image candidates using VLM (only images go through VLM)
        if image_candidates and self.vlm_service:
            await self._score_images_batch(query, image_candidates, effective_weight)
        elif image_candidates:
            # No VLM service, preserve original scores and store in metadata
            for c in image_candidates:
                c.rerank_score = c.original_score
                c.metadata["original_score"] = c.original_score
                c.metadata["vlm_score"] = None
                c.metadata["vlm_skipped"] = "no_vlm_service"

        # Text candidates keep original score (or can be boosted by associated images)
        for c in text_candidates:
            c.metadata["original_score"] = c.original_score
            if rerank_images_only:
                c.rerank_score = c.original_score
            else:
                # Optionally enhance with VLM-based scoring in future
                c.rerank_score = c.original_score

        # Combine and sort
        all_candidates = image_candidates + text_candidates
        all_candidates = [c for c in all_candidates if c.rerank_score >= score_threshold]
        all_candidates.sort(key=lambda x: x.rerank_score, reverse=True)

        return all_candidates[:top_k]

    async def _score_images_batch(
        self,
        query: str,
        candidates: list[RerankCandidate],
        score_weight: float | None = None,
    ) -> None:
        """
        Score image candidates in batch with concurrency control.

        Args:
            query: User query
            candidates: Image candidates to score (modified in-place)
            score_weight: Weight for VLM score in fusion. If None, uses self.image_weight.
        """
        # Use provided weight or instance default
        effective_weight = score_weight if score_weight is not None else self.image_weight

        async def score_one(c: RerankCandidate):
            async with self._semaphore:
                try:
                    vlm_score = await self._score_single_image(query, c)
                    # Store original score in metadata before modifying
                    c.metadata["original_score"] = c.original_score
                    c.metadata["vlm_score"] = vlm_score
                    c.metadata["score_weight"] = effective_weight
                    # Combine VLM score with original score using weighted fusion
                    c.rerank_score = (
                        1 - effective_weight
                    ) * c.original_score + effective_weight * vlm_score
                except asyncio.TimeoutError:
                    logger.warning(f"Timeout scoring image {c.segment_id}")
                    c.metadata["original_score"] = c.original_score
                    c.metadata["vlm_score"] = None
                    c.metadata["vlm_error"] = "timeout"
                    c.rerank_score = c.original_score * 0.7  # Penalize timeout
                except Exception as e:
                    logger.warning(f"Failed to score image {c.segment_id}: {e}")
                    c.metadata["original_score"] = c.original_score
                    c.metadata["vlm_score"] = None
                    c.metadata["vlm_error"] = str(e)
                    c.rerank_score = c.original_score * 0.5  # Penalize errors

        tasks = [score_one(c) for c in candidates]
        await asyncio.gather(*tasks, return_exceptions=True)

    async def _score_single_image(
        self,
        query: str,
        candidate: RerankCandidate,
    ) -> float:
        """
        Score a single image against the query using VLM.

        Args:
            query: User query
            candidate: Image candidate

        Returns:
            Relevance score [0.0, 1.0]
        """
        if not self.vlm_service:
            return candidate.original_score

        # Get image bytes
        image_bytes = candidate.image_bytes
        if not image_bytes and candidate.image_url:
            image_bytes = await self._load_image_from_url(candidate.image_url)

        if not image_bytes:
            logger.warning(f"No image data for {candidate.segment_id}")
            return candidate.original_score

        # Build description context if available
        description_context = ""
        if candidate.text:
            description_context = (
                f"图片描述: {candidate.text}\n"
                if "你是" in self.prompt_template
                else f"Image description: {candidate.text}\n"
            )

        # Build prompt with query and optional description
        prompt = self.prompt_template.format(query=query, description_context=description_context)

        try:
            # Call VLM with timeout
            result = await asyncio.wait_for(
                self.vlm_service.describe_image(
                    image_bytes=image_bytes,
                    prompt=prompt,
                    max_tokens=10,  # Only need a number
                ),
                timeout=self.timeout,
            )

            # Parse score from response
            score_text = result.description.strip()

            # Try to extract number from response
            score = self._parse_score(score_text)
            logger.debug(f"VLM scored {candidate.segment_id}: {score} (raw: {score_text[:50]})")
            return score

        except Exception as e:
            logger.warning(f"VLM scoring failed for {candidate.segment_id}: {e}")
            return candidate.original_score

    def _parse_score(self, text: str) -> float:
        """
        Parse a score from VLM response text.

        Handles various formats:
        - "0.85"
        - "Score: 0.85"
        - "The relevance score is 0.85"
        - "85%"

        Args:
            text: VLM response text

        Returns:
            Score in range [0.0, 1.0]
        """
        import re

        text = text.strip()

        # Try direct float parsing
        try:
            score = float(text)
            if 0 <= score <= 1:
                return score
            elif 0 <= score <= 100:
                return score / 100.0
        except ValueError:
            pass

        # Try to extract number from text
        # Match patterns like "0.85", ".85", "85%", "85"
        patterns = [
            r"(\d+\.\d+)",  # 0.85
            r"\.(\d+)",  # .85
            r"(\d+)%",  # 85%
            r"(\d+)",  # 85
        ]

        for pattern in patterns:
            match = re.search(pattern, text)
            if match:
                try:
                    value = float(match.group(1))
                    if pattern == r"\.(\d+)":
                        value = float(f"0.{match.group(1)}")
                    if value > 1 and value <= 100:
                        value = value / 100.0
                    if 0 <= value <= 1:
                        return value
                except ValueError:
                    continue

        # Default to 0.5 if parsing fails
        logger.warning(f"Could not parse score from: {text[:100]}")
        return 0.5

    async def _load_image_from_url(self, url: str) -> bytes | None:
        """
        Load image bytes from URL.

        Args:
            url: Image URL (S3, OSS, HTTP, or file://)

        Returns:
            Image bytes or None if loading fails

        Supported URL schemes:
            - s3://bucket/key - AWS S3 (requires image_storage_service)
            - oss://bucket/key - Alibaba OSS (requires image_storage_service)
            - https://... - Standard HTTP(S) URLs
            - file://... - Local file URLs
        """
        import httpx

        if not url:
            return None

        try:
            # Handle S3/OSS URLs via storage service
            if url.startswith("s3://") or url.startswith("oss://"):
                if not self.image_storage_service:
                    logger.warning(f"S3/OSS URL loading requires image_storage_service: {url}")
                    return None

                # Parse S3/OSS URL: s3://bucket/path/to/image.png
                from urllib.parse import urlparse

                parsed = urlparse(url)
                key = parsed.path.lstrip("/")

                if not key:
                    logger.warning(f"Invalid S3/OSS URL (no key): {url}")
                    return None

                # Use storage service backend to download
                try:
                    content = await self.image_storage_service._backend.download(key)
                    logger.debug(f"Loaded {len(content)} bytes from {url}")
                    return content
                except Exception as storage_err:
                    logger.warning(f"Storage service failed to load {url}: {storage_err}")
                    return None

            # Handle local file:// URLs
            if url.startswith("file://"):
                import os
                from urllib.parse import unquote, urlparse

                parsed = urlparse(url)
                # Handle signed URLs by stripping query params
                file_path = unquote(parsed.path)

                if not os.path.exists(file_path):
                    logger.warning(f"Local file not found: {file_path}")
                    return None

                with open(file_path, "rb") as f:
                    content = f.read()
                logger.debug(f"Loaded {len(content)} bytes from local file: {file_path}")
                return content

            # Standard HTTP(S) URL
            async with httpx.AsyncClient(timeout=10.0) as client:
                response = await client.get(url)
                response.raise_for_status()
                return response.content

        except Exception as e:
            logger.warning(f"Failed to load image from {url}: {e}")
            return None

    async def score_text_with_images(
        self,
        query: str,
        text: str,
        image_urls: list[str],
        original_score: float = 0.0,
    ) -> float:
        """
        Score a text segment by also considering its associated images.

        This can boost the score of text segments that have highly
        relevant associated images.

        Args:
            query: User query
            text: Text content
            image_urls: URLs of associated images
            original_score: Original retrieval score

        Returns:
            Enhanced score incorporating image relevance
        """
        if not image_urls or not self.vlm_service:
            return original_score

        # Score each associated image
        image_scores: list[float] = []
        for url in image_urls[:3]:  # Limit to top 3 images
            try:
                image_bytes = await self._load_image_from_url(url)
                if image_bytes:
                    candidate = RerankCandidate(
                        segment_id="temp",
                        image_url=url,
                        image_bytes=image_bytes,
                        media_type="image",
                        original_score=0.5,
                    )
                    score = await self._score_single_image(query, candidate)
                    image_scores.append(score)
            except Exception as e:
                logger.warning(f"Failed to score associated image {url}: {e}")

        if not image_scores:
            return original_score

        # Average image score
        avg_image_score = sum(image_scores) / len(image_scores)

        # Combine: if images are relevant, boost the text score
        if avg_image_score > 0.6:
            # Images are relevant - boost text score
            boost = 0.1 * (avg_image_score - 0.5)  # Max boost of 0.05 at score 1.0
            return min(1.0, original_score + boost)

        return original_score


def create_multimodal_reranker(
    vlm_service: DashScopeVLMService | None = None,
    max_concurrent: int = 3,
    timeout_seconds: float = 30.0,
) -> MultimodalReranker:
    """
    Factory function to create a MultimodalReranker.

    Args:
        vlm_service: VLM service for image analysis
        max_concurrent: Maximum concurrent VLM calls
        timeout_seconds: Timeout per VLM call

    Returns:
        Configured MultimodalReranker instance
    """
    return MultimodalReranker(
        vlm_service=vlm_service,
        max_concurrent=max_concurrent,
        timeout_seconds=timeout_seconds,
    )
