#!/usr/bin/env python
from __future__ import annotations

import argparse
import json
import sys

from src.core.gateway.capacity import CapacityResolver


def main() -> int:
    parser = argparse.ArgumentParser(description="Print Gateway UAT capacity inventory.")
    parser.add_argument("--format", choices=["json", "text"], default="text")
    parser.add_argument("--allow-missing", action="store_true")
    args = parser.parse_args()

    resolver = CapacityResolver()
    rows = resolver.inventory_rows()
    missing = [row for row in rows if row.get("source_status") == "missing"]
    payload = {
        "mode": resolver.mode,
        "cluster_epoch": resolver.cluster_epoch,
        "services": rows,
        "missing_count": len(missing),
    }

    if args.format == "json":
        print(json.dumps(payload, indent=2, sort_keys=True))
    else:
        for row in rows:
            print(
                "{service_id} -> {budget_key} enforced={enforced} source_status={source_status}".format(
                    **row
                )
            )

    if missing and not args.allow_missing:
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
