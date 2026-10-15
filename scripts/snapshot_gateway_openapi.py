"""Export the actual Python/hybrid Gateway without overwriting the mixed SDK contract."""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from scripts.harness.openapi_profiles import check_python_ownership, load_ownership  # noqa: E402


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", required=True, choices=["python-hybrid"])
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args(argv)
    published = ROOT / "sdk/openapi.json"
    if args.output.resolve() == published.resolve() or (args.output.exists() and args.output.samefile(published)):
        parser.error("Python export cannot overwrite sdk/openapi.json; choose a separate candidate output")
    ownership = load_ownership(json.loads(published.read_text(encoding="utf-8")))
    from src.main import create_app
    spec = create_app().openapi()
    check_python_ownership(spec, ownership)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(spec, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"Wrote Python/hybrid candidate to {args.output} ({len(spec['paths'])} paths); mixed publication unchanged")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
