#!/usr/bin/env python3
"""Rebuild the editable native graphics example (stdlib only, no rendering)."""
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXAMPLES = ROOT / "examples"
FONT = "../crates/ferrocut-engine/tests/data/text/NotoSans-Regular.ttf"
OUTPUT = {"width": 1280, "height": 720, "fps": "24", "duration": "4", "gop": 24, "gops_per_chunk": 1}


def keys(*pairs):
    return {"keyframes": [{"t": str(t), "v": str(v), **({"interp": "ease_out"} if i == 0 else {})} for i, (t, v) in enumerate(pairs)]}


def clip(ident, generator, **extra):
    return {"id": ident, "generator": generator, "start": "0", "duration": "4", **extra}


def shape(ident, geometry, fill, stroke=None, **extra):
    payload = {"geometry": geometry, "fill": fill}
    if stroke:
        payload["stroke"] = stroke
    return clip(ident, {"type": "shape", "shape": payload}, **extra)


def solid(color):
    return {"type": "solid", "color": color}


def text(ident, content, size, position, box, fill, **extra):
    return clip(ident, {"type": "text", "text": {
        "content": content, "font": FONT, "font_size": str(size),
        "position": list(map(str, position)), "box_size": list(map(str, box)),
        "fill": fill, **extra,
    }})


def track(name, c, **extra):
    return {"name": name, "clips": [c], **extra}


def main():
    EXAMPLES.mkdir(exist_ok=True)
    # A native nested source makes the keyed composite reproducible without
    # external footage or a browser. Original alpha/color math has pixel tests.
    source = {"name": "native-key-source", "output": OUTPUT, "tracks": [
        track("Screen", clip("green", solid(["0", "1", "0"]))),
        track("Disc", shape("disc", {"type": "ellipse", "center": ["910", "315"], "radius": ["155", "155"]},
            {"type": "radial_gradient", "center": ["865", "260"], "radius": "230", "interpolation": "linear", "stops": [
                {"offset": "0", "color": ["1", "0.75", "0.35"]},
                {"offset": "1", "color": ["0.8", "0.18", "0.08"]}]})),
        track("Cut", shape("cut", {"type": "path", "commands": [
            {"type": "move_to", "point": ["740", "360"]},
            {"type": "cubic_to", "control1": ["820", "260"], "control2": ["960", "405"], "to": ["1080", "270"]}]},
            None, {"paint": solid(["0.04", "0.08", "0.1"]), "width": "20", "cap": "round"})),
    ]}
    main_tl = {"name": "ferrocut-native-showcase", "output": OUTPUT, "tracks": [
        track("Background", clip("background", {"type": "linear_gradient", "start": ["0", "0"], "end": ["1280", "720"],
            "start_color": ["0.025", "0.06", "0.09"], "end_color": ["0.09", "0.16", "0.19"], "interpolation": "linear"}),
            effects=[{"type": "exposure", "stops": "1/4"}, {"type": "saturation", "amount": "0.8"}]),
        track("Frame", shape("frame", {"type": "rectangle", "x": "710", "y": "115", "width": "400", "height": "400", "radius": "28"},
            solid(["0.06", "0.12", "0.15"]), {"paint": solid(["0.22", "0.38", "0.39", "0.5"]), "width": "1"})),
        track("Keyed graphic", {"id": "keyed", "source": "native-key-source.json", "start": "0", "duration": "4",
            "effects": [{"type": "chroma_key", "key_color": ["0", "1", "0"], "tolerance": "0.12", "softness": "0.15", "spill": "0.4"}],
            "opacity": keys((0, 0), ("3/4", 1)), "transform": {"rotation": keys((0, -12), ("3/2", 0))}}),
        track("Kicker", text("kicker", "FERROCUT  /  NATIVE ENGINE", 18, (80, 90), (600, 40), ["0.4", "0.83", "0.76"], tracking="1.2")),
        track("Headline", text("headline", "CUT. COMPOSE.\nCREATE.", 66, (76, 155), (640, 190), ["0.95", "0.95", "0.9"],
            line_height="78", tracking="-1.5", animators=[{"selector": {"unit": "characters", "start": keys((0, 0), ("6/5", 22)), "end": "100"}, "opacity": "0"}])),
        track("Description", text("description", "A professional canvas for agents.", 24, (80, 370), (580, 80), ["0.59", "0.7", "0.72"], opacity=keys(("2/5", 0), ("6/5", 1)))),
        track("Accent", shape("accent", {"type": "rectangle", "x": "80", "y": "475", "width": keys(("1/5", 0), ("6/5", 520)), "height": "3", "radius": "1.5"},
            {"type": "linear_gradient", "start": ["80", "475"], "end": ["600", "475"], "stops": [
                {"offset": "0", "color": ["0.4", "0.83", "0.76"]}, {"offset": "1", "color": ["0.4", "0.83", "0.76", "0"]}]})),
        track("Capabilities", text("capabilities", "EDITING     MOTION     COMPOSITING", 15, (80, 505), (640, 40), ["0.49", "0.65", "0.66"], tracking="1")),
        track("Caption background", shape("caption-background", {"type": "rectangle", "x": "80", "y": "613", "width": "1120", "height": "58", "radius": "12"}, solid(["0.01", "0.025", "0.035", "0.65"]))),
    ]}
    style = {"content": "", "font": FONT, "font_size": "22", "position": ["100", "620"], "box_size": ["1080", "44"],
        "align": "center", "vertical_align": "center", "fill": ["0.84", "0.89", "0.87"]}
    revision = [{"op": "set_param", "clip": "headline", "param": "generator.text.content", "value": "EDIT. ANIMATE.\nCOMPOSE."}]
    for name, value in [("native-key-source.json", source), ("native-showcase.json", main_tl), ("native-caption-style.text-style", style), ("native-revise.ops", revision)]:
        (EXAMPLES / name).write_text(json.dumps(value, indent=2) + "\n")
    (EXAMPLES / "native-captions.srt").write_text("1\n00:00:00,000 --> 00:00:02,000\nNative typography, editable paths, and GPU keying.\n\n2\n00:00:02,000 --> 00:00:04,000\nBuilt on one exact, reversible timeline.\n")
    print("Wrote native showcase, keyed source, caption style/SRT and revision edits.")


if __name__ == "__main__":
    main()
