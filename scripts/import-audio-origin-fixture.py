#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Import public synthetic inputs from the completed, pinned audio-origin experiment.

This copy-only adapter invokes no codec or subprocess. It refuses
incomplete/changed evidence and existing output folders.
Import is provenance conversion, never a mechanism or repair acceptance verdict.
"""

import argparse
import hashlib
import json
from pathlib import Path


RECIPE_PINS = {
    "experiment.py": "0a8e68482d1f6316cba085a4a6bbc9cbb21cfc0b6c77e1a447380594bdb3a188",
    "probe.c": "bce7d756e29a7ac1622e27d7129b7b3bab2352913cca021da4fd55eb11dd580a",
    "tool-pins.json": "a5f9d27aac1d976192a281c8cb08514ba0302fc480090195cb7501067fe284b7",
}
SOURCES = {
    "wav": "synthetic.wav",
    "flac": "synthetic.flac",
    "tagged": "tagged.mp3",
    "untagged": "untagged.mp3",
}
REFERENCES = [(name, 48000) for name in SOURCES] + [("tagged", 44100)]
CASES = (
    [f"{name}-48000-zero" for name in SOURCES]
    + ["wav-48000-trim", "tagged-48000-trim", "tagged-44100-zero"]
)


def require(ok, message):
    if not ok:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--recipe-dir", type=Path, required=True)
    parser.add_argument("--experiment-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    recipe = args.recipe_dir.resolve()
    run = args.experiment_dir.resolve()
    for name, expected in RECIPE_PINS.items():
        require(digest((recipe / name).read_bytes()) == expected, f"changed recipe: {name}")

    manifest_bytes = (run / "manifest.json").read_bytes()
    entries = json.loads(manifest_bytes)
    indexed = {entry["path"]: entry for entry in entries}
    require(len(indexed) == len(entries), "duplicate experiment manifest paths")

    def read(name):
        # Callers supply only the explicit synthetic/evidence names below.
        path = (run / name).resolve()
        require(path.is_relative_to(run), f"outside experiment: {name}")
        data = path.read_bytes()
        pin = indexed[name]
        require(len(data) == pin["bytes"] and digest(data) == pin["sha256"],
                f"changed experiment file: {name}")
        return data

    observations_bytes = read("observations.json")
    observations = json.loads(observations_bytes)
    require(observations["status"] == "measurements_only", "unexpected experiment status")
    require(set(observations["cases"]) == set(CASES), "incomplete experiment cases")
    commands_bytes = read("commands.json")
    commands = json.loads(commands_bytes)
    labels = {
        "ffmpeg-version", "lame-version", "engine-version", "compile-probe",
        "probe-linkage", "make-flac", "make-tagged", "make-untagged",
    }
    labels.update(f"{name}-metadata" for name in SOURCES)
    labels.update(f"{name}-{mode}" for name in SOURCES for mode in ("unset", "stream"))
    labels.update(f"{name}-reference-{rate}" for name, rate in REFERENCES)
    labels.update(f"{case}-{step}" for case in CASES for step in ("render", "pcm"))
    require(len(commands) == len(labels) and {c["label"] for c in commands} == labels,
            "incomplete command sequence")
    require(all(c.get("returncode") == 0 and "outcome" not in c for c in commands),
            "experiment command did not finish successfully")

    probes = {}
    for name in SOURCES:
        modes = {}
        packets = {}
        for mode in ("unset", "stream"):
            label = f"{name}-{mode}"
            events = [json.loads(line) for line in read(f"logs/{label}.stdout").splitlines()]
            stream = next(e for e in events if e["kind"] == "stream")
            frames = [e for e in events if e["kind"] == "frame"]
            summary = events[-1]
            packets[mode] = [e for e in events if e["kind"] == "packet"]
            require(summary["kind"] == "summary" and frames, f"{label}: no complete decode")
            require(summary["frames"] == len(frames)
                    and summary["samples"] == sum(f["nb_samples"] for f in frames),
                    f"{label}: sample/frame counts disagree")
            require(stream["sample_rate"] == 44100
                    and all(f["sample_rate"] == 44100 and f["channels"] == 2
                            and f["nb_samples"] > 0 for f in frames),
                    f"{label}: unexpected source format")
            require(stream["mode"] == mode, f"{label}: wrong probe arm")
            require(stream["pkt_timebase"] == stream["time_base"] if mode == "stream"
                    else stream["pkt_timebase"][0] == 0, f"{label}: wrong treatment")
            raw_hash = digest(read(f"{label}.frame-by-frame.bin"))
            observed = observations["probe"][name][mode]
            for field, value in (
                ("stream", stream), ("first_frame", frames[0]),
                ("last_frame", frames[-1]), ("summary", summary), ("raw_sha256", raw_hash),
            ):
                require(observed[field] == value, f"{label}: observations disagree on {field}")
            # Copy only defined numeric/timing/codec fields, never command paths.
            modes[mode] = {
                "stream": {key: stream[key] for key in (
                    "decoder", "time_base", "pkt_timebase", "start_time",
                    "sample_rate", "initial_padding", "trailing_padding",
                )},
                "first_frame": {key: frames[0][key] for key in (
                    "pts", "nb_samples", "sample_rate", "channels", "time_base",
                )},
                "last_frame": {key: frames[-1][key] for key in (
                    "pts", "nb_samples", "sample_rate", "channels", "time_base",
                )},
                "summary": {"frames": summary["frames"], "samples": summary["samples"]},
                "raw_sha256": raw_hash,
            }
        equal = (modes["unset"]["raw_sha256"] == modes["stream"]["raw_sha256"]
                 and modes["unset"]["summary"] == modes["stream"]["summary"])
        require(equal and observations["probe"][name]["decoded_bytes_equal"],
                f"{name}: probe arms changed decoded bytes/counts; retain as inconclusive")
        require(packets["unset"] == packets["stream"], f"{name}: packet sequences differ")
        skips = [p.get("skip_samples", 0) for p in packets["unset"]]
        require(any(s > 0 for s in skips) == (name == "tagged"),
                f"{name}: expected tagged/untagged control not observed")
        probes[name] = {
            **modes, "decoded_bytes_equal": equal, "skip_samples": skips,
            "discard_padding": [p.get("discard_padding", 0) for p in packets["unset"]],
        }

    # No glob/copytree: only four original synthetic sources and their own
    # same-library reference PCM. No private custody, film inputs or command logs.
    names = list(SOURCES.values()) + [
        f"{name}-reference-{rate}.f32le" for name, rate in REFERENCES
    ]
    files = {name: read(name) for name in names}
    for name, rate in REFERENCES:
        data = files[f"{name}-reference-{rate}.f32le"]
        require(len(data) > 0 and len(data) % 8 == 0, f"{name}: incomplete stereo PCM")
        if name != "untagged":
            require(len(data) == 2 * rate * 8, f"{name}: expected two-second reference")

    fixture = {
        "schema": "ferrocut.audio-origin-fixture/1",
        "status": "measurements_only",
        "provenance": {
            "recipe_pins": RECIPE_PINS,
            "experiment_manifest_sha256": digest(manifest_bytes),
            "observations_sha256": digest(observations_bytes),
            "commands_sha256": digest(commands_bytes),
            "baseline_engine_sha256": observations["engine_sha256"],
            "source": "Original seeded integer noise bursts; no third-party/client media.",
            "source_sample_rate": 44100, "source_channels": 2, "source_samples": 88200,
            "source_license": "Apache-2.0",
            "encoding": "Existing standalone LGPL LAME only for MP3; LGPL FFmpeg for FLAC.",
            "decoding": "Pinned LGPL FFmpeg only, including each MP3's own reference.",
        },
        "probes": probes,
        "files": {name: {"bytes": len(data), "sha256": digest(data)}
                  for name, data in files.items()},
    }
    # Publish the manifest last. A partial copy has no usable fixture.json.
    args.output.mkdir(parents=True, exist_ok=False)
    for name, data in files.items():
        (args.output / name).write_bytes(data)
    (args.output / "fixture.json").write_text(json.dumps(fixture, indent=2) + "\n")
    print("Synthetic fixture imported; mechanism and regression verdicts remain separate.")


if __name__ == "__main__":
    main()
