#!/usr/bin/env python3
"""Check pinned upstream source and retained licenses, without downloading code."""
import argparse
import hashlib
import json
from pathlib import Path
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
BASE = ROOT / "vendor/storytold"


def check():
    manifest = json.loads((BASE / "manifest.json").read_text())
    errors, summary = [], {}
    for name, repo in manifest["repositories"].items():
        directory = BASE / name
        hashes = repo["source_files"] | repo["provenance_files"]
        patches=repo.get("local_patches",[])
        for patch in patches:
            if hashes.get(patch["path"]) != patch["upstream_sha256"]:
                errors.append(f"{name}/{patch['path']}: patch does not identify pinned original")
            hashes[patch["path"]]=patch["local_sha256"]
        for rel, expected in hashes.items():
            path = directory / rel
            if not path.resolve().is_relative_to(directory.resolve()):
                errors.append(f"{name}/{rel}: escaped vendor directory")
            elif not path.is_file():
                errors.append(f"{name}/{rel}: missing")
            elif hashlib.sha256(path.read_bytes()).hexdigest() != expected:
                errors.append(f"{name}/{rel}: differs from pinned source")
        for required in ["LICENSE-MIT", "LICENSE-APACHE", "NOTICE", "ATTRIBUTION.md", "Cargo.upstream.toml"]:
            if required not in repo["provenance_files"]:
                errors.append(f"{name}: missing recorded provenance {required}")
        packages = []
        for cargo in sorted((directory / "crates").glob("*/Cargo.toml")):
            packages.append(tomllib.loads(cargo.read_text())["package"]["name"])
        if sorted(packages) != sorted(repo["packages"]):
            errors.append(f"{name}: package inventory differs")
        upstream = tomllib.loads((directory / "Cargo.upstream.toml").read_text())
        local = tomllib.loads((directory / "Cargo.toml").read_text())
        for key in ["members", "default-members"]:
            upstream["workspace"][key] = ["crates/*"]
        if local != upstream:
            errors.append(f"{name}/Cargo.toml: undocumented workspace changes")
        if (directory / "crates/ui-egui").exists() or (directory / "assets").exists():
            errors.append(f"{name}: desktop UI or repository assets unexpectedly included")
        summary[name] = {"revision": repo["revision"], "core_packages": len(packages),
                         "unchanged_source_files": len(repo["source_files"])-len(patches), "documented_patches":len(patches), "provenance_files": len(repo["provenance_files"])}
    print(json.dumps({"ok": not errors, "repositories": summary, "errors": errors}, indent=2))
    return 1 if errors else 0


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["check"], default="check", nargs="?")
    parser.parse_args()
    sys.exit(check())
