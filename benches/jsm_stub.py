#!/usr/bin/env python3
"""Phase 0-only benchmark stub; this is not the JSM package manager."""

from __future__ import annotations

import json
import sys
from pathlib import Path


def main() -> int:
    if len(sys.argv) > 1 and sys.argv[1] in {"--version", "-V"}:
        print("jsm-stub 0.1.0 (Phase 0; not an installer)")
        return 0
    manifest = Path("package.json")
    if not manifest.is_file():
        print("jsm-stub: missing package.json", file=sys.stderr)
        return 2
    try:
        data = json.loads(manifest.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        print(f"jsm-stub: invalid fixture manifest: {error}", file=sys.stderr)
        return 2
    dependencies = data.get("dependencies", {})
    Path(".jsm-stub-state").write_text(
        json.dumps({"dependency_count": len(dependencies), "phase": 0}, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(f"jsm-stub: benchmark smoke only; observed {len(dependencies)} declared dependencies")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
