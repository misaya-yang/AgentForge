"""
Summary Generator - LLM-based document summarization

Generates document summaries and keywords for L1 hierarchical indexing.
Uses LLM to create:
- Document-level summaries
- Section-level summaries
- Keywords and topics extraction
"""

import asyncio
from dataclasses import dataclass
from typing import Any

from ...core.observability.logging import get_logger

logger = get_logger(__name__)


@dataclass
class SummaryResult:
    """Result of summary generation."""

    summary: str
    keywords: list[str]
    topics: list[str]
    language: str = "en"
    token_count: int = 0

    def to_dict(self) -> dict[str, Any]:
        return {
            "summary": self.summary,
            "keywords": self.keywords,
            "topics": self.topics,
            "language": self.language,
            "token_count": self.token_count,
        }


class SummaryGenerator:
    """
    LLM-based summary generator for hierarchical indexing.

    Generates summaries at document and section levels for L1/L2 indexing.
    Supports multiple LLM providers via the unified LLM service.
    """

    # Prompt templates
    DOCUMENT_SUMMARY_PROMPT = """Analyze the following document and provide:
1. A concise summary (2-3 sentences) capturing the main content and purpose
2. 5-10 relevant keywords
3. 2-3 main topics/themes

Document:
{text}

Respond in JSON format:
{{
    "summary": "...",
    "keywords": ["keyword1", "keyword2", ...],
    "topics": ["topic1", "topic2", ...]
}}"""

    SECTION_SUMMARY_PROMPT = """Summarize the following section in 1-2 sentences:

{text}

Summary:"""

    # Limits
    MAX_INPUT_CHARS = 15000  # ~3000-4000 tokens
    MAX_SECTION_CHARS = 4000

    def __init__(
        self,
        llm_service: Any | None = None,
        model: str = "gpt-4o-mini",
        timeout: float = 60.0,
    ):
        """
        Initialize the summary generator.

        Args:
            llm_service: LLM service for generation
            model: Model to use for summarization
            timeout: Request timeout in seconds
        """
        self.llm_service = llm_service
        self.model = model
        self.timeout = timeout
        self._client = None

    async def summarize_document(
        self,
        text: str,
        language: str = "auto",
    ) -> dict[str, Any]:
        """
        Generate document-level summary with keywords and topics.

        Args:
            text: Full document text
            language: Target language for summary

        Returns:
            Dictionary with summary, keywords, and topics
        """
        # Truncate if too long
        if len(text) > self.MAX_INPUT_CHARS:
            # Take beginning and end for context
            text = (
                text[: self.MAX_INPUT_CHARS // 2] + "\n...\n" + text[-self.MAX_INPUT_CHARS // 2 :]
            )

        prompt = self.DOCUMENT_SUMMARY_PROMPT.format(text=text)

        try:
            response = await self._call_llm(prompt)
            result = self._parse_json_response(response)

            return {
                "summary": result.get("summary", ""),
                "keywords": result.get("keywords", []),
                "topics": result.get("topics", []),
            }

        except Exception as e:
            logger.error(f"Document summarization failed: {e}")
            # Fallback: extract first paragraph as summary
            return self._fallback_summary(text)

    async def summarize_section(self, text: str) -> str:
        """
        Generate a brief summary for a section.

        Args:
            text: Section text

        Returns:
            Section summary string
        """
        # Truncate if needed
        if len(text) > self.MAX_SECTION_CHARS:
            text = text[: self.MAX_SECTION_CHARS]

        prompt = self.SECTION_SUMMARY_PROMPT.format(text=text)

        try:
            response = await self._call_llm(prompt)
            return response.strip()
        except Exception as e:
            logger.debug(f"Section summarization failed: {e}")
            # Fallback: return first sentence
            return self._extract_first_sentence(text)

    async def extract_keywords(self, text: str, count: int = 10) -> list[str]:
        """
        Extract keywords from text.

        Args:
            text: Source text
            count: Number of keywords to extract

        Returns:
            List of keywords
        """
        prompt = f"""Extract {count} important keywords from the following text.
Return only the keywords as a comma-separated list.

Text:
{text[: self.MAX_INPUT_CHARS]}

Keywords:"""

        try:
            response = await self._call_llm(prompt)
            keywords = [k.strip() for k in response.split(",")]
            return keywords[:count]
        except Exception as e:
            logger.debug(f"Keyword extraction failed: {e}")
            return []

    async def _call_llm(self, prompt: str) -> str:
        """Call the LLM service."""
        if self.llm_service:
            # Use provided LLM service
            try:
                response = await asyncio.wait_for(
                    self.llm_service.generate(
                        prompt=prompt,
                        model=self.model,
                        max_tokens=500,
                    ),
                    timeout=self.timeout,
                )
                return response.get("content", response.get("text", ""))
            except Exception as e:
                logger.error(f"LLM service call failed: {e}")
                raise

        # Fallback to direct API call if no service provided
        return await self._call_openai_direct(prompt)

    async def _call_openai_direct(self, prompt: str) -> str:
        """Direct OpenAI API call as fallback."""
        try:
            import openai

            if self._client is None:
                self._client = openai.AsyncOpenAI()

            response = await asyncio.wait_for(
                self._client.chat.completions.create(
                    model=self.model,
                    messages=[{"role": "user", "content": prompt}],
                    max_tokens=500,
                    temperature=0.3,
                ),
                timeout=self.timeout,
            )

            return response.choices[0].message.content or ""

        except ImportError:
            raise RuntimeError("OpenAI package not installed")
        except Exception as e:
            logger.error(f"Direct OpenAI call failed: {e}")
            raise

    def _parse_json_response(self, response: str) -> dict[str, Any]:
        """Parse JSON from LLM response."""
        import json

        # Try to extract JSON from response
        text = response.strip()

        # Look for JSON block
        if "```json" in text:
            start = text.find("```json") + 7
            end = text.find("```", start)
            text = text[start:end].strip()
        elif "```" in text:
            start = text.find("```") + 3
            end = text.find("```", start)
            text = text[start:end].strip()

        # Try to find JSON object
        if "{" in text:
            start = text.find("{")
            end = text.rfind("}") + 1
            text = text[start:end]

        try:
            return json.loads(text)
        except json.JSONDecodeError:
            logger.debug(f"Failed to parse JSON response: {response[:200]}")
            return {}

    def _fallback_summary(self, text: str) -> dict[str, Any]:
        """Generate fallback summary without LLM."""
        # Take first 500 chars as summary
        summary = text[:500].strip()
        if len(text) > 500:
            # End at sentence boundary
            for end in [". ", "。", "! ", "? "]:
                idx = summary.rfind(end)
                if idx > 100:
                    summary = summary[: idx + 1]
                    break

        # Extract simple keywords (capitalized words, numbers)
        import re

        words = re.findall(r"\b[A-Z][a-z]+\b", text[:5000])
        keywords = list(set(words))[:10]

        return {
            "summary": summary,
            "keywords": keywords,
            "topics": [],
        }

    def _extract_first_sentence(self, text: str) -> str:
        """Extract first sentence from text."""
        text = text.strip()

        for end in [". ", "。", "! ", "? ", "\n"]:
            idx = text.find(end)
            if 10 < idx < 200:
                return text[: idx + 1].strip()

        return text[:200].strip()


class BatchSummaryGenerator:
    """
    Batch summary generator for processing multiple documents.

    Provides rate limiting and parallel processing.
    """

    def __init__(
        self,
        generator: SummaryGenerator,
        max_concurrent: int = 3,
        rate_limit_delay: float = 1.0,
    ):
        """
        Initialize batch generator.

        Args:
            generator: Base summary generator
            max_concurrent: Max concurrent LLM calls
            rate_limit_delay: Delay between calls in seconds
        """
        self.generator = generator
        self.semaphore = asyncio.Semaphore(max_concurrent)
        self.rate_limit_delay = rate_limit_delay

    async def summarize_documents(
        self,
        documents: list[dict[str, str]],
    ) -> list[dict[str, Any]]:
        """
        Summarize multiple documents.

        Args:
            documents: List of {"id": ..., "text": ...}

        Returns:
            List of summary results
        """

        async def process_one(doc: dict[str, str]) -> dict[str, Any]:
            async with self.semaphore:
                result = await self.generator.summarize_document(doc["text"])
                await asyncio.sleep(self.rate_limit_delay)
                return {"id": doc["id"], **result}

        tasks = [process_one(doc) for doc in documents]
        return await asyncio.gather(*tasks, return_exceptions=True)
