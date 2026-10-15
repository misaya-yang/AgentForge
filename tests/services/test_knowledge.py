"""
知识库检索算法测试

测试内容：
- 分词 (tokenize)
- BM25 评分
- RRF 融合
- MMR 多样化
"""

import pytest
from knowledge_service.services.knowledge.retrieval import (
    bm25_scores,
    mmr_select,
    reciprocal_rank_fusion,
    text_to_sparse_vector,
    tokenize,
)
from qdrant_client import models as qdrant_models
from qdrant_client.hybrid.fusion import reciprocal_rank_fusion as qdrant_rrf


class TestTokenize:
    """分词测试"""

    def test_tokenize_latin(self):
        """测试拉丁文分词"""
        assert tokenize("Hello, world!") == ["hello", "world"]

    def test_tokenize_cjk(self):
        """测试中日韩文分词 (bigrams)"""
        toks = tokenize("智能网关知识库")
        assert "智能" in toks
        assert "知识" in toks

    def test_tokenize_mixed(self):
        """测试混合文本分词"""
        toks = tokenize("AI网关Gateway")
        assert len(toks) > 0

    def test_lexical_v1_tokenization_golden_keeps_term_presence_schema(self):
        text = 'Alpha alpha "Beta Gamma" 智能智能'

        assert tokenize(text) == [
            "beta gamma",
            "alpha",
            "智能智能",
            "智能",
            "能智",
            "智",
            "能",
        ]
        assert text_to_sparse_vector(text) == (
            [
                787234666,
                1569418667,
                1732189608,
                2175947854,
                2278473498,
                2577206709,
                3279047699,
            ],
            [1.0] * 7,
        )


class TestBM25:
    """BM25 评分测试"""

    def test_bm25_prefers_matching_docs(self):
        """测试 BM25 优先匹配文档"""
        query = "vector search"
        q = tokenize(query)
        docs = [
            tokenize("this document talks about vector search and embeddings"),
            tokenize("nothing relevant here"),
            tokenize("search is important, but not about vectors"),
        ]

        scores = bm25_scores(q, docs)

        assert len(scores) == 3
        assert scores[0] > scores[1]  # 第一个文档得分更高
        assert scores[0] > scores[2]  # 第一个文档得分更高

    def test_bm25_empty_query(self):
        """测试空查询"""
        q = []
        docs = [tokenize("some document")]

        scores = bm25_scores(q, docs)
        assert len(scores) == 1

    def test_document_term_frequency_is_preserved_for_bm25(self):
        query = tokenize("vector")
        docs = [
            tokenize("vector", deduplicate=False),
            tokenize("vector vector vector", deduplicate=False),
        ]

        scores = bm25_scores(query, docs)

        assert scores[1] > scores[0]

    def test_lexical_v1_default_bm25_keeps_duplicate_terms_neutral(self):
        query = tokenize("vector")
        documents = [
            tokenize("vector"),
            tokenize("vector vector vector"),
        ]

        assert documents == [["vector"], ["vector"]]
        assert bm25_scores(query, documents) == pytest.approx(
            [0.1823215567939546, 0.1823215567939546]
        )


class TestRRF:
    """RRF 融合测试"""

    def test_rrf_fusion_rank_order(self):
        """测试 RRF 融合排序"""
        fused = reciprocal_rank_fusion(
            {"vector": ["a", "b", "c"], "keyword": ["b", "d", "a"]},
            k=60,
            weights={"vector": 1.0, "keyword": 1.0},
        )

        # 所有文档应该有得分
        assert fused["a"] > 0.0
        assert fused["b"] > 0.0
        assert fused["d"] > 0.0

        # b 在两个列表中排名都高，应该得分更高
        assert fused["b"] > fused["d"]

        # Golden lexical_v1 formula: rank is one-based and the denominator is
        # k + rank (not Qdrant weighted RRF's k - 1 + rank / weight).
        assert fused == pytest.approx(
            {
                "a": 1 / 61 + 1 / 63,
                "b": 1 / 62 + 1 / 61,
                "c": 1 / 63,
                "d": 1 / 62,
            }
        )

    def test_rrf_with_weights(self):
        """带权 RRF 使用 Qdrant 的 rank/weight 分母语义。"""
        fused = reciprocal_rank_fusion(
            {"vector": ["a", "b"], "keyword": ["b", "a"]},
            k=60,
            weights={"vector": 2.0, "keyword": 1.0},  # vector 权重更高
            qdrant_weighted=True,
        )

        assert fused["a"] == pytest.approx(1 / (1 / 2 + 59) + 1 / (2 / 1 + 59))
        assert fused["b"] == pytest.approx(1 / (2 / 2 + 59) + 1 / (1 / 1 + 59))

    def test_weighted_rrf_matches_qdrant_formula_and_ordering(self):
        """Fallback must preserve Qdrant's non-equal-weight rank semantics."""
        dense_ids = [3, 4, 5, 2]
        sparse_ids = [1]
        fallback = reciprocal_rank_fusion(
            {"dense": dense_ids, "bm25": sparse_ids},
            k=60,
            weights={"dense": 0.75, "bm25": 0.25},
            qdrant_weighted=True,
        )
        native = qdrant_rrf(
            [
                [
                    qdrant_models.ScoredPoint(id=item_id, version=0, score=1.0)
                    for item_id in dense_ids
                ],
                [
                    qdrant_models.ScoredPoint(id=item_id, version=0, score=1.0)
                    for item_id in sparse_ids
                ],
            ],
            limit=10,
            ranking_constant_k=60,
            weights=[0.75, 0.25],
        )

        native_scores = {int(point.id): point.score for point in native}
        assert fallback.keys() == native_scores.keys()
        for item_id, score in fallback.items():
            assert score == pytest.approx(native_scores[item_id])
        assert sorted(fallback, key=fallback.get, reverse=True) == [
            int(point.id) for point in native
        ]
        assert fallback[1] > fallback[2]

    def test_zero_weight_source_keeps_qdrant_zero_score_candidates(self):
        fallback = reciprocal_rank_fusion(
            {"dense": [2], "bm25": [1]},
            k=60,
            weights={"dense": 1.0, "bm25": 0.0},
            qdrant_weighted=True,
        )
        native = qdrant_rrf(
            [
                [qdrant_models.ScoredPoint(id=2, version=0, score=1.0)],
                [qdrant_models.ScoredPoint(id=1, version=0, score=1.0)],
            ],
            limit=10,
            ranking_constant_k=60,
            weights=[1.0, 0.0],
        )

        assert list(fallback) == [2, 1]
        assert fallback[2] == pytest.approx(native[0].score)
        assert fallback[1] == 0.0
        assert [int(point.id) for point in native] == [2, 1]


class TestMMR:
    """MMR 多样化测试"""

    def test_mmr_select_diversifies(self):
        """测试 MMR 选择多样化"""
        # 两个近似文档 (a,b) 和一个不同的 (c)
        candidates = ["a", "b", "c"]
        relevance = {"a": 1.0, "b": 0.99, "c": 0.8}
        vectors = {
            "a": [1.0, 0.0],
            "b": [0.99, 0.1],  # 与 a 非常相似
            "c": [0.0, 1.0],  # 与 a/b 不同
        }

        selected, picks = mmr_select(
            candidates=candidates,
            relevance=relevance,
            vectors=vectors,
            lambda_mult=0.5,
            top_k=2,
        )

        # 第一个应该是最相关的 a
        assert selected[0] == "a"
        # 第二个应该是多样化的 c（而不是相似的 b）
        assert selected[1] == "c"

    def test_mmr_with_high_lambda(self):
        """测试高 lambda 值（更重相关性）"""
        candidates = ["a", "b", "c"]
        relevance = {"a": 1.0, "b": 0.99, "c": 0.5}
        vectors = {
            "a": [1.0, 0.0],
            "b": [0.99, 0.1],
            "c": [0.0, 1.0],
        }

        selected, picks = mmr_select(
            candidates=candidates,
            relevance=relevance,
            vectors=vectors,
            lambda_mult=0.9,  # 高 lambda，更重相关性
            top_k=2,
        )

        # 高 lambda 时应该选择相关性高的
        assert "a" in selected
        assert "b" in selected  # b 相关性高于 c
