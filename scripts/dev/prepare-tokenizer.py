"""Prepare the exact offline tokenizer input declared by the locked tiktoken wheel."""
from __future__ import annotations

import hashlib
import importlib.metadata
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

from scripts.harness.knowledge_proxy_contract import (  # noqa: E402
    tokenizer_declaration,
    tokenizer_input,
)

distribution = importlib.metadata.distribution("tiktoken")
declaration = Path(distribution.locate_file("tiktoken_ext/openai_public.py"))
url, expected = tokenizer_declaration(declaration.read_text())
cache = Path(os.environ["TIKTOKEN_CACHE_DIR"])
cache.mkdir(parents=True, exist_ok=True)
asset = cache / hashlib.sha1(url.encode()).hexdigest()
if not asset.is_file() or hashlib.sha256(asset.read_bytes()).hexdigest() != expected:
    partial = asset.with_suffix(".part")
    subprocess.run([
        "curl", "--fail", "--silent", "--show-error", "--location",
        "--retry", "4", "--retry-all-errors", "--continue-at", "-",
        "--connect-timeout", "15", "--max-time", "120", url, "-o", str(partial),
    ], check=True)
    if hashlib.sha256(partial.read_bytes()).hexdigest() != expected:
        raise SystemExit("Tokenizer input SHA-256 mismatch")
    partial.replace(asset)
tokenizer_input(cache)
print("Verified offline cl100k_base tokenizer cache")
