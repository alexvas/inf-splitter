#!/usr/bin/env python3
"""Regenerate THIRD_PARTY_NOTICES from Cargo.lock (runtime deps only)."""

from __future__ import annotations

import json
import re
import sys
import urllib.request
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LOCK = ROOT / "Cargo.lock"
OUT = ROOT / "THIRD_PARTY_NOTICES"
REGISTRY = Path.home() / ".cargo/registry/src"


def parse_lock() -> list[tuple[str, str]]:
    text = LOCK.read_text(encoding="utf-8")
    packages: list[tuple[str, str]] = []
    for block in re.split(r"\n\[\[package\]\]\n", text):
        m_name = re.search(r'^name = "([^"]+)"', block, re.M)
        m_ver = re.search(r'^version = "([^"]+)"', block, re.M)
        if m_name and m_ver and m_name.group(1) != "inf-splitter":
            packages.append((m_name.group(1), m_ver.group(1)))
    return packages


def license_from_registry(name: str, version: str) -> str | None:
    for index_dir in REGISTRY.glob("index.*"):
        manifest = index_dir / f"{name}-{version}" / "Cargo.toml"
        if not manifest.exists():
            continue
        in_package = False
        for line in manifest.read_text(encoding="utf-8", errors="replace").splitlines():
            if line.strip() == "[package]":
                in_package = True
                continue
            if in_package and line.startswith("[") and line.strip() != "[package]":
                break
            if in_package and line.startswith("license"):
                return line.split("=", 1)[1].strip().strip('"')
    return None


def license_from_crates_io(name: str, version: str, cache: dict[tuple[str, str], str | None]) -> str | None:
    key = (name, version)
    if key not in cache:
        url = f"https://crates.io/api/v1/crates/{name}/{version}"
        with urllib.request.urlopen(url, timeout=30) as response:
            data = json.load(response)
        cache[key] = data.get("version", {}).get("license")
    return cache[key]


def main() -> int:
    if not LOCK.is_file():
        print(f"error: {LOCK} not found", file=sys.stderr)
        return 1

    api_cache: dict[tuple[str, str], str | None] = {}
    by_license: dict[str, list[str]] = defaultdict(list)

    for name, version in sorted(set(parse_lock()), key=lambda p: (p[0].lower(), p[1])):
        license_id = license_from_registry(name, version) or license_from_crates_io(
            name, version, api_cache
        )
        if not license_id:
            print(f"warning: no license for {name} {version}", file=sys.stderr)
            license_id = "UNKNOWN"
        by_license[license_id].append(f"{name} {version}")

    lines = [
        "Third-Party Notices for inf-splitter",
        "=====================================",
        "",
        "This file lists Rust crates used to build inf-splitter (dependency tree from",
        "Cargo.lock, including transitive dependencies). Each crate is used under the",
        "license shown in its section.",
        "",
        "Regenerate:",
        "",
        "    python3 scripts/generate-third-party-notices.py",
        "",
    ]

    for license_id in sorted(by_license.keys(), key=str.lower):
        crates = by_license[license_id]
        lines.extend(
            [
                "---",
                f"SPDX-License-Identifier: {license_id}",
                "",
                f"({len(crates)} crates)",
                "",
            ]
        )
        lines.extend(f"  - {crate}" for crate in crates)
        lines.append("")

    lines.extend(
        [
            "---",
            "Standard license texts",
            "======================",
            "",
            "The sections above use SPDX license expressions. Where a crate is",
            "dual-licensed (e.g. MIT OR Apache-2.0), you may use either license.",
            "",
            "Full texts for common identifiers are in the licenses/ directory:",
            "",
            "  licenses/MIT.txt",
            "  licenses/Apache-2.0.txt",
            "  licenses/ISC.txt",
            "  licenses/BSD-3-Clause.txt",
            "  licenses/Zlib.txt",
            "  licenses/Unlicense.txt",
            "",
            "See licenses/README.md for other SPDX IDs (Unicode-3.0, BSL-1.0, etc.).",
            "",
        ]
    )

    OUT.write_text("\n".join(lines), encoding="utf-8")
    print(f"wrote {OUT} ({len(by_license)} license groups, {sum(len(v) for v in by_license.values())} crates)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
