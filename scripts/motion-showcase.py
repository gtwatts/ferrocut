#!/usr/bin/env python3
"""Reproduce the native mask/repeater/tracking/revision/undo milestone.

Build first: cargo build --locked --offline -p ferrocut-engine --bin ferrocut
Run: python3 scripts/motion-showcase.py --output-dir /tmp/ferrocut-motion-demo
The directory must be empty. All pictures are rendered by Ferrocut itself.
"""

import argparse
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import shutil
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--binary", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    binary = (args.binary or root / "target/debug/ferrocut").resolve()
    if not binary.is_file():
        parser.error("build the ferrocut binary first")
    output = args.output_dir.resolve()
    if output.exists() and any(output.iterdir()):
        parser.error("output directory must be empty; existing artifacts are preserved")
    output.mkdir(parents=True, exist_ok=True)

    def write(name, value):
        path = output / name
        path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")
        return path

    def read(name):
        return json.loads((output / name).read_text())

    def cli(name, *argv):
        log = output / (name + ".log")
        with log.open("w") as stream:
            result = subprocess.run([str(binary), *map(str, argv)], cwd=root,
                                    stdout=stream, stderr=subprocess.STDOUT, check=False)
        if result.returncode:
            raise SystemExit(f"{name} failed ({result.returncode}); inspect {log}")
        return log

    def render(name, timeline):
        cli(name, "render", timeline, "--output", output / (name + ".mkv"),
            "--report", output / (name + ".report.json"), "--cache-dir", output / "cache",
            "--cpu", "--jobs", "1")
        return read(name + ".report.json")

    examples = root / "examples"
    render("motion-source", examples / "motion-source.json")
    document = json.loads((examples / "motion-showcase.json").read_text())
    for track in document["tracks"]:
        for clip in track["clips"]:
            if clip.get("source"):
                clip["source"] = str(output / "motion-source.mkv")
            generator = clip.get("generator", {})
            if generator.get("type") == "text":
                generator["text"]["font"] = str((examples / generator["text"]["font"]).resolve())
    timeline = write("timeline.json", document)
    cli("tracking-analyze", "tracking", "analyze", output / "motion-source.mkv",
        "--settings", examples / "motion-tracking.settings",
        "--output", output / "tracking-analysis.json", "--timeout", "60")
    analysis = read("tracking-analysis.json")
    assert analysis["completion"] == "completed", analysis["completion"]
    track = analysis["tracks"][0]
    assert track["outcome"] == "tracked", track["outcome"]
    errors = []
    for sample in track["samples"]:
        expected = [Fraction(197, 2) + sample["frame"] * 2,
                    Fraction(173, 2) + sample["frame"]]
        errors.append(max(abs(Fraction(actual) - wanted)
                          for actual, wanted in zip(sample["position"], expected)))
    assert max(errors) <= Fraction(1, 100), "tracking drift exceeds synthetic fixture tolerance"
    cli("tracking-keyframes", "tracking", "keyframes", output / "tracking-analysis.json",
        "--timeline", timeline, "--options", examples / "motion-tracking.options")
    attach = json.loads((output / "tracking-keyframes.log").read_text())
    attach_ops = write("tracking-attach.ops", attach["ops"])
    cli("tracking-edit", "edit", timeline, attach_ops, "--json", "--plan")
    shutil.copy2(timeline, output / "attached.timeline.json")
    render("motion-showcase", timeline)

    # Stabilization is a separate native revision with its own edit journal.
    stabilized = output / "stabilized.timeline.json"
    shutil.copy2(timeline, stabilized)
    stabilization_options = write("stabilization.options.json", {
        "point": "target", "source_clip": "source", "target_clip": "source",
        "mode": "stabilize", "stabilization": {"mode": "lock", "smoothness": 50},
    })
    cli("stabilization-keyframes", "tracking", "keyframes", output / "tracking-analysis.json",
        "--timeline", stabilized, "--options", stabilization_options)
    operations = json.loads((output / "stabilization-keyframes.log").read_text())["ops"]
    operations += [
        {"op": "set_param", "clip": "reticle", "param": "opacity", "value": 0},
        {"op": "set_param", "clip": "title", "param": "generator.text.content",
         "value": "TRANSLATION, STABILIZED."},
    ]
    cli("stabilization-edit", "edit", stabilized, write("stabilization.ops", operations), "--json")
    render("motion-stabilized", stabilized)

    cli("revision-edit", "edit", timeline, examples / "motion-revise.ops", "--json", "--plan")
    shutil.copy2(timeline, output / "revised.timeline.json")
    render("motion-revised", timeline)
    render("motion-repeat", timeline)
    cli("undo", "undo", timeline, "--json")
    render("motion-undo", timeline)

    def digest(name):
        return hashlib.sha256((output / name).read_bytes()).hexdigest()

    assert digest("motion-revised.mkv") == digest("motion-repeat.mkv")
    assert digest("motion-showcase.mkv") == digest("motion-undo.mkv")
    assert digest("motion-revised.mkv") != digest("motion-showcase.mkv")
    for name in ["motion-repeat", "motion-undo"]:
        report = read(name + ".report.json")
        assert report["rendered_frames"] == 0 and report["reused_frames"] == 32, report
        assert report["gpu_submissions"] == 0, report
    summary = {
        "schema": "ferrocut.native-motion-showcase/1",
        "source": "original synthetic native vector animation; not real-footage tracking validation",
        "frames": 32, "minimum_measured_confidence": min(
            float(Fraction(sample["confidence"])) for sample in track["samples"] if "confidence" in sample),
        "maximum_axis_error_pixels": float(max(errors)),
        "repeat_exact": True, "undo_exact": True,
        "files": {name: digest(name) for name in [
            "motion-source.mkv", "motion-showcase.mkv", "motion-revised.mkv",
            "motion-repeat.mkv", "motion-undo.mkv", "motion-stabilized.mkv",
        ]},
    }
    write("workflow-summary.json", summary)
    print(json.dumps({"output": str(output), **summary}, indent=2))


if __name__ == "__main__":
    main()
