#!/usr/bin/env python3
"""
批量文档入库脚本

支持批量上传 PDF 到知识库并完成索引。

用法:
    uv run python tests/knowledge/tools/batch_ingest.py --dataset agent --files "/path/to/*.pdf"
    uv run python tests/knowledge/tools/batch_ingest.py --dataset agent --dir /path/to/documents/
    uv run python tests/knowledge/tools/batch_ingest.py --dataset agent --files "*.pdf" --concurrency 3

环境变量:
    GATEWAY_API_KEY - API Key for authentication
    GATEWAY_BASE_URL - API base URL (default: http://localhost:8080)
"""

import argparse
import asyncio
import glob
import os
import sys
import time
from pathlib import Path
from typing import Any

import httpx

# Configuration
DEFAULT_BASE_URL = os.getenv("GATEWAY_BASE_URL", "http://localhost:8080")
DEFAULT_API_KEY = os.getenv("GATEWAY_API_KEY")  # MUST be set via environment variable


class BatchIngestor:
    def __init__(self, base_url: str, api_key: str, dataset_id: str):
        self.base_url = base_url
        self.api_key = api_key
        self.dataset_id = dataset_id
        self.client = httpx.AsyncClient(
            base_url=base_url,
            headers={
                "X-API-Key": api_key,
                "Accept": "application/json",
            },
            timeout=300.0,  # 5 minutes for large file uploads
        )
        self.results: list[dict[str, Any]] = []

    async def check_dataset(self) -> bool:
        """Verify dataset exists."""
        try:
            resp = await self.client.get(f"/api/v1/knowledge/datasets/{self.dataset_id}")
            if resp.status_code == 200:
                data = resp.json()
                print(f"✅ Dataset '{self.dataset_id}' ready: {data.get('name')}")
                return True
            elif resp.status_code == 404:
                print(f"❌ Dataset '{self.dataset_id}' not found")
                print("   Create it first with: POST /knowledge/datasets")
                return False
            else:
                print(f"❌ Failed to check dataset: {resp.status_code}")
                return False
        except Exception as e:
            print(f"❌ Error checking dataset: {e}")
            return False

    async def upload_file(self, file_path: Path) -> str | None:
        """Upload a single file and return document_id."""
        start_time = time.time()

        try:
            print(
                f"  📤 Uploading {file_path.name} ({file_path.stat().st_size / 1024 / 1024:.2f} MB)...",
                end=" ",
            )

            with open(file_path, "rb") as f:
                files = {"file": (file_path.name, f, "application/pdf")}
                resp = await self.client.post(
                    f"/api/v1/knowledge/{self.dataset_id}/documents/upload",
                    files=files,
                )

            if resp.status_code == 200:
                data = resp.json()
                doc_id = data.get("document_id")
                elapsed = time.time() - start_time
                print(f"✅ ({elapsed:.1f}s) -> {doc_id[:8]}...")
                return doc_id
            elif resp.status_code == 401:
                print("❌ Authentication failed (401)")
                print("   Check your API key")
                return None
            elif resp.status_code == 404:
                print("❌ Dataset not found (404)")
                return None
            elif resp.status_code == 413:
                print("❌ File too large (413)")
                return None
            else:
                print(f"❌ Failed: {resp.status_code}")
                try:
                    error_data = resp.json()
                    print(f"   Error: {error_data.get('detail', resp.text[:200])}")
                except ValueError:
                    print(f"   Response: {resp.text[:200]}")
                return None

        except httpx.TimeoutException:
            print("❌ Timeout: Request took too long")
            return None
        except httpx.ConnectError:
            print(f"❌ Connection failed: Cannot reach API server at {self.base_url}")
            return None
        except FileNotFoundError:
            print(f"❌ File not found: {file_path}")
            return None
        except PermissionError:
            print(f"❌ Permission denied: Cannot read {file_path}")
            return None
        except Exception as e:
            print(f"❌ Unexpected error: {type(e).__name__}: {e}")
            return None

    async def wait_for_document(self, document_id: str, max_wait: int = 600) -> bool:
        """Wait for document processing to complete."""
        start_time = time.time()
        check_interval = 5
        consecutive_errors = 0
        max_consecutive_errors = 3

        while time.time() - start_time < max_wait:
            try:
                resp = await self.client.get(
                    f"/api/v1/knowledge/{self.dataset_id}/documents/{document_id}"
                )

                # Reset error counter on successful request
                consecutive_errors = 0

                if resp.status_code == 200:
                    data = resp.json()
                    status = data.get("status")
                    progress = data.get("progress", 0)

                    if status == "completed":
                        segment_count = data.get("segment_count", 0)
                        print(f"     ✅ Completed with {segment_count} segments")
                        return True
                    elif status == "failed":
                        error = data.get("error", "Unknown error")
                        print(f"     ❌ Failed: {error}")
                        return False
                    else:
                        # Still processing, show progress if available
                        if progress > 0:
                            print(f"     ⏳ Processing... {progress}%", end="\r")
                elif resp.status_code == 404:
                    print("     ❌ Document not found")
                    return False
                else:
                    print(f"     ⚠️ Status check failed: {resp.status_code}")

                await asyncio.sleep(check_interval)

            except httpx.TimeoutException:
                consecutive_errors += 1
                print(
                    f"     ⚠️ Timeout checking status (attempt {consecutive_errors}/{max_consecutive_errors})"
                )
                if consecutive_errors >= max_consecutive_errors:
                    print("     ❌ Too many consecutive errors, giving up")
                    return False
                await asyncio.sleep(check_interval)
            except Exception as e:
                consecutive_errors += 1
                print(f"     ⚠️ Check error: {type(e).__name__}: {e}")
                if consecutive_errors >= max_consecutive_errors:
                    print("     ❌ Too many consecutive errors, giving up")
                    return False
                await asyncio.sleep(check_interval)

        print(f"     ⏱️ Timeout after {max_wait}s")
        return False

    async def process_batch(
        self, files: list[Path], concurrency: int = 2, wait_for_completion: bool = True
    ):
        """Process a batch of files with controlled concurrency."""
        semaphore = asyncio.Semaphore(concurrency)

        async def process_one(file_path: Path):
            async with semaphore:
                doc_id = await self.upload_file(file_path)
                if doc_id and wait_for_completion:
                    success = await self.wait_for_document(doc_id)
                    return {
                        "file": str(file_path),
                        "document_id": doc_id,
                        "success": success,
                    }
                elif doc_id:
                    return {
                        "file": str(file_path),
                        "document_id": doc_id,
                        "success": True,
                    }
                return {
                    "file": str(file_path),
                    "document_id": None,
                    "success": False,
                }

        tasks = [process_one(f) for f in files]
        self.results = await asyncio.gather(*tasks)

    def print_summary(self):
        """Print batch processing summary."""
        total = len(self.results)
        successful = sum(1 for r in self.results if r["success"])
        failed = total - successful

        print("\n" + "=" * 60)
        print("📊 Batch Processing Summary")
        print("=" * 60)
        print(f"   Total files:   {total}")
        print(f"   Successful:    {successful}")
        print(f"   Failed:        {failed}")

        if failed > 0:
            print("\n❌ Failed files:")
            for r in self.results:
                if not r["success"]:
                    print(f"   - {r['file']}")

    async def close(self):
        await self.client.aclose()


def expand_file_patterns(patterns: list[str]) -> list[Path]:
    """Expand glob patterns to file list."""
    files = set()
    for pattern in patterns:
        for path in glob.glob(pattern, recursive=True):
            p = Path(path)
            if p.is_file() and p.suffix.lower() == ".pdf":
                files.add(p.resolve())
    return sorted(files)


def collect_files_from_dir(directory: Path, recursive: bool = True) -> list[Path]:
    """Collect all PDF files from directory."""
    pattern = "**/*.pdf" if recursive else "*.pdf"
    return sorted(directory.glob(pattern))


async def main():
    parser = argparse.ArgumentParser(description="Batch ingest PDF documents into knowledge base")
    parser.add_argument("--dataset", "-d", required=True, help="Dataset ID (e.g., 'agent')")
    parser.add_argument(
        "--files", "-f", nargs="+", help="File patterns to upload (e.g., '*.pdf' or specific paths)"
    )
    parser.add_argument("--dir", "-D", type=Path, help="Directory to scan for PDF files")
    parser.add_argument(
        "--recursive",
        "-r",
        action="store_true",
        default=True,
        help="Recursively scan subdirectories (default: True)",
    )
    parser.add_argument(
        "--concurrency", "-c", type=int, default=2, help="Number of concurrent uploads (default: 2)"
    )
    parser.add_argument(
        "--no-wait", action="store_true", help="Don't wait for processing to complete"
    )
    parser.add_argument(
        "--base-url",
        "-u",
        default=DEFAULT_BASE_URL,
        help=f"API base URL (default: {DEFAULT_BASE_URL})",
    )
    parser.add_argument(
        "--api-key",
        "-k",
        default=DEFAULT_API_KEY,
        help="API Key for authentication (or set GATEWAY_API_KEY env var)",
    )

    args = parser.parse_args()

    # Validate API key is provided
    if not args.api_key:
        print(
            "❌ API Key is required. Set GATEWAY_API_KEY environment variable or use --api-key flag."
        )
        sys.exit(1)

    # Collect files
    files_to_process: list[Path] = []

    if args.files:
        for pattern in args.files:
            matched = expand_file_patterns([pattern])
            files_to_process.extend(matched)

    if args.dir:
        if args.dir.exists():
            files_to_process.extend(collect_files_from_dir(args.dir, args.recursive))
        else:
            print(f"❌ Directory not found: {args.dir}")
            sys.exit(1)

    # Remove duplicates and validate
    files_to_process = sorted(set(files_to_process))
    files_to_process = [f for f in files_to_process if f.exists()]

    if not files_to_process:
        print("❌ No PDF files found to process")
        sys.exit(1)

    print("=" * 60)
    print(f"📚 Batch Ingest to Dataset: {args.dataset}")
    print("=" * 60)
    print(f"   API URL: {args.base_url}")
    print(f"   Files to process: {len(files_to_process)}")
    print(f"   Concurrency: {args.concurrency}")
    print(f"   Wait for completion: {not args.no_wait}")
    print()

    # Process files
    ingestor = BatchIngestor(args.base_url, args.api_key, args.dataset)

    try:
        # Check dataset
        if not await ingestor.check_dataset():
            sys.exit(1)

        print("\n🚀 Starting batch upload...\n")

        await ingestor.process_batch(
            files_to_process,
            concurrency=args.concurrency,
            wait_for_completion=not args.no_wait,
        )

        ingestor.print_summary()

    finally:
        await ingestor.close()


if __name__ == "__main__":
    asyncio.run(main())
