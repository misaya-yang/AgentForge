#!/usr/bin/env python3
"""
知识库问答测试脚本

测试入库后的检索和问答功能。

用法:
    python test_kb_qa.py --dataset agent --query "release rollback"
    python test_kb_qa.py --dataset agent --test-file test_questions.txt
    python test_kb_qa.py --dataset agent --interactive
"""

import argparse
import asyncio
import os
import sys

import httpx

DEFAULT_BASE_URL = os.getenv("GATEWAY_BASE_URL", "http://localhost:8080")
DEFAULT_API_KEY = os.getenv("GATEWAY_API_KEY", "")


class KBTester:
    def __init__(self, base_url: str, api_key: str, dataset_id: str):
        self.base_url = base_url
        self.api_key = api_key
        self.dataset_id = dataset_id
        self.client = httpx.AsyncClient(
            base_url=base_url,
            headers={
                "X-API-Key": api_key,
                "Accept": "application/json",
                "Content-Type": "application/json",
            },
            timeout=60.0,
        )

    async def test_retrieval(
        self, query: str, top_k: int = 5, mode: str = "hybrid", include_images: bool = False
    ) -> bool:
        """Test retrieval endpoint."""
        print("\n🔍 Retrieval Test")
        print(f"   Query: '{query}'")
        print(f"   Mode: {mode}, top_k: {top_k}")

        try:
            payload = {
                "query": query,
                "top_k": top_k,
                "mode": mode,
                "include_associated_images": include_images,
            }

            resp = await self.client.post(
                f"/api/v1/knowledge/{self.dataset_id}/hit_test", json=payload
            )

            if resp.status_code == 200:
                data = resp.json()
                results = data.get("results", [])
                metadata = data.get("metadata", {})

                print(f"✅ Success! Found {len(results)} results")
                print(f"   Retrieval mode: {metadata.get('mode', 'unknown')}")
                print(f"   Total results: {metadata.get('total_results', 0)}")

                for i, r in enumerate(results[:3], 1):
                    text = r.get("text", "")[:120].replace("\n", " ")
                    score = r.get("score", 0)
                    content_type = r.get("content_type", "text")
                    print(f"\n   [{i}] Score: {score:.3f} | Type: {content_type}")
                    print(f"       {text}...")

                return True
            else:
                print(f"❌ Failed: {resp.status_code}")
                print(f"   {resp.text[:200]}")
                return False

        except Exception as e:
            print(f"❌ Error: {e}")
            return False

    async def test_retrieve_with_images(self, query: str, top_k: int = 5) -> bool:
        """Test multimodal retrieval with images."""
        print("\n🖼️  Multimodal Retrieval Test")
        print(f"   Query: '{query}'")

        try:
            payload = {
                "query": query,
                "top_k": top_k,
                "mode": "hybrid",
                "include_associated_images": True,
            }

            resp = await self.client.post(
                f"/api/v1/knowledge/{self.dataset_id}/retrieve", json=payload
            )

            if resp.status_code == 200:
                data = resp.json()
                results = data.get("results", [])

                print(f"✅ Success! Found {len(results)} results")

                image_count = sum(1 for r in results if r.get("content_type") == "image")
                text_count = len(results) - image_count

                print(f"   Text segments: {text_count}")
                print(f"   Image segments: {image_count}")

                for i, r in enumerate(results[:3], 1):
                    content_type = r.get("content_type", "text")
                    score = r.get("score", 0)

                    if content_type == "image":
                        vlm_desc = r.get("vlm_description", "")[:80]
                        print(f"\n   [{i}] 🖼️  Image (score: {score:.3f})")
                        print(f"       VLM: {vlm_desc}...")
                    else:
                        text = r.get("text", "")[:100].replace("\n", " ")
                        assoc_images = len(r.get("associated_images", []))
                        img_info = f" (+{assoc_images} images)" if assoc_images else ""
                        print(f"\n   [{i}] 📝 Text (score: {score:.3f}){img_info}")
                        print(f"       {text}...")

                return True
            else:
                print(f"❌ Failed: {resp.status_code}")
                return False

        except Exception as e:
            print(f"❌ Error: {e}")
            return False

    async def list_documents(self) -> bool:
        """List documents in dataset."""
        print(f"\n📄 Documents in dataset '{self.dataset_id}':")

        try:
            resp = await self.client.get(f"/api/v1/knowledge/{self.dataset_id}/documents")

            if resp.status_code == 200:
                docs = resp.json()
                if not docs:
                    print("   No documents found")
                    return True

                print(f"   Total: {len(docs)} documents\n")

                for doc in docs:
                    doc_id = doc.get("document_id", "N/A")[:8]
                    title = doc.get("title", "Untitled")[:40]
                    status = doc.get("status", "unknown")
                    seg_count = doc.get("segment_count", 0)

                    status_icon = {
                        "completed": "✅",
                        "failed": "❌",
                        "processing": "⏳",
                        "uploaded": "📤",
                    }.get(status, "❓")

                    print(f"   {status_icon} [{doc_id}...] {title}")
                    print(f"      Status: {status}, Segments: {seg_count}")

                return True
            else:
                print(f"❌ Failed: {resp.status_code}")
                return False

        except Exception as e:
            print(f"❌ Error: {e}")
            return False

    async def interactive_mode(self):
        """Interactive Q&A mode."""
        print("\n" + "=" * 60)
        print("💬 Interactive Knowledge Base Q&A")
        print("=" * 60)
        print("Dataset:", self.dataset_id)
        print("Commands:")
        print("  /retrieval <query>  - Test retrieval")
        print("  /images <query>     - Test multimodal retrieval")
        print("  /docs               - List documents")
        print("  /quit               - Exit")
        print("=" * 60)

        while True:
            try:
                user_input = input("\n> ").strip()

                if not user_input:
                    continue

                if user_input.lower() == "/quit":
                    break

                if user_input.lower() == "/docs":
                    await self.list_documents()
                    continue

                if user_input.lower().startswith("/retrieval "):
                    query = user_input[11:]
                    await self.test_retrieval(query)
                    continue

                if user_input.lower().startswith("/images "):
                    query = user_input[8:]
                    await self.test_retrieve_with_images(query)
                    continue

                # Default: treat as retrieval query
                await self.test_retrieval(user_input)

            except KeyboardInterrupt:
                break
            except EOFError:
                break

        print("\n👋 Goodbye!")

    async def close(self):
        await self.client.aclose()


async def main():
    parser = argparse.ArgumentParser(description="Test knowledge base Q&A")
    parser.add_argument("--dataset", "-d", required=True, help="Dataset ID to test")
    parser.add_argument("--query", "-q", help="Single query to test")
    parser.add_argument("--test-file", "-f", help="File with test questions (one per line)")
    parser.add_argument("--interactive", "-i", action="store_true", help="Interactive mode")
    parser.add_argument("--top-k", "-k", type=int, default=5, help="Number of results to retrieve")
    parser.add_argument(
        "--mode",
        "-m",
        default="hybrid",
        choices=["vector", "keyword", "hybrid"],
        help="Retrieval mode",
    )
    parser.add_argument(
        "--multimodal", "-M", action="store_true", help="Test multimodal retrieval with images"
    )
    parser.add_argument(
        "--base-url",
        "-u",
        default=DEFAULT_BASE_URL,
        help=f"API base URL (default: {DEFAULT_BASE_URL})",
    )
    parser.add_argument("--api-key", "-k", default=DEFAULT_API_KEY, help="API Key")
    parser.add_argument("--list-docs", "-l", action="store_true", help="List documents and exit")

    args = parser.parse_args()

    tester = KBTester(args.base_url, args.api_key, args.dataset)

    try:
        if args.list_docs:
            await tester.list_documents()
            return

        if args.interactive:
            await tester.interactive_mode()
            return

        if args.query:
            if args.multimodal:
                await tester.test_retrieve_with_images(args.query, args.top_k)
            else:
                await tester.test_retrieval(args.query, args.top_k, args.mode)
            return

        if args.test_file:
            if not os.path.exists(args.test_file):
                print(f"❌ File not found: {args.test_file}")
                sys.exit(1)

            with open(args.test_file) as f:
                questions = [line.strip() for line in f if line.strip()]

            print(f"Running {len(questions)} test questions...")
            for q in questions:
                await tester.test_retrieval(q, args.top_k, args.mode)
            return

        # No specific action, show help
        parser.print_help()

    finally:
        await tester.close()


if __name__ == "__main__":
    asyncio.run(main())
