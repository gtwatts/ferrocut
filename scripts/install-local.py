#!/usr/bin/env python3
"""Install an already-built Ferrocut and register its local Codex MCP server."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tomllib

# whisper.cpp files the engine's discovery looks for under an ancestor of the
# running executable (crates/ferrocut-engine/src/index/whisper.rs), in its
# model order. The installed runtime gets links at these same paths so an
# installed ferrocut finds the checkout's whisper from any directory.
WHISPER_CLI = Path("third_party/whisper.cpp/build/bin/whisper-cli")
WHISPER_MODELS = [Path("third_party/whisper-models") / name for name in
                  ("ggml-medium.en.bin", "ggml-medium.bin", "ggml-small.en.bin", "ggml-small.bin")]


def whisper_links(root):
    """(path in the runtime, checkout file) for each whisper file that exists.

    Optional: none at all is fine. Only an executable CLI and the discovery
    model names are linked, never other third_party data; nothing is copied
    or downloaded.
    """
    links = []
    cli = root / WHISPER_CLI
    if cli.is_file() and os.access(cli, os.X_OK):
        links.append((WHISPER_CLI, cli.resolve()))
    links += [(model, (root / model).resolve()) for model in WHISPER_MODELS if (root / model).is_file()]
    return links


def link(path, target):
    """Point `path` at `target`, replacing an earlier link atomically."""
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".installing")
    if temporary.is_symlink() or temporary.exists():
        temporary.unlink()
    temporary.symlink_to(target)
    os.replace(temporary, path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--check", action="store_true", help="Review changes without writing")
    args = parser.parse_args()
    root = args.root.resolve()
    home = Path.home()
    bindir = home / ".local/bin"
    config = home / ".codex/config.toml"
    text = config.read_text() if config.exists() else ""
    parsed = tomllib.loads(text)
    names = ["ferrocut", "ferrocut-mcp", "ferrocut-perceive", "ferrocut-deliver"]
    for name in names:
        if not (root / "target/release" / name).is_file():
            raise SystemExit(f"Missing release binary: {name}")
    entry = {
        "command": str(bindir / "ferrocut-mcp"),
        "args": ["--root", str(root)],
        "cwd": str(root),
        "env": {"FERROCUT_PERCEIVE": str(bindir / "ferrocut-perceive")},
        "startup_timeout_sec": 30,
        "tool_timeout_sec": 1800,
    }
    existing = parsed.get("mcp_servers", {}).get("ferrocut")
    if existing is not None and existing != entry:
        raise SystemExit("An existing different Ferrocut MCP entry needs review; no files changed.")
    table = "\n\n# Ferrocut local agent tools (scripts/install-local.py).\n[mcp_servers.ferrocut]\n"
    for key, value in entry.items():
        if key == "env":
            table += "env = { FERROCUT_PERCEIVE = " + json.dumps(value["FERROCUT_PERCEIVE"]) + " }\n"
        else:
            table += f"{key} = {json.dumps(value)}\n"
    updated = text if existing is not None else text.rstrip() + table
    tomllib.loads(updated)
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    binaries = {name: hashlib.sha256((root / "target/release" / name).read_bytes()).hexdigest() for name in names}
    libraries = root / "third_party/ffmpeg-lgpl/lib"
    if not (libraries / "libavutil.so").is_file():
        raise SystemExit("Build the project's shared LGPL FFmpeg before installing.")
    runtime = home / ".local/share/ferrocut/versions" / (revision[:12] + "-" + binaries["ferrocut"][:12])
    links = whisper_links(root)
    whisper = {"cli": next((str(t) for p, t in links if p == WHISPER_CLI), None),
               "models": [str(t) for p, t in links if p != WHISPER_CLI]}
    print(json.dumps({"root": str(root), "revision": revision, "binary_destination": str(bindir),
                      "runtime": str(runtime), "runtime_libraries": str(libraries),
                      "codex_config": str(config), "mcp": entry, "sha256": binaries, "whisper": whisper,
                      "write": not args.check}, indent=2), flush=True)
    if args.check:
        return
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    backup = home / ".local/share/ferrocut/backups" / stamp
    backup.mkdir(parents=True, exist_ok=False)
    bindir.mkdir(parents=True, exist_ok=True)
    # Preserve the layout expected by the binaries' $ORIGIN-relative RUNPATH.
    runtime_bin = runtime / "target/release"
    runtime_bin.mkdir(parents=True, exist_ok=True)
    runtime_lib = runtime / "third_party/ffmpeg-lgpl/lib"
    shutil.copytree(libraries, runtime_lib, symlinks=True, dirs_exist_ok=True,
                    ignore=shutil.ignore_patterns("pkgconfig"))
    # Links into the checkout (absolute, so not portable): the installed
    # route transcribes with the checkout's whisper, as the MCP entry runs
    # in the checkout. Explicit options and FERROCUT_WHISPER_* still win.
    for path, target in links:
        link(runtime / path, target)
    for path in {WHISPER_CLI, *WHISPER_MODELS} - {path for path, _ in links}:
        if (runtime / path).is_symlink():  # gone from the checkout since an earlier install
            (runtime / path).unlink()
    shutil.copy2(root / "LICENSE", runtime / "LICENSE")
    (runtime / "SOURCE.txt").write_text(
        f"Ferrocut source: https://github.com/gtwatts/ferrocut/tree/{revision}\n"
        "Shared FFmpeg built with scripts/build-ffmpeg-lgpl.sh in the source checkout.\n"
        f"Local matching FFmpeg sources/build: {root / 'third_party'}\n")
    for name in names:
        destination = bindir / name
        if destination.exists() or destination.is_symlink():
            shutil.copy2(destination, backup / name)
        installed_binary = runtime_bin / name
        shutil.copy2(root / "target/release" / name, installed_binary)
        installed_binary.chmod(0o755)
        temporary = bindir / (name + ".installing")
        temporary.symlink_to(installed_binary)
        os.replace(temporary, destination)
    # Fail before editing Codex settings if the packaged loader cannot start.
    for name in names:
        subprocess.run([str(bindir / name), "--help"], check=True, capture_output=True)
    if updated != text:
        config.parent.mkdir(parents=True, exist_ok=True)
        if config.exists():
            shutil.copy2(config, backup / "config.toml")
        temporary = config.with_suffix(".ferrocut-installing.toml")
        temporary.write_text(updated)
        temporary.chmod(config.stat().st_mode & 0o777 if config.exists() else 0o600)
        os.replace(temporary, config)
    record = home / ".local/share/ferrocut/install.json"
    record.write_text(json.dumps({"installed_at": stamp, "revision": revision, "root": str(root),
                                  "runtime": str(runtime), "sha256": binaries, "whisper": whisper,
                                  "backup": str(backup)}, indent=2) + "\n")
    print(f"Installed; recoverable backup: {backup}")


if __name__ == "__main__":
    main()
