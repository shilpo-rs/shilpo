#!/usr/bin/env python3
"""Discover and verify exact cross-repository Git revision pins without mutating inputs."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Iterator
from urllib.parse import urlparse


@dataclass(frozen=True)
class Repository:
    name: str
    root: Path


@dataclass(frozen=True)
class Pin:
    consumer: str
    source: str
    url: str
    revision: str
    fetch_revision: str
    location: str


def parse_repository(value: str) -> Repository:
    try:
        name, raw_root = value.split("=", 1)
    except ValueError as error:
        raise argparse.ArgumentTypeError("expected OWNER/REPO=PATH") from error
    root = Path(raw_root).resolve()
    if not name or not root.is_dir():
        raise argparse.ArgumentTypeError(f"repository root does not exist: {raw_root}")
    return Repository(name=name, root=root)


def owning_repository(url: str) -> str:
    github = re.search(r"github\.com[/:]([^/]+)/([^/?#]+)", url)
    if github:
        return f"{github.group(1)}/{github.group(2).removesuffix('.git')}"

    parsed = urlparse(url)
    path = Path(parsed.path if parsed.scheme else url)
    parts = path.parts
    if len(parts) >= 2:
        return f"{parts[-2]}/{parts[-1].removesuffix('.git')}"
    return path.name.removesuffix(".git") or url


def canonical_url(url: str) -> str:
    return url.rstrip("/").removesuffix(".git")


def walk_tables(value: object, path: tuple[str, ...] = ()) -> Iterator[tuple[tuple[str, ...], dict]]:
    if isinstance(value, dict):
        yield path, value
        for key, child in value.items():
            yield from walk_tables(child, (*path, str(key)))
    elif isinstance(value, list):
        for index, child in enumerate(value):
            yield from walk_tables(child, (*path, str(index)))


def cargo_lock_resolutions(repository: Repository) -> dict[tuple[str, str], str]:
    resolutions: dict[tuple[str, str], str] = {}
    source_pattern = re.compile(r"^git\+([^?]+)\?[^#]*\brev=([^&#]+)[^#]*#([0-9a-f]{40})$")
    for lockfile in sorted(repository.root.rglob("Cargo.lock")):
        relative = lockfile.relative_to(repository.root)
        if any(part in {".git", "target"} for part in relative.parts):
            continue
        with lockfile.open("rb") as handle:
            document = tomllib.load(handle)
        for package in document.get("package", []):
            source = package.get("source")
            match = source_pattern.match(source) if isinstance(source, str) else None
            if match:
                url, revision, resolved = match.groups()
                resolutions[(canonical_url(url), revision)] = resolved
    return resolutions


def discover_cargo_pins(repository: Repository) -> list[Pin]:
    pins: list[Pin] = []
    resolutions = cargo_lock_resolutions(repository)
    for manifest in sorted(repository.root.rglob("Cargo.toml")):
        relative = manifest.relative_to(repository.root)
        if any(part in {".git", "target"} for part in relative.parts):
            continue
        with manifest.open("rb") as handle:
            document = tomllib.load(handle)
        for table_path, table in walk_tables(document):
            url = table.get("git")
            revision = table.get("rev")
            if isinstance(url, str) and isinstance(revision, str):
                pins.append(
                    Pin(
                        consumer=repository.name,
                        source=owning_repository(url),
                        url=url,
                        revision=revision,
                        fetch_revision=resolutions.get(
                            (canonical_url(url), revision), revision
                        ),
                        location=f"{relative}:{'.'.join(table_path)}",
                    )
                )
    return pins


def discover_wit_pin(repository: Repository, source_url: str) -> list[Pin]:
    revision_file = repository.root / "WIT_REV"
    if not revision_file.is_file():
        return []
    revision = revision_file.read_text(encoding="utf-8").strip()
    if not revision:
        return []
    return [
        Pin(
            consumer=repository.name,
            source=owning_repository(source_url),
            url=source_url,
            revision=revision,
            fetch_revision=revision,
            location="WIT_REV",
        )
    ]


def verify_revision(pin: Pin) -> tuple[bool, str]:
    with tempfile.TemporaryDirectory(prefix="shilpo-pin-") as tmp:
        git_dir = Path(tmp) / "objects.git"
        init = subprocess.run(
            ["git", "init", "--bare", "--quiet", str(git_dir)],
            text=True,
            capture_output=True,
        )
        if init.returncode != 0:
            return False, init.stderr.strip()
        fetch = subprocess.run(
            [
                "git",
                f"--git-dir={git_dir}",
                "fetch",
                "--quiet",
                "--depth=1",
                "--no-tags",
                pin.url,
                pin.fetch_revision,
            ],
            text=True,
            capture_output=True,
        )
        if fetch.returncode != 0:
            output = fetch.stderr.strip() or fetch.stdout.strip() or "git fetch failed without output"
            return False, output.splitlines()[-1]
        resolved = subprocess.run(
            ["git", f"--git-dir={git_dir}", "rev-parse", "FETCH_HEAD^{commit}"],
            check=True,
            text=True,
            capture_output=True,
        ).stdout.strip()
        if re.fullmatch(r"[0-9a-fA-F]{7,40}", pin.revision) and not resolved.startswith(
            pin.revision.lower()
        ):
            return False, f"resolved to unexpected commit {resolved}"
        return True, resolved


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo",
        action="append",
        required=True,
        type=parse_repository,
        metavar="OWNER/REPO=PATH",
        help="repository checkout to scan; repeat for each ecosystem repository",
    )
    parser.add_argument(
        "--wit-source",
        default="https://github.com/shilpo-rs/shilpo",
        metavar="URL",
        help="Git repository referenced by SDK WIT_REV files",
    )
    args = parser.parse_args(argv)

    pins = [
        pin
        for repository in args.repo
        for pin in [
            *discover_cargo_pins(repository),
            *discover_wit_pin(repository, args.wit_source),
        ]
    ]
    failures = 0
    cache: dict[tuple[str, str], tuple[bool, str]] = {}
    for pin in pins:
        key = (pin.url, pin.fetch_revision)
        if key not in cache:
            cache[key] = verify_revision(pin)
        verdict = cache[key]
        ok, detail = verdict
        status = "OK" if ok else "MISSING"
        resolved_suffix = f" resolved={detail}" if ok else ""
        print(
            f"{status} consumer={pin.consumer} source={pin.source} "
            f"revision={pin.revision}{resolved_suffix} location={pin.location}"
        )
        if not ok:
            failures += 1
            print(f"  {detail}", file=sys.stderr)

    if not pins:
        print("No configured exact Git revisions found.", file=sys.stderr)
        return 2
    if failures:
        print(f"Failed to resolve {failures} of {len(pins)} configured revision(s).", file=sys.stderr)
        return 1
    print(f"Verified {len(pins)} configured revision(s) across {len(args.repo)} repository root(s).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
