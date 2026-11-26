#!/usr/bin/env python3
"""Check the licence: LICENSE is the MIT licence, the workspace declares MIT, and every crate
inherits the workspace's licence (or states MIT itself). Usage: check-license.py [root]"""

import sys
import tomllib
from pathlib import Path


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
    problems = []

    license_text = (root / "LICENSE").read_text(encoding="utf-8")
    if not license_text.startswith("MIT License"):
        problems.append("LICENSE does not start with `MIT License`")
    if "Permission is hereby granted, free of charge" not in license_text:
        problems.append("LICENSE is missing the MIT permission notice")

    workspace = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    if workspace.get("workspace", {}).get("package", {}).get("license") != "MIT":
        problems.append("Cargo.toml: [workspace.package] license is not \"MIT\"")

    manifests = sorted((root / "crates").glob("*/Cargo.toml"))
    if not manifests:
        problems.append("no crates found under crates/")
    for manifest in manifests:
        package = tomllib.loads(manifest.read_text(encoding="utf-8")).get("package", {})
        license_field = package.get("license")
        if license_field != {"workspace": True} and license_field != "MIT":
            problems.append(f"{manifest.relative_to(root)}: license is {license_field!r}, not MIT")

    for problem in problems:
        print(problem)
    print(f"Checked LICENSE and {len(manifests)} crate manifests: {len(problems)} problems.")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
