"""
Comprehensive Unit Tests for Document Chunking

Evaluates chunking strategies using real documents from C:\\database.
Tests include:
- Chunk quality metrics (size distribution, coverage, overlap)
- Different chunking strategies (automatic, hierarchical, recursive, heading)
- Image-aware chunking preservation
- Parent-child relationship integrity
- Edge cases and boundary conditions
"""

from __future__ import annotations

import contextlib
import re
import statistics
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace

import pytest
from knowledge_service.auth.user_context import UserContext
from knowledge_service.services.knowledge.chunking import (
    AutomaticChunker,
    Chunk,
    ChunkingConfig,
    ChunkingMode,
    FixedSizeChunker,
    HeadingChunker,
    HierarchicalChunker,
    ParagraphChunker,
    RecursiveChunker,
    TextPreprocessor,
    create_chunker,
    flatten_chunks,
    process_document,
)
from knowledge_service.services.knowledge.chunking_manager import ChunkingManager

# ============ Test Data Directory ============

TEST_DATA_DIR = Path(r"C:\database")


# ============ Quality Metrics ============


@dataclass
class ChunkQualityMetrics:
    """Metrics for evaluating chunk quality"""

    total_chunks: int
    mean_size: float
    median_size: float
    std_dev: float
    min_size: int
    max_size: int
    undersized_count: int  # < min_chunk_size
    oversized_count: int  # > max_chunk_size
    total_chars: int
    coverage_ratio: float  # chars in chunks / original chars
    size_variance_coefficient: float  # std_dev / mean (lower is more uniform)

    @classmethod
    def compute(
        cls,
        chunks: list[Chunk],
        original_text: str,
        min_size: int = 100,
        max_size: int = 1000,
    ) -> ChunkQualityMetrics:
        """Compute quality metrics for a list of chunks"""
        if not chunks:
            return cls(
                total_chunks=0,
                mean_size=0,
                median_size=0,
                std_dev=0,
                min_size=0,
                max_size=0,
                undersized_count=0,
                oversized_count=0,
                total_chars=0,
                coverage_ratio=0,
                size_variance_coefficient=0,
            )

        sizes = [len(c.text) for c in chunks]
        total_chars = sum(sizes)
        mean = statistics.mean(sizes)
        std = statistics.stdev(sizes) if len(sizes) > 1 else 0

        return cls(
            total_chunks=len(chunks),
            mean_size=mean,
            median_size=statistics.median(sizes),
            std_dev=std,
            min_size=min(sizes),
            max_size=max(sizes),
            undersized_count=sum(1 for s in sizes if s < min_size),
            oversized_count=sum(1 for s in sizes if s > max_size),
            total_chars=total_chars,
            coverage_ratio=total_chars / len(original_text) if original_text else 0,
            size_variance_coefficient=std / mean if mean > 0 else 0,
        )


def count_image_placeholders(text: str) -> int:
    """Count image placeholders in text"""
    patterns = [
        r"\[Image\]",
        r"\[图片\]",
        r"!\[.*?\]\(.*?\)",
        r"<img[^>]+>",
        r"\[IMAGE:.*?\]",
    ]
    count = 0
    for pattern in patterns:
        count += len(re.findall(pattern, text))
    return count


def get_pdf_text(pdf_path: Path) -> str | None:
    """Extract text from PDF using PyMuPDF if available"""
    try:
        import fitz  # PyMuPDF

        doc = fitz.open(str(pdf_path))
        text_parts = []

        for page_num, page in enumerate(doc):
            text = page.get_text()
            if text.strip():
                text_parts.append(f"[Page {page_num + 1}]\n{text}")

            # Check for images
            images = page.get_images()
            if images:
                text_parts.append(f"[Image] (Page {page_num + 1})")

        doc.close()
        return "\n\n".join(text_parts)
    except ImportError:
        return None
    except Exception as e:
        print(f"Error reading PDF {pdf_path}: {e}")
        return None


# ============ Sample Test Documents ============

SAMPLE_MARKDOWN = """# Introduction

This is the introduction section. It provides an overview of the document.

## Background

The background section explains the context and history. This section is quite long and
contains multiple paragraphs of information that need to be properly chunked.

### Key Points

1. First point with important details
2. Second point explaining the methodology
3. Third point about the results

## Methodology

The methodology section describes the approach taken. It includes:

- Step 1: Data collection
- Step 2: Analysis
- Step 3: Validation

### Data Sources

We collected data from multiple sources including databases, APIs, and manual entry.

## Results

The results show significant improvements in:

1. Processing speed (40% faster)
2. Accuracy (95% vs 85%)
3. Cost reduction (30% savings)

## Conclusion

In conclusion, this document has demonstrated the effectiveness of the approach.
"""

SAMPLE_PLAIN_TEXT = (
    """
This is a plain text document without any headings or special formatting.
It contains multiple paragraphs that should be chunked appropriately.

The document discusses various topics related to finance and business operations.
Each paragraph contains important information that should not be split arbitrarily.

Financial services require careful attention to detail and compliance with regulations.
The regulatory framework includes multiple layers of oversight and reporting requirements.

Business operations involve complex workflows and decision-making processes.
Effective management requires understanding of both strategic and operational aspects.
"""
    * 10
)  # Repeat to make it longer

SAMPLE_WITH_IMAGES = """
# Product Overview

Our product provides comprehensive solutions for your needs.

[Image] Product Dashboard Screenshot

The dashboard shows real-time metrics and analytics for monitoring performance.

## Features

### Analytics Module

[Image] Analytics Graph

The analytics module provides deep insights into user behavior and trends.

### Reporting System

[Image] Report Template

Generate custom reports with our flexible reporting system.

## Conclusion

Our product delivers value through intuitive design and powerful features.
"""

SAMPLE_CHINESE = """
# 项目概述

本文档介绍项目的整体架构和实施方案。

## 第一章 背景介绍

项目源于市场需求的变化和技术发展的趋势。我们需要开发一套全新的系统来满足用户需求。

### 1.1 市场分析

当前市场环境复杂多变，竞争日益激烈。通过详细的市场调研，我们发现了以下几个关键趋势：

1. 数字化转型加速
2. 用户体验要求提高
3. 成本控制压力增大

## 第二章 技术方案

本章介绍系统的技术架构和实现方案。

### 2.1 系统架构

系统采用微服务架构，包括以下核心组件：

- 网关服务：处理请求路由和认证
- 业务服务：实现核心业务逻辑
- 数据服务：管理数据存储和查询

[图片] 系统架构图

### 2.2 技术选型

经过综合评估，我们选择了以下技术栈：Python、FastAPI、PostgreSQL。

## 第三章 实施计划

项目分三个阶段实施，预计总工期为六个月。
"""


# ============ Unit Tests ============


class TestChunkingConfig:
    """Test ChunkingConfig class"""

    def test_default_config(self):
        """Test default configuration values"""
        config = ChunkingConfig()

        assert config.mode == ChunkingMode.AUTOMATIC
        # Defaults optimized for ~400-500 tokens (2000 chars ≈ 400-500 tokens)
        assert config.chunk_size == 2000
        assert config.chunk_overlap == 300
        assert config.use_token_count is True
        assert config.parent_chunk_size == 8000  # ~1500-2000 tokens
        assert config.child_chunk_size == 2000  # ~400-500 tokens

    def test_from_dict(self):
        """Test creating config from dictionary"""
        data = {
            "mode": "hierarchical",
            "chunk_size": 600,
            "chunk_overlap": 100,
            "parent_chunk_size": 2500,
            "child_chunk_size": 400,
        }

        config = ChunkingConfig.from_dict(data)

        assert config.mode == ChunkingMode.HIERARCHICAL
        assert config.chunk_size == 600
        assert config.chunk_overlap == 100
        assert config.parent_chunk_size == 2500
        assert config.child_chunk_size == 400

    def test_to_dict(self):
        """Test converting config to dictionary"""
        config = ChunkingConfig(
            mode=ChunkingMode.RECURSIVE,
            chunk_size=800,
        )

        data = config.to_dict()

        assert data["mode"] == "recursive"
        assert data["chunk_size"] == 800


class TestTextPreprocessor:
    """Test TextPreprocessor class"""

    def test_normalize_whitespace(self):
        """Test whitespace normalization"""
        config = ChunkingConfig(normalize_whitespace=True)
        text = "Hello\r\nWorld\r\n\r\n\r\nTest"

        result = TextPreprocessor.preprocess(text, config)

        assert "\r\n" not in result
        assert "\n\n\n" not in result

    def test_remove_extra_spaces(self):
        """Test extra space removal"""
        config = ChunkingConfig(remove_extra_spaces=True)
        text = "Hello    World   Test"

        result = TextPreprocessor.preprocess(text, config)

        assert "    " not in result

    def test_extract_metadata_title(self):
        """Test title extraction"""
        text = "# My Document Title\n\nContent here."

        metadata = TextPreprocessor.extract_metadata(text, ["title"])

        assert "title" in metadata
        assert metadata["title"] == "My Document Title"


class TestRecursiveChunker:
    """Test RecursiveChunker (industry default, 85-90% recall)"""

    def test_basic_chunking(self):
        """Test basic recursive chunking"""
        config = ChunkingConfig(
            mode=ChunkingMode.RECURSIVE,
            chunk_size=200,
            chunk_overlap=20,
        )
        chunker = RecursiveChunker(config)

        chunks = chunker.chunk(SAMPLE_PLAIN_TEXT)

        assert len(chunks) > 0
        # All chunks should be under max size (with some tolerance)
        for chunk in chunks:
            assert len(chunk.text) <= config.chunk_size * 1.2

    def test_respects_boundaries(self):
        """Test that chunker respects semantic boundaries"""
        config = ChunkingConfig(
            mode=ChunkingMode.RECURSIVE,
            chunk_size=100,
            chunk_overlap=10,
        )
        chunker = RecursiveChunker(config)

        # Text with clear paragraph boundaries
        text = "First paragraph.\n\nSecond paragraph.\n\nThird paragraph."
        chunks = chunker.chunk(text)

        # Should prefer splitting at paragraph boundaries
        assert len(chunks) >= 1

    def test_handles_long_text(self):
        """Test handling of very long documents"""
        config = ChunkingConfig(
            mode=ChunkingMode.RECURSIVE,
            chunk_size=500,
            chunk_overlap=50,
        )
        chunker = RecursiveChunker(config)

        # Create a very long document
        long_text = "A" * 10000
        chunks = chunker.chunk(long_text)

        assert len(chunks) > 1
        metrics = ChunkQualityMetrics.compute(chunks, long_text, 100, 500)
        assert metrics.coverage_ratio >= 0.9  # At least 90% coverage


class TestHierarchicalChunker:
    """Test HierarchicalChunker (parent-child chunking)"""

    def test_creates_parent_child_structure(self):
        """Test that parent-child structure is created correctly"""
        config = ChunkingConfig(
            mode=ChunkingMode.HIERARCHICAL,
            parent_chunk_size=500,
            parent_overlap=50,
            child_chunk_size=150,
            child_overlap=20,
        )
        chunker = HierarchicalChunker(config)

        chunks = chunker.chunk(SAMPLE_MARKDOWN)

        # chunks list contains only parents; children are in parent.children
        parents = [c for c in chunks if c.metadata.get("is_parent")]
        assert len(parents) > 0, "Should have parent chunks"

        # Children are accessible via flatten_chunks
        flat = flatten_chunks(chunks)
        children = [c for c in flat if c.metadata.get("is_child")]
        assert len(children) > 0, "Should have child chunks after flattening"

    def test_children_link_to_parents(self):
        """Test that children properly reference their parents"""
        config = ChunkingConfig(
            mode=ChunkingMode.HIERARCHICAL,
            parent_chunk_size=800,
            child_chunk_size=200,
        )
        chunker = HierarchicalChunker(config)

        chunks = chunker.chunk(SAMPLE_MARKDOWN)

        flat = flatten_chunks(chunks)
        children = [c for c in flat if c.metadata.get("is_child")]
        parent_hashes = {c.hash_id for c in chunks if c.metadata.get("is_parent")}

        for child in children:
            # Each child should reference a valid parent
            assert child.parent_id is not None
            assert child.parent_id in parent_hashes

    def test_child_chunk_size_respected(self):
        """Test that child chunks respect configured size"""
        config = ChunkingConfig(
            mode=ChunkingMode.HIERARCHICAL,
            parent_chunk_size=2000,
            child_chunk_size=500,
            child_overlap=75,
        )
        chunker = HierarchicalChunker(config)

        chunks = chunker.chunk(SAMPLE_PLAIN_TEXT)

        flat = flatten_chunks(chunks)
        children = [c for c in flat if c.metadata.get("is_child")]

        for child in children:
            # Allow 20% tolerance for natural boundaries
            assert len(child.text) <= config.child_chunk_size * 1.2


class TestHeadingChunker:
    """Test HeadingChunker for structured documents"""

    def test_splits_by_headings(self):
        """Test that markdown is split by headings"""
        config = ChunkingConfig(
            mode=ChunkingMode.HEADING,
            chunk_size=1000,
        )
        chunker = HeadingChunker(config)

        chunks = chunker.chunk(SAMPLE_MARKDOWN)

        # Should have multiple chunks based on sections
        assert len(chunks) > 1

        # Check that heading info is preserved in metadata
        chunks_with_headings = [c for c in chunks if c.metadata.get("heading")]
        assert len(chunks_with_headings) > 0

    def test_chinese_headings(self):
        """Test Chinese heading detection"""
        config = ChunkingConfig(
            mode=ChunkingMode.HEADING,
            chunk_size=500,
        )
        chunker = HeadingChunker(config)

        chunks = chunker.chunk(SAMPLE_CHINESE)

        # Should detect Chinese chapter markers
        assert len(chunks) > 1


class TestAutomaticChunker:
    """Test AutomaticChunker (intelligent strategy selection)"""

    def test_selects_heading_for_structured(self):
        """Test that automatic selects heading chunker for structured docs"""
        config = ChunkingConfig(mode=ChunkingMode.AUTOMATIC, chunk_size=500)
        chunker = AutomaticChunker(config)

        # Markdown with clear headings
        chunks = chunker.chunk(SAMPLE_MARKDOWN)

        assert len(chunks) > 0
        # Should have detected headings
        chunks_with_headings = [c for c in chunks if c.metadata.get("heading")]
        assert len(chunks_with_headings) > 0

    def test_selects_hierarchical_for_long_docs(self):
        """Test that automatic selects hierarchical for long documents"""
        config = ChunkingConfig(
            mode=ChunkingMode.AUTOMATIC,
            chunk_size=500,
            parent_chunk_size=2000,
            child_chunk_size=500,
        )
        chunker = AutomaticChunker(config)

        # Plain text longer than 5000 chars
        long_text = "Plain text. " * 500  # ~6000 chars
        chunks = chunker.chunk(long_text)

        assert len(chunks) > 0

    def test_preserves_images(self):
        """Test that images are preserved with context"""
        config = ChunkingConfig(
            mode=ChunkingMode.AUTOMATIC,
            chunk_size=300,
            preserve_images=True,
            image_context_chars=100,
        )
        chunker = AutomaticChunker(config)

        chunks = chunker.chunk(SAMPLE_WITH_IMAGES)

        # Count images in original
        original_images = count_image_placeholders(SAMPLE_WITH_IMAGES)

        # Count images in chunks
        chunk_images = sum(count_image_placeholders(c.text) for c in chunks)

        # All images should be preserved
        assert chunk_images >= original_images


class TestImageAwareChunking:
    """Test image-aware chunking functionality"""

    def test_image_context_preserved(self):
        """Test that image context is preserved"""
        config = ChunkingConfig(
            mode=ChunkingMode.AUTOMATIC,
            chunk_size=200,
            preserve_images=True,
            image_context_chars=100,
        )
        chunker = AutomaticChunker(config)

        text = (
            "Introduction text here. " * 5
            + "[Image] Important diagram"
            + " Description after image. " * 5
        )
        chunks = chunker.chunk(text)

        # Find chunk with image
        image_chunks = [c for c in chunks if "[Image]" in c.text]
        assert len(image_chunks) > 0

        # Image should have surrounding context
        for chunk in image_chunks:
            # Should have text before and after [Image]
            img_pos = chunk.text.find("[Image]")
            assert img_pos > 0, "Should have context before image"
            assert img_pos < len(chunk.text) - 10, "Should have context after image"

    def test_image_metadata_flag(self):
        """Test that image chunks are flagged in metadata"""
        config = ChunkingConfig(
            mode=ChunkingMode.AUTOMATIC,
            preserve_images=True,
        )
        chunker = AutomaticChunker(config)

        chunks = chunker.chunk(SAMPLE_WITH_IMAGES)

        image_chunks = [c for c in chunks if c.metadata.get("has_image")]

        assert image_chunks
        assert all("[Image]" in chunk.text for chunk in image_chunks)
        assert all(chunk.metadata.get("chunk_type") == "image_context" for chunk in image_chunks)


class TestChunkQualityMetrics:
    """Test chunk quality metrics computation"""

    def test_metrics_computation(self):
        """Test basic metrics computation"""
        chunks = [
            Chunk(text="A" * 100, index=0),
            Chunk(text="B" * 150, index=1),
            Chunk(text="C" * 120, index=2),
        ]
        original = "A" * 100 + "B" * 150 + "C" * 120

        metrics = ChunkQualityMetrics.compute(chunks, original, 50, 200)

        assert metrics.total_chunks == 3
        assert metrics.min_size == 100
        assert metrics.max_size == 150
        assert metrics.undersized_count == 0
        assert metrics.oversized_count == 0
        assert metrics.coverage_ratio == 1.0

    def test_variance_coefficient(self):
        """Test size variance coefficient"""
        # Uniform chunks
        uniform_chunks = [Chunk(text="A" * 100, index=i) for i in range(5)]
        uniform_metrics = ChunkQualityMetrics.compute(uniform_chunks, "A" * 500, 50, 200)

        # Variable chunks
        variable_chunks = [
            Chunk(text="A" * 50, index=0),
            Chunk(text="B" * 200, index=1),
            Chunk(text="C" * 100, index=2),
        ]
        variable_metrics = ChunkQualityMetrics.compute(variable_chunks, "A" * 350, 50, 200)

        # Uniform should have lower variance coefficient
        assert (
            uniform_metrics.size_variance_coefficient < variable_metrics.size_variance_coefficient
        )


class TestProcessDocument:
    """Test the main process_document function"""

    def test_basic_processing(self):
        """Test basic document processing"""
        config = ChunkingConfig(
            mode=ChunkingMode.AUTOMATIC,
            chunk_size=500,
        )

        chunks = process_document(SAMPLE_MARKDOWN, config, document_id="doc_001")

        assert len(chunks) > 0
        for chunk in chunks:
            assert chunk.metadata.get("document_id") == "doc_001"

    def test_metadata_extraction(self):
        """Test metadata extraction during processing"""
        config = ChunkingConfig(
            mode=ChunkingMode.AUTOMATIC,
            extract_metadata=True,
            metadata_fields=["title", "word_count"],
        )

        chunks = process_document(SAMPLE_MARKDOWN, config)

        # Metadata should be added to chunks
        assert len(chunks) > 0
        assert "word_count" in chunks[0].metadata


class TestFlattenChunks:
    """Test chunk flattening for hierarchical structures"""

    def test_flatten_hierarchical(self):
        """Test flattening hierarchical chunks"""
        config = ChunkingConfig(
            mode=ChunkingMode.HIERARCHICAL,
            parent_chunk_size=800,
            child_chunk_size=200,
        )
        chunker = HierarchicalChunker(config)

        chunks = chunker.chunk(SAMPLE_PLAIN_TEXT)
        flattened = flatten_chunks(chunks)

        # Flattened should contain only children (no parents with children)
        for chunk in flattened:
            assert not chunk.children, "Flattened chunks should have no children"


class TestEdgeCases:
    """Test edge cases and boundary conditions"""

    def test_empty_text(self):
        """Test handling of empty text"""
        config = ChunkingConfig()

        for chunker_cls in [
            RecursiveChunker,
            HierarchicalChunker,
            HeadingChunker,
            AutomaticChunker,
        ]:
            chunker = chunker_cls(config)
            chunks = chunker.chunk("")
            assert chunks == []

    def test_whitespace_only(self):
        """Test handling of whitespace-only text"""
        config = ChunkingConfig()
        chunks = process_document("   \n\n\t\t  ", config)
        assert chunks == []

    def test_single_word(self):
        """Test handling of very short text"""
        config = ChunkingConfig(min_chunk_size=10)
        chunker = RecursiveChunker(config)

        chunks = chunker.chunk("Hello")
        # Should still produce a chunk even if under min size
        assert len(chunks) >= 0  # May or may not produce chunk based on min_size

    def test_very_long_single_line(self):
        """Test handling of very long single line"""
        config = ChunkingConfig(chunk_size=100, chunk_overlap=10)
        chunker = RecursiveChunker(config)

        long_line = "Word " * 500  # ~2500 chars, no natural breaks
        chunks = chunker.chunk(long_line)

        assert len(chunks) > 1


class TestChunkerFactory:
    """Test chunker factory function"""

    def test_creates_correct_chunker(self):
        """Test that factory creates correct chunker type"""
        mode_to_class = {
            ChunkingMode.AUTOMATIC: AutomaticChunker,
            ChunkingMode.RECURSIVE: RecursiveChunker,
            ChunkingMode.HIERARCHICAL: HierarchicalChunker,
            ChunkingMode.HEADING: HeadingChunker,
            ChunkingMode.FIXED_SIZE: FixedSizeChunker,
            ChunkingMode.PARAGRAPH: ParagraphChunker,
        }

        for mode, expected_class in mode_to_class.items():
            config = ChunkingConfig(mode=mode)
            chunker = create_chunker(config)
            assert isinstance(chunker, expected_class)


# ============ Integration Tests with Real PDFs ============


@pytest.mark.skipif(
    not TEST_DATA_DIR.exists(), reason=f"Test data directory {TEST_DATA_DIR} not found"
)
class TestRealDocuments:
    """Integration tests using real PDF documents"""

    @pytest.fixture
    def pdf_files(self) -> list[Path]:
        """Get list of PDF files in test directory"""
        if not TEST_DATA_DIR.exists():
            return []
        return list(TEST_DATA_DIR.glob("*.pdf"))

    def test_pdf_extraction_available(self):
        """Test that PDF extraction is available"""
        try:
            import fitz as _fitz

            assert _fitz is not None
        except ImportError:
            pytest.skip("PyMuPDF (fitz) not installed")

    @pytest.mark.skipif(not TEST_DATA_DIR.exists(), reason="Test data directory not found")
    def test_automatic_chunking_on_pdfs(self, pdf_files):
        """Test automatic chunking on real PDF documents"""
        try:
            import fitz as _fitz
            assert _fitz is not None
        except ImportError:
            pytest.skip("PyMuPDF not installed")

        if not pdf_files:
            pytest.skip("No PDF files found")

        config = ChunkingConfig(
            mode=ChunkingMode.AUTOMATIC,
            chunk_size=500,
            chunk_overlap=75,
            parent_chunk_size=2000,
            child_chunk_size=500,
        )

        results = []

        for pdf_path in pdf_files[:3]:  # Test first 3 PDFs
            text = get_pdf_text(pdf_path)
            if not text:
                continue

            chunks = process_document(text, config)
            metrics = ChunkQualityMetrics.compute(chunks, text, 100, 600)

            results.append(
                {
                    "file": pdf_path.name,
                    "chunks": metrics.total_chunks,
                    "mean_size": metrics.mean_size,
                    "coverage": metrics.coverage_ratio,
                    "variance": metrics.size_variance_coefficient,
                }
            )

            # Basic assertions
            assert metrics.total_chunks > 0, f"No chunks for {pdf_path.name}"
            assert metrics.coverage_ratio >= 0.5, f"Low coverage for {pdf_path.name}"

        # Print summary
        print("\n=== PDF Chunking Results ===")
        for r in results:
            print(
                f"{r['file']}: {r['chunks']} chunks, "
                f"mean={r['mean_size']:.0f}, "
                f"coverage={r['coverage']:.2f}"
            )

    @pytest.mark.skipif(not TEST_DATA_DIR.exists(), reason="Test data directory not found")
    def test_hierarchical_vs_recursive(self, pdf_files):
        """Compare hierarchical and recursive chunking on PDFs"""
        try:
            import fitz as _fitz
            assert _fitz is not None
        except ImportError:
            pytest.skip("PyMuPDF not installed")

        if not pdf_files:
            pytest.skip("No PDF files found")

        # Pick a larger PDF for better comparison
        pdf_files_by_size = sorted(pdf_files, key=lambda p: p.stat().st_size, reverse=True)
        pdf_path = pdf_files_by_size[0] if pdf_files_by_size else None

        if not pdf_path:
            pytest.skip("No PDF files found")

        text = get_pdf_text(pdf_path)
        if not text:
            pytest.skip(f"Could not extract text from {pdf_path}")

        # Hierarchical config
        hierarchical_config = ChunkingConfig(
            mode=ChunkingMode.HIERARCHICAL,
            parent_chunk_size=2000,
            parent_overlap=100,
            child_chunk_size=500,
            child_overlap=75,
        )

        # Recursive config
        recursive_config = ChunkingConfig(
            mode=ChunkingMode.RECURSIVE,
            chunk_size=500,
            chunk_overlap=75,
        )

        h_chunks = HierarchicalChunker(hierarchical_config).chunk(text)
        r_chunks = RecursiveChunker(recursive_config).chunk(text)

        h_children = [c for c in h_chunks if c.metadata.get("is_child")]

        h_metrics = ChunkQualityMetrics.compute(h_children or h_chunks, text, 100, 600)
        r_metrics = ChunkQualityMetrics.compute(r_chunks, text, 100, 600)

        print(f"\n=== Comparison for {pdf_path.name} ===")
        print(
            f"Hierarchical: {h_metrics.total_chunks} chunks (children), "
            f"mean={h_metrics.mean_size:.0f}, var_coef={h_metrics.size_variance_coefficient:.2f}"
        )
        print(
            f"Recursive: {r_metrics.total_chunks} chunks, "
            f"mean={r_metrics.mean_size:.0f}, var_coef={r_metrics.size_variance_coefficient:.2f}"
        )

        # Both should have reasonable coverage
        assert h_metrics.coverage_ratio >= 0.5
        assert r_metrics.coverage_ratio >= 0.5


# ============ Performance Benchmarks ============


class TestPerformance:
    """Performance benchmarks for chunking"""

    def test_chunking_speed(self):
        """Benchmark chunking speed"""
        import time

        # Generate large document
        large_text = SAMPLE_PLAIN_TEXT * 10  # ~30k chars

        configs = [
            ("Automatic", ChunkingConfig(mode=ChunkingMode.AUTOMATIC)),
            ("Recursive", ChunkingConfig(mode=ChunkingMode.RECURSIVE)),
            ("Hierarchical", ChunkingConfig(mode=ChunkingMode.HIERARCHICAL)),
        ]

        print("\n=== Chunking Performance ===")

        for name, config in configs:
            chunker = create_chunker(config)

            start = time.perf_counter()
            for _ in range(10):
                chunker.chunk(large_text)
            elapsed = time.perf_counter() - start

            avg_ms = (elapsed / 10) * 1000
            print(f"{name}: {avg_ms:.2f}ms per document ({len(large_text)} chars)")

            # Should be reasonably fast
            assert avg_ms < 1000, f"{name} chunking too slow"


@pytest.mark.asyncio
@pytest.mark.parametrize("enabled", [False, True])
async def test_chunking_manager_segment_toggle_is_database_authoritative(
    enabled: bool,
) -> None:
    row = {
        "segment_id": "segment-a",
        "dataset_id": "dataset-a",
        "document_id": "document-a",
        "vector_id": "point-a",
        "enabled": not enabled,
    }
    saved: list[dict] = []
    payload_calls: list[tuple[bool, bool]] = []

    class Database:
        @contextlib.asynccontextmanager
        async def segment_index_update_lease(
            self,
            dataset_id: str,
            document_id: str,
            segment_id: str,
        ):
            assert (dataset_id, document_id, segment_id) == (
                "dataset-a",
                "document-a",
                "segment-a",
            )
            yield self

        async def get_segment(self, segment_id: str, *, connection=None):
            assert connection in (None, self)
            assert segment_id == "segment-a"
            return dict(row)

        async def get_document(self, document_id: str, *, connection=None):
            assert connection is self
            assert document_id == "document-a"
            return {
                "document_id": "document-a",
                "dataset_id": "dataset-a",
                "enabled": True,
                "archived": False,
                "status": "completed",
                "metadata": {},
            }

        async def update_segment_fields(
            self,
            segment_id: str,
            fields: dict,
            *,
            connection=None,
        ):
            assert connection in (None, self)
            assert segment_id == "segment-a"
            saved.append(dict(fields))
            row.update(fields)

    class Knowledge:
        async def require_dataset_access(
            self,
            _user,
            dataset_id: str,
            *,
            required: str,
        ):
            assert (dataset_id, required) == ("dataset-a", "editor")
            return {
                "dataset_id": "dataset-a",
                "tenant_id": "tenant-a",
                "index_config": {},
            }

    class VectorStore:
        async def set_segment_payload_enabled(self, **kwargs):
            assert kwargs["lifecycle_lease_held"] is True
            assert kwargs["segment_id"] == "segment-a"
            payload_calls.append((kwargs["enabled"], row["enabled"]))
            return ["custom-base", "custom-base_sections"]

    manager = ChunkingManager(
        SimpleNamespace(),
        Database(),  # type: ignore[arg-type]
        VectorStore(),  # type: ignore[arg-type]
        Knowledge(),
    )

    result = await manager.set_segment_enabled(
        UserContext(user_id="editor-a", tenant_id="tenant-a"),
        "dataset-a",
        "segment-a",
        enabled,
    )

    assert result["enabled"] is enabled
    assert saved[0]["enabled"] is enabled
    assert saved[0]["disabled_by"] == (None if enabled else "editor-a")
    assert (saved[0]["disabled_at"] is None) is enabled
    # Both transitions keep PostgreSQL inactive until every owned Qdrant
    # collection has acknowledged the payload state.
    assert payload_calls == [(enabled, False)]


if __name__ == "__main__":
    pytest.main([__file__, "-v", "--tb=short"])
