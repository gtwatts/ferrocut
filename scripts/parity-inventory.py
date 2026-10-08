#!/usr/bin/env python3
"""Maintain the researched capability ledger without turning row counts into parity.

Usage (stdlib Python only):
  python3 scripts/parity-inventory.py generate
  python3 scripts/parity-inventory.py triage
  python3 scripts/parity-inventory.py check
  python3 scripts/parity-inventory.py summary
  python3 scripts/parity-inventory.py run AE-150 [--check CHECK_ID]
  python3 scripts/parity-inventory.py self-test

Research columns come from the two Markdown inventories. `generate` preserves
per-ID Ferrocut reviews, but refuses removed/unknown IDs or invalid evidence.
`triage` only fills still-unreviewed rows; it never overwrites later assessments.
`run` executes explicitly registered argv arrays without a shell, records logs
and source fingerprints, and never promotes a capability automatically. Failed
reruns revoke verified/reviewed status. `check` does not execute acceptance tests
or fetch URLs: it verifies the canonical research and recorded local evidence.

To check a requirement off, set acceptance_complete only after its *whole*
proposed criterion is met. Register meaningful executable acceptance_checks,
then run them. Verified requires implementation/integration/test evidence and
an inspected render when applicable; reviewed additionally requires a recorded
agent production workflow and creative review. Test-source existence, a crate
build, and a render without inspection are not those later stages.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from urllib.parse import urlparse


ROOT = Path(__file__).resolve().parents[1]
LEDGER = Path("docs/parity/capabilities.json")
INVENTORIES = (
    ("PP", 227, "Adobe Premiere", "docs/parity/PREMIERE_PRO_INVENTORY.md", 8),
    ("AE", 229, "Adobe After Effects", "docs/parity/AFTER_EFFECTS_INVENTORY.md", 7),
)
STATUSES = {
    "unreviewed": "Official requirement recorded; Ferrocut support has not been assessed.",
    "missing": "An explicit code audit found the required behavior absent or rejected.",
    "partial": "Some behavior or a subsystem exists; recorded limitations remain.",
    "implemented": "Source implements the requirement; normal project integration is pending.",
    "integrated": "Source and the normal project/agent surface are wired; acceptance is pending.",
    "verified": "The full recorded criterion passed executable checks and applicable output review.",
    "reviewed": "Verified behavior also passed a demanding agent production and creative review.",
}
STAGES = {"audit", "implemented", "integrated", "tested", "rendered", "reviewed"}
FERROCUT_KEYS = {
    "status", "assessed_at", "acceptance_complete", "evidence", "limitations",
    "acceptance_checks", "output_review_required", "output_review_exemption",
    "render_reviews", "production_reviews", "notes", "needs_design",
}
CHECK_KEYS = {"id", "argv", "cwd", "env", "covers", "timeout_seconds", "result"}
SHELLS = {"sh", "bash", "dash", "zsh", "fish", "cmd", "powershell", "pwsh"}


class Invalid(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise Invalid(message)


def nonempty(value):
    return isinstance(value, str) and bool(value.strip())


def sha_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha_file(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def canonical_digest(value):
    return sha_bytes(json.dumps(value, sort_keys=True, separators=(",", ":")).encode())


def no_duplicate_keys(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, f"duplicate JSON object key: {key}")
        result[key] = value
    return result


def read_json(path):
    return json.loads(path.read_text(), object_pairs_hook=no_duplicate_keys)


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")
    temporary.replace(path)


def official_source(url):
    parsed = urlparse(url)
    host = parsed.hostname or ""
    return parsed.scheme == "https" and (host == "adobe.com" or host.endswith(".adobe.com"))


def sources_in(cell, references, context):
    sources = []
    for match in re.finditer(r"\[([^\]]+)\]\((https://[^\s)]+)\)|\[([^\]]+)\]\[([^\]]+)\]", cell):
        if match[2]:
            title, url = match[1], match[2]
        else:
            title, key = match[3], match[4].casefold()
            require(key in references, f"{context}: unresolved source reference [{key}]")
            url = references[key]
        require(official_source(url), f"{context}: nonofficial or invalid source URL {url}")
        sources.append({"title": title, "url": url})
    require(bool(sources), f"{context}: missing official source")
    return sources


def research(root):
    rows, manifests = [], []
    for prefix, expected_count, product, relative, width in INVENTORIES:
        path = root / relative
        data = path.read_bytes()
        lines = data.decode("utf-8").splitlines()
        references = {}
        for line in lines:
            match = re.match(r"^\[([^\]]+)\]:\s+(https://\S+)\s*$", line)
            if match:
                key = match[1].casefold()
                require(key not in references, f"{relative}: duplicate source definition [{key}]")
                references[key] = match[2]
        found = []
        for number, line in enumerate(lines, 1):
            if not re.match(r"^\|\s*(?:PP|AE)-\d+\s*\|", line):
                continue
            cells = [cell.strip().replace(r"\|", "|") for cell in re.split(r"(?<!\\)\|", line)[1:-1]]
            require(len(cells) == width, f"{relative}:{number}: expected {width} columns, got {len(cells)}")
            if prefix == "PP":
                identifier, family, capability, classification, channel, constraints, source, acceptance = cells
            else:
                identifier, family, classification, capability, constraints, source, acceptance = cells
                channel = "Beta" if classification.startswith("beta_") else "Stable documented"
            require(identifier.startswith(prefix + "-"), f"{relative}:{number}: unexpected ID {identifier}")
            require(all(nonempty(cell) for cell in cells), f"{identifier}: empty research column")
            found.append(identifier)
            rows.append({
                "id": identifier, "product": product, "family": family,
                "capability": capability, "class": classification,
                "adobe_channel": channel,
                "scope": "beta_watchlist" if channel == "Beta" else "stable",
                "constraints": constraints, "source_inventory": relative,
                "source_line": number, "source_markdown": source,
                "sources": sources_in(source, references, identifier),
                "proposed_acceptance": acceptance,
            })
        expected = [f"{prefix}-{number:03d}" for number in range(1, expected_count + 1)]
        require(len(found) == len(set(found)), f"{relative}: duplicate capability IDs")
        require(set(found) == set(expected), f"{relative}: ID set differs: missing={sorted(set(expected)-set(found))}, unknown={sorted(set(found)-set(expected))}")
        require(found == expected, f"{relative}: capability IDs are out of order")
        manifests.append({"path": relative, "sha256": sha_bytes(data), "product": product,
                          "prefix": prefix, "count": expected_count})
    return rows, manifests


def fresh_review():
    return {"status": "unreviewed", "assessed_at": None, "acceptance_complete": False,
            "evidence": [], "limitations": [], "acceptance_checks": [],
            "output_review_required": True, "output_review_exemption": None,
            "render_reviews": [], "production_reviews": [], "notes": [], "needs_design": False}


# These are conservative source-audit mappings, not fresh test-pass claims.
# Native text/vector/captions/finishing slices are intentionally left for the
# integration owner to assess against each complete Adobe requirement.
BASELINE = {
    "PP-005": ("partial", [("crates/ferrocut-engine/src/media/decode.rs", "Pixel::RGBA", "FFmpeg decode exists; upload currently quantizes to RGBA8."), ("crates/ferrocut-engine/tests/frames.rs", "", "Codec/decode tests exist; their existence is not a recorded acceptance run.")], ["No published tested camera/codec/platform support matrix.", "High-bit-depth source data is reduced to eight-bit RGBA before composition."]),
    "PP-013": ("partial", [("crates/ferrocut-engine/src/compile.rs", "pub fn compile_proxies", "Compiler can substitute generated proxy paths."), ("crates/ferrocut-engine/tests/proxy.rs", "", "Proxy generation and render test source exists.")], ["Full requirement, progress/error behavior and end-to-end original/proxy identity have not been assessed against recorded acceptance."]),
    "PP-023": ("partial", [("crates/ferrocut-engine/src/markers.rs", "pub fn list", "Marker types, IDs, time mapping and listing exist."), ("crates/ferrocut-engine/tests/markers.rs", "", "Marker edit/time-mapping test source exists.")], ["Interchange/export mapping and full clip/sequence annotation acceptance remain unverified."]),
    "PP-028": ("partial", [("crates/ferrocut-engine/src/timeline.rs", "pub struct Timeline", "Timeline stores rational frame rate and frame geometry.")], ["Preview settings and differing-size nested composition behavior do not implement the full sequence configuration requirement."]),
    "PP-029": ("partial", [("crates/ferrocut-engine/src/timeline.rs", "pub struct Track", "Timeline has separate tracks and clips."), ("crates/ferrocut-engine/src/edit.rs", "AddTrack", "Typed editing includes adding tracks.")], ["No complete typed track rename/reorder/remove control suite was found in the baseline edit operations."]),
    "PP-048": ("partial", [("crates/ferrocut-engine/src/compile.rs", "", "Compiler resolves nested composition sources."), ("crates/ferrocut-engine/tests/nest.rs", "", "Nested composition source, edit and cache tests exist.")], ["Nested compositions currently must match parent frame geometry.", "A recorded shared-child revision production test is still required."]),
    "PP-053": ("missing", [("crates/ferrocut-engine/src/timeline.rs", "c.sampling != Sampling::OpticalFlow", "Timeline validation explicitly rejects optical-flow sampling; frame mixing is separate.")], ["Motion estimation and synthesized intermediate frames are not implemented.", "No occlusion/artifact reviewed slow-motion production fixture exists."]),
    "PP-102": ("partial", [("crates/ferrocut-engine/src/audio.rs", "", "Audio pipeline provides loudness and dynamics processing."), ("crates/ferrocut-engine/tests/mixdown.rs", "", "Audio mixdown test source exists.")], ["The group normalization requirement and listening/clipping acceptance have not been recorded for this ledger."]),
    "PP-113": ("partial", [("crates/ferrocut-engine/src/fx/mod.rs", "pub fn ensure_builtins", "Effect registry publishes parameterized native effect definitions."), ("crates/ferrocut-mcp/src/schema.rs", "", "MCP schema exposes effect and parameter metadata.")], ["Full discovery/apply/resolved-parameter acceptance needs a recorded fresh agent workflow."]),
    "PP-118": ("partial", [("crates/ferrocut-engine/src/transform.rs", "", "Native animated clip transform implementation exists."), ("crates/ferrocut-engine/tests/transform.rs", "", "Transform render and numeric test source exists.")], ["Recorded explicit-anchor acceptance and inspected animation are pending."]),
    "PP-156": ("partial", [("crates/ferrocut-engine/tests/perceive.rs", "", "Technical perception/scopes integration test source exists.")], ["Full calibrated waveform, RGB/YUV parade, vectorscope and histogram requirement is not demonstrated by this audit.", "Dedicated MCP rendered-frame and scope inspection surface is incomplete."]),
    "PP-158": ("partial", [("crates/ferrocut-engine/src/compositor.rs", "SOURCE_SPACE", "Compositor works in linear ACEScg with fixed Rec.709 input/output assumptions.")], ["Per-source color metadata and configurable project input/working/output transforms are absent.", "The separate OCIO crate is not wired into the normal timeline compiler."]),
    "PP-170": ("partial", [("crates/ferrocut-engine/src/media/encode.rs", "Pixel::BGRZ", "Eight-bit FFV1 master encoding exists."), ("crates/ferrocut-engine/src/deliver.rs", "", "Optional H.264/AAC delivery integration exists.")], ["Only a limited output matrix exists; high-bit-depth, alpha and HDR mastering are missing.", "H.264 availability depends on the runtime encoder dependency and explicit enablement."]),
    "PP-203": ("partial", [("crates/ferrocut-mcp/src/lib.rs", "fn tools", "MCP exposes typed tool discovery and structured edit/render operations."), ("crates/ferrocut-mcp/src/schema.rs", "", "Schemas and parameter metadata are discoverable.")], ["No complete native capability/dependency availability and minimum-version query was found.", "This is an agent API equivalent, not compatibility with Adobe's API."]),
    "AE-015": ("partial", [("crates/ferrocut-engine/src/compositor.rs", "", "Working textures use linear ACEScg RGBA16F."), ("crates/ferrocut-engine/src/media/decode.rs", "Pixel::RGBA", "Input conversion currently bottlenecks at eight-bit RGBA."), ("crates/ferrocut-engine/src/media/encode.rs", "Pixel::BGRZ", "Output conversion currently bottlenecks at eight-bit BGR0.")], ["No selectable 8/16/32-bpc processing contract; storage is half-float.", "Higher input precision and overrange/alpha output are not preserved end to end."]),
    "AE-017": ("partial", [("crates/ferrocut-engine/src/compositor.rs", "", "Working-space compositing is linear ACEScg."), ("crates/ferrocut-engine/tests/blend.rs", "", "Blend render reference test source exists.")], ["Source/output transforms remain fixed; project linearization/blending settings and full reference acceptance are pending."]),
    "AE-018": ("partial", [("crates/ferrocut-color/src/node.rs", "OcioTransformNode", "Separate OCIO CPU/GPU render node exists."), ("crates/ferrocut-engine/Cargo.toml", "", "The engine does not depend on the OCIO extension crate.")], ["OCIO is subsystem source evidence only, not normal timeline/MCP integration.", "Config/input/display/output selection, resource hashing and precise media I/O are pending.", "Parallel OCIO OOM regression crashed during baseline; serial GPU/CPU runs passed, cause remains unresolved."]),
    "AE-019": ("partial", [("crates/ferrocut-color/src/node.rs", "OcioTransformNode", "Separate transform node provides a future display/output integration boundary."), ("crates/ferrocut-engine/src/compositor.rs", "OUTPUT_SPACE", "Actual engine output uses fixed Rec.709 conversion.")], ["No independently selectable project display/view versus archival output transform is integrated."]),
    "AE-025": ("partial", [("crates/ferrocut-engine/src/compile.rs", "", "Nested composition compilation exists."), ("crates/ferrocut-engine/tests/nest.rs", "", "Nested source reuse and edit/cache test source exists.")], ["Child frame dimensions must currently match parent dimensions.", "Recorded shared-child revision acceptance is pending."]),
    "AE-043": ("partial", [("crates/ferrocut-engine/tests/blend.rs", "", "Implemented blend modes have numeric render test source.")], ["The supported native blend subset does not include all Adobe stencil/silhouette behaviors.", "No full requirement acceptance run is recorded."]),
    "AE-054": ("partial", [("crates/ferrocut-engine/src/transform.rs", "", "Transform/camera motion can be temporally sampled."), ("crates/ferrocut-engine/tests/layer3d.rs", "", "2.5D camera and motion sampling test source exists.")], ["Samples reuse the same content frame, so source-content motion is not blurred.", "Controlled-shutter visual acceptance is pending."]),
    "AE-057": ("partial", [("crates/ferrocut-engine/src/retime.rs", "", "Nearest and frame-mix retiming exist."), ("crates/ferrocut-engine/src/timeline.rs", "c.sampling != Sampling::OpticalFlow", "Timeline validation explicitly rejects optical-flow sampling."), ("crates/ferrocut-engine/tests/retime.rs", "", "Retime test source exists.")], ["Pixel-motion interpolation is missing; frame mixing cannot satisfy that part of the criterion."]),
    "AE-058": ("partial", [("crates/ferrocut-engine/src/expr.rs", "", "Existing uncommitted Rhai expression evaluation and references are present."), ("crates/ferrocut-engine/tests/expressions.rs", "", "Expression sandbox, type and integration test source exists.")], ["Rhai is the documented planned language, not Adobe JavaScript compatibility.", "Full property coverage and a recorded agent-authored acceptance workflow remain to be assessed."]),
    "AE-105": ("partial", [("crates/ferrocut-engine/src/compile.rs", "", "Native alpha/luma matte compilation exists.")], ["Baseline mattes use adjacency/consumption; arbitrary reusable nonadjacent matte references are not supported."]),
    "AE-128": ("partial", [("crates/ferrocut-engine/tests/layer3d.rs", "", "Native planar 2.5D transform/camera render tests exist.")], ["Painter ordering has no depth buffer or correct intersecting-plane visibility.", "Parenting, lighting, shadows, depth of field and mesh geometry are not native supported capabilities."]),
    "AE-129": ("partial", [("crates/ferrocut-engine/src/timeline.rs", "", "Timeline supports one camera and point-of-interest camera properties."), ("crates/ferrocut-engine/tests/layer3d.rs", "", "Camera projection/animation test source exists.")], ["Full one-node/two-node camera, lens and orientation contract is incomplete.", "Inspected camera-orbit acceptance is pending."]),
    "AE-150": ("partial", [("crates/ferrocut-engine/src/fx/mod.rs", "", "Ordered animated native effect stacks are integrated."), ("crates/ferrocut-engine/tests/effects.rs", "", "Effect order, edits and cache reference test source exists.")], ["Effect masks and the full effect-compositing options requirement are not implemented.", "Recorded noncommutative reorder acceptance and inspected render are pending."]),
    "AE-177": ("partial", [("crates/ferrocut-color/src/node.rs", "OcioTransformNode", "Separate OCIO transform subsystem exists.")], ["Normal timeline/MCP projects cannot yet select a color LUT/config resource.", "A known-LUT chart through the actual compiler is pending."]),
    "AE-180": ("partial", [("crates/ferrocut-engine/src/audio.rs", "", "Native stereo timing, mixing, retiming and level processing exist."), ("crates/ferrocut-engine/tests/mixdown.rs", "", "Audio sample-alignment and mixing test source exists.")], ["Advanced channel routing is absent; full trim/retime audio acceptance and listening review are not recorded."]),
    "AE-200": ("partial", [("crates/ferrocut-engine/src/render.rs", "", "Render pipeline supports workers and frame/chunk caching."), ("crates/ferrocut-engine/tests/streaming.rs", "", "Streaming/worker integration test source exists.")], ["Full memory-budget, parallel/sequential output and long-production reliability acceptance are not recorded."]),
    "AE-214": ("partial", [("crates/ferrocut-ofx/src/node.rs", "OfxNode", "Separate out-of-process OpenFX render-node subsystem exists."), ("crates/ferrocut-ofx/tests/isolation.rs", "", "Plugin host isolation/recovery test source exists.")], ["OpenFX adapter is not integrated into engine effect discovery or normal timeline/MCP projects.", "Adobe native plugin binary/API compatibility is not provided or implied.", "Capability/version/resource discovery and representative plugin acceptance are pending."]),
}


PATHS = {
    "timeline": "crates/ferrocut-engine/src/timeline.rs",
    "edit": "crates/ferrocut-engine/src/edit.rs",
    "project": "crates/ferrocut-engine/src/project.rs",
    "main": "crates/ferrocut-engine/src/main.rs",
    "compile": "crates/ferrocut-engine/src/compile.rs",
    "comp": "crates/ferrocut-engine/src/comp.rs",
    "params": "crates/ferrocut-engine/src/params.rs",
    "mcp": "crates/ferrocut-mcp/src/lib.rs",
    "schema": "crates/ferrocut-mcp/src/schema.rs",
    "dependencies": "crates/ferrocut-engine/Cargo.toml",
    "decode": "crates/ferrocut-engine/src/media/decode.rs",
    "probe": "crates/ferrocut-engine/src/media/probe.rs",
    "proxy": "crates/ferrocut-engine/src/media/proxy.rs",
    "encode": "crates/ferrocut-engine/src/media/encode.rs",
    "deliver": "crates/ferrocut-engine/src/deliver.rs",
    "index": "crates/ferrocut-engine/src/index/mod.rs",
    "search": "crates/ferrocut-engine/src/index/search.rs",
    "shots": "crates/ferrocut-engine/src/index/shots.rs",
    "whisper": "crates/ferrocut-engine/src/index/whisper.rs",
    "text": "crates/ferrocut-engine/src/text.rs",
    "vector": "crates/ferrocut-engine/src/vector.rs",
    "captions": "crates/ferrocut-engine/src/captions.rs",
    "audio": "crates/ferrocut-engine/src/audio.rs",
    "audiofx": "crates/ferrocut-engine/src/audio_fx.rs",
    "analysis": "crates/ferrocut-audio/src/analysis.rs",
    "dsp": "crates/ferrocut-audio/src/effects.rs",
    "loudness": "crates/ferrocut-audio/src/loudness.rs",
    "perceive": "crates/ferrocut-engine/src/perceive.rs",
    "scopes": "crates/ferrocut-perceive/src/scopes.rs",
    "markers": "crates/ferrocut-engine/src/markers.rs",
    "transform": "crates/ferrocut-engine/src/transform.rs",
    "layer3d": "crates/ferrocut-engine/src/layer3d.rs",
    "retime": "crates/ferrocut-engine/src/retime.rs",
    "keyframe": "crates/ferrocut-types/src/keyframe.rs",
    "expr": "crates/ferrocut-engine/src/expr.rs",
    "fx": "crates/ferrocut-engine/src/fx/mod.rs",
    "native": "crates/ferrocut-engine/src/fx/native.rs",
    "finishing": "crates/ferrocut-engine/src/fx/finishing.rs",
    "compositor": "crates/ferrocut-engine/src/compositor.rs",
    "ocio": "crates/ferrocut-color/src/node.rs",
    "colorspace": "crates/ferrocut-colorspace/src/named.rs",
    "ofx": "crates/ferrocut-ofx/src/node.rs",
    "render": "crates/ferrocut-engine/src/render.rs",
    "graph": "crates/ferrocut-engine/src/graph.rs",
    "diff": "crates/ferrocut-engine/src/diff.rs",
    "blend": "crates/ferrocut-engine/src/blend.rs",
}


# Deliberate per-requirement source triage. Requirements not listed here or in
# BASELINE have no matching native model/effect/backend in the audited source
# families below. That absence is recorded, not inferred from Adobe UI names.
TRIAGE = {}


def assess(selector, status, paths, behavior, limitations=(), decision=None):
    prefix, numbers = selector.split(":")
    for part in numbers.split(","):
        limits = [int(value) for value in part.split("-")]
        for number in range(limits[0], limits[-1] + 1):
            identifier = f"{prefix}-{number:03d}"
            require(identifier not in TRIAGE, f"duplicate triage policy {identifier}")
            TRIAGE[identifier] = (status, paths.split(), behavior, list(limitations), decision)


assess("PP:1", "partial", "project timeline comp", "Persistent JSON timelines, snapshots and nested composition files exist.", ["No multi-sequence project container or duplicate-with-independent-project-identity operation is defined."])
assess("PP:7", "partial", "decode probe", "FFmpeg supplies some still-image decoding through the video media path.", ["No declared large-still size/alpha fixture matrix or dedicated still duration/interpretation contract is recorded."])
assess("PP:12", "partial", "proxy encode main", "Proxy generation already transcodes supported source video.", ["General ingest transcode jobs, configurable mezzanine profiles and source timecode retention are absent."])
assess("PP:14", "partial", "proxy compile main", "Generated content-hashed proxies can be selected for draft rendering.", ["Arbitrary proxy association/attach/detach/reconnect editing is not modeled; generated paths are derived."])
assess("PP:15", "integrated", "compile render main", "Draft proxy compilation and final original-media rendering preserve the timeline edits; delivery explicitly restores originals.")
assess("PP:16", "integrated", "project mcp", "media_status lists missing source paths and referencing clips while the persisted timeline retains their identities.")
assess("PP:17", "partial", "edit project", "Relink supports explicit paths, directory prefixes and file-name search.", ["Relink does not disambiguate by tape/timecode and acquisition metadata."])
assess("PP:20", "partial", "probe markers mcp", "Probe returns technical stream information; editable markers carry names/comments.", ["No general technical/descriptive metadata field model, mutability policy or asset metadata editor exists."])
assess("PP:24", "partial", "markers encode", "Generic timed point/range markers are persistent.", ["Chapter/segment marker types and container-specific delivery mappings are missing."])
assess("PP:26", "partial", "search index mcp", "Source transcripts support ranked phrase/word search with exact temporal ranges.", ["General searchable descriptive metadata and saved metadata queries are absent."])
assess("PP:27", "partial", "mcp perceive diff", "timeline_get exposes a flat clip list; media_status and quality_check expose source/QC results.", ["No unified queryable Sequence Index, CSV export or mapping from every output QC issue back to editorial rows exists."])
assess("PP:30,33,36,38-42", "integrated", "edit params mcp", "Typed editorial operations explicitly address track/clip IDs, validate source handles and preserve rational timing through the normal agent API.")
assess("PP:32", "partial", "edit audio probe", "Source ranges and video/audio destination tracks are explicit; clip audio can be muted.", ["Per-source stream-index selection and independent video/audio source patching are not implemented."])
assess("PP:34", "partial", "edit", "Move supports exact time positions and same-kind destination tracks with collision checks.", ["No persistent grouping or atomic group-nudge operation exists."])
assess("PP:35", "partial", "edit", "RippleDelete closes a removed clip's interval on its track or all tracks.", ["A distinct lift/delete-with-gap operation is missing."])
assess("PP:43", "partial", "edit project mcp", "Edit arrays are checked and persisted atomically, with dry-run change summaries.", ["Asymmetrical trim selection and coordinated multi-boundary preview are not modeled; each sequential op must pass intermediate validation."], "Use structured boundary selections and a render-impact preview, without requiring desktop trim-mode gestures.")
assess("PP:44", "partial", "edit timeline", "Ripple operations can affect the addressed track or every track.", ["Persistent mixed per-track sync-lock state and a selected participation policy are absent."])
assess("PP:46", "partial", "timeline edit", "Video clips retain linked audio mappings and J/L offsets.", ["General groups, unlink/link selection and independent synchronization metadata are absent."])
assess("PP:50-52", "integrated", "retime edit compile", "Constant/reverse speed, duration changes, keyframed speed/time maps, frame holds and nearest/frame-mix sampling reach the native compiler.")
assess("PP:56", "partial", "index whisper mcp", "Content-hashed whisper.cpp indexing records source words/segments, model hash, language and device provenance.", ["A composition/sequence transcript assembled from edited clips is not implemented.", "Execution depends on a local whisper CLI/model; this audit is not a fresh speech accuracy run."])
assess("PP:59,63", "partial", "search edit mcp", "Transcript search returns source ranges padded and aligned for the existing typed assembly/trim operations.", ["No transcript-edit or Paper Edit plan model synchronizes word deletion/reordering with sequence edits.", "Agent composition of primitives still needs a documented, accepted end-to-end transcript editorial workflow."], "Expose utterance/range edit plans rather than duplicating a desktop transcript panel.")
assess("PP:60", "integrated", "search index mcp", "transcript_search returns ranked dialogue hits, source ranges and frame-aligned cut suggestions.")
assess("PP:64", "partial", "index main", "The media index persists/exportable JSON transcript words and times.", ["No typed transcript correction operation or standalone editable transcript format contract exists."])
assess("PP:65", "partial", "shots index mcp", "Source indexing calls an optional detector and records boundaries/confidence or explicit unavailability.", ["Detector availability and real cut/dissolve/fade accuracy are not established by source existence."])
assess("PP:66", "partial", "search params edit", "Batch edit arrays can mute matched source clips using transcript ranges and audio.mute.", ["Bulk lexical censor plans and synthesized bleep audio are missing."])
assess("PP:67", "integrated", "text generator compile params schema", "Native editable UTF-8 text, pinned font assets and text rendering are wired into generator clips, parameter edits and agent schemas.")
assess("PP:68", "integrated", "text params compile", "Native no-wrap text and constrained paragraph boxes/wrapping are editable and wired into ordinary generator clips; Unicode shaping preserves stable line composition.")
assess("PP:69", "partial", "text params compile", "Native text supports explicit font face/size, tracking, line height and alignment.", ["Character style runs and an agent-visible measured text-bounds query are incomplete."])
assess("PP:70", "partial", "text native", "Native glyph fill and one outside stroke exist; ordered drop-shadow effects can style the result.", ["Multiple native glyph strokes and independent text-style shadow controls are absent."])
assess("PP:73", "partial", "text params timeline", "Font paths/fallback assets are explicit, settable and hashed on compilation.", ["No project-wide font replacement plan or missing-font substitution report exists."])
assess("PP:74-75", "partial", "text", "The native shaping/raster path handles Unicode clusters and has a color-glyph branch using explicit font assets.", ["No published color-font/emoji format, sequence and fallback support matrix or representative color-glyph review is recorded."])
assess("PP:76", "integrated", "vector generator compile params", "Editable open/closed quadratic/cubic path commands, fills and strokes render as native generator clips.")
assess("PP:80", "partial", "comp compile edit", "Reusable nested compositions can contain source graphics and update their instances on recompilation.", ["No dedicated source-graphic asset/template interface exists; nested composition dimensions must match the parent."])
assess("PP:85-86", "partial", "index captions text", "Source word timings, editable caption interchange and native word/character text selectors exist.", ["Automatic transcript-to-cue segmentation, single-word cue generation and editorial reflow are not implemented."])
assess("PP:87", "partial", "captions text params", "Caption imports create editable native text clips with a shared initial text style.", ["No linked caption-track style object or restyle-all-cues operation exists."])
assess("PP:88", "integrated", "captions main edit", "Plain SRT/WebVTT imports become editable native text clips; unsupported rich markup/regions/embedded streams are rejected explicitly.")
assess("PP:90,181", "partial", "captions main encode", "Native caption text can burn into video and round-trip plain SRT/WebVTT sidecars.", ["Embedded caption encoding and a complete selected transcript/text-track export contract are missing.", "Rich subtitle styles are rejected rather than silently carried into unsupported sidecars."])
assess("PP:91", "partial", "audio timeline", "Sources may have multiple decoded channels, but the native program/master is stereo.", ["Adaptive and 5.1 track/bus layouts are not implemented."])
assess("PP:93", "partial", "analysis loudness mcp", "Audio analysis exposes block energy, loudness and peak measurements.", ["No multichannel min/max waveform-envelope API at arbitrary time range/resolution exists."], "Provide numerical waveform/envelope queries and optional plots for agent/human review.")
assess("PP:94,97,106-107,109", "integrated", "audio audiofx params mcp", "Native audio gain automation, ordered clip/track effects, EQ/filters, dynamics and loudness/peak reports are wired into timeline editing and rendering.")
assess("PP:95", "partial", "timeline audio params", "Track buses expose gain, pan, mute and ordered effects.", ["Solo and a separate monitoring mixer are missing."])
assess("PP:96", "partial", "timeline audio", "Per-track buses process audio before a stereo master.", ["Arbitrary submix/send routing, routing graphs and cycle detection are not implemented."])
assess("PP:98-99", "partial", "params keyframe audio", "Gain/pan and effect automation can be authored as exact keyframes.", ["Recorded Write/Touch/Latch automation and real-time clip mixer capture do not exist."], "Prefer explicit automation curves/imports for agents; decide whether a human live mixer is a separate surface.")
assess("PP:100", "partial", "timeline audio", "Native fades/crossfades implement linear and equal-power laws.", ["The broader Adobe fade-law collection, including exponential behavior, is incomplete."])
assess("PP:103", "partial", "analysis audio params", "Track sidechain ducking derives gain from dialogue with attack/release controls.", ["Generated ducking envelopes are analysis controls, not a persisted editable keyframe-conversion workflow."])
assess("PP:114", "partial", "fx edit params", "Video stacks support IDs, reordering, bypass and removal; audio chains have typed edits.", ["Reusable chain copy/paste with remapped identities and a single chain-level operation is missing."])
assess("PP:116", "partial", "fx compile timeline", "Adjustment clips process the composite of lower tracks through native effect stacks.", ["The current compiler rejects adjustment layers in timelines containing 3D layers."])
assess("PP:117", "partial", "comp compile", "Effects inside a reused nested composition update all its instances.", ["Media-item-level source effects distinct from composition/clip effects are not modeled."])
assess("PP:118,120,122,133", "integrated", "transform keyframe blend edit compile mcp", "Native transforms, numeric keyframes, representative alpha-aware blend modes and exact dissolve alignment are addressable through the real timeline/agent surface.")
assess("PP:119", "partial", "vector text transform compile", "Paths and glyphs are natively rasterized from editable geometry at the generator canvas resolution.", ["Clip/nested transforms can resample a rasterized source; continuous rasterization/collapse-transform semantics are not implemented."])
assess("PP:121", "partial", "keyframe transform", "Temporal interpolation includes hold, linear, cubic Bezier, speed/influence and Easy Ease.", ["Spatial Bezier motion paths, roving/path velocity and a complete path inspection API are absent."])
assess("PP:123", "partial", "finishing fx compile", "Native chroma key has encoded-space threshold, softness and luma-preserving despill.", ["Matte cleanup, uneven-screen adaptation and production hair/motion-blur acceptance remain absent.", "This is original math, not an Ultra Key or proprietary keyer implementation."])
assess("PP:124-126", "partial", "vector blend compile native", "Animated native rectangle/ellipse/Bezier generators can supply adjacent alpha/luma track mattes; blur and inversion primitives exist.", ["A per-layer/per-effect mask stack, add/subtract/intersect operators, expansion and arbitrary reusable mask references are missing."])
assess("PP:130", "partial", "finishing blend compile", "Luma/chroma keyers can generate alpha and the compiler supports alpha/luma mattes.", ["HSL range selection, a reusable named matte asset and secondary refinement controls are missing."])
assess("PP:134", "partial", "edit", "Dissolve edits validate source handles and reject insufficient media with diagnostics.", ["An explicitly selectable repeated-frame/shortened-transition fallback policy is absent."])
assess("PP:135", "partial", "edit nodes", "Native dissolves are timed and composited through sequence nodes.", ["Wipe and directional-move transition implementations are absent."])
PATHS["nodes"] = "crates/ferrocut-engine/src/nodes.rs"
assess("PP:139", "partial", "native fx", "Gaussian/directional blur and sharpen/unsharp native effects exist.", ["Matte-driven/compound blur and the broader blur family are missing."])
assess("PP:140", "partial", "native fx", "A native threshold/radius glow effect is present.", ["Light rays, lens flare and RGB-split treatments are not implemented as native effects."])
assess("PP:145-146,151", "partial", "finishing fx compile", "New native exposure, pivot contrast, saturation and RGB lift/gamma/gain are wired into effect stacks.", ["Temperature/tint white balance, separate tonal-range controls and a complete three-way wheel correction model are missing."])
assess("PP:147", "partial", "ocio dependencies", "The separate OCIO subsystem supports configured CPU/GPU transforms.", ["Engine timelines cannot yet select/hash LUT/config assets or apply a pinned input/custom LUT through ordinary agent operations."])
assess("PP:152", "partial", "scopes perceive", "Rendered technical scope reports and output comparisons provide some reference-analysis primitives.", ["No native reference-shot view, automatic shot matching or reversible matched-grade proposal exists."], "Expose paired reference/shot measurements and editable grade proposals, rather than reproducing a comparison-view UI.")
assess("PP:159", "partial", "colorspace compositor encode", "Named transfer/primary conversion code exists; actual engine output is fixed to encoded Rec.709.", ["No configurable SDR/HLG/PQ output pipeline, matching tags or high-depth output exists."])
assess("PP:164", "partial", "ocio compositor", "Separate OCIO transforms offer a possible display-preview boundary; current engine conversion is fixed.", ["Managed display/HDR monitoring and independently selectable preview transforms are not integrated."], "A declared preview transform/artifact can serve agents; external HDR display support is a separate hardware capability.")
assess("PP:165,169", "partial", "render graph main mcp", "Full-sequence render/chunk reuse/invalidation, cancellation and progress reporting exist.", ["Selected time/frame range rendering/export is not exposed as a normal CLI/MCP job."])
assess("PP:172", "partial", "timeline encode main", "Project width/height/rational frame rate drive output encoding.", ["Independent delivery geometry/rate conversion, pixel-aspect and field controls are absent."])
assess("PP:173", "partial", "deliver main", "Optional H.264 delivery validates a constant QP control.", ["CBR/VBR bitrate, profile/level and comprehensive encoder preset controls are absent."])
assess("PP:174", "partial", "compositor encode transform", "The internal compositor uses half-float pixels and native spatial resampling.", ["No maximum-depth/quality-mode contract exists; media I/O is still eight-bit."])
assess("PP:189-190", "partial", "project comp", "Local journal snapshots, branches/merges and separate nested composition files exist.", ["Cloud publication/offline synchronization and a Productions shared multi-project container are absent."], "Separate local reversible collaboration primitives from optional cloud/team providers and shared-storage project ownership.")
assess("PP:193-194", "partial", "markers project mcp", "Timed comments/markers can be authored through structured edits.", ["Frame.io comment/version import, status/provenance mapping and frame-anchor reconciliation are absent."], "Define a provider-neutral review-note format and an optional Frame.io adapter; do not assume Adobe service compatibility.")
assess("PP:198", "partial", "edit comp compile", "Nest/unnest promotes timeline ranges into native editable composition files while preserving timing.", ["Adobe composition replacement/Dynamic Link interoperability is absent; parent/child frame geometry is constrained."], "The unified native composition workflow can satisfy the product need; decide separately whether Adobe project interchange is required.")
assess("PP:200", "partial", "decode compile project", "Recognized source file bytes participate in compilation/cache identity, allowing refresh on recompilation.", ["Layered PSD import, explicit external-editor handoff and edit-return dependency tracking are absent."], "Specify supported flattened/layered file interchange without promising a Photoshop API round trip.")
assess("PP:202,206", "integrated", "mcp schema main edit", "MCP tool discovery, strict JSON schemas, an authoring guide and CLI help expose a headless programmatic editing workflow.", decision="The intended equivalent is the native agent API; Adobe UXP/JavaScript and keyboard command-map compatibility are not provided.")
assess("PP:204-205", "partial", "ofx dependencies fx", "An isolated OpenFX adapter exists as a separate crate; the engine has a native extensible effect trait/registry.", ["The OFX crate is not wired into normal timeline effect discovery, and Adobe hybrid/C++ extension ABI or hardware-plugin compatibility is absent."], "Choose supported native extension/codec APIs and licenses explicitly; Adobe plug-in ABI equivalence is not implied.")
assess("PP:207", "partial", "render perceive proxy", "Full renders, proxy drafts, contact sheets and technical output checks provide offline preview primitives.", ["Source/composed range playback, synchronized interactive audio and explicit preview transforms are absent."], "Design direct frame/range/audio inspection for agents and a suitable optional human review player.")
assess("PP:208-209", "partial", "mcp schema main", "Tools are self-describing and the project guide/schema are discoverable on demand.", ["Task-specific editing/sound/graphics/grading context bundles and a human accessible review UI are absent."], "Prioritize concise task context and keyboard/assistive review controls over cloning desktop workspace layouts.")
assess("PP:210-212", "partial", "project main mcp", "Atomic timeline writes, append-only edits, retained content snapshots, undo, branches and explicit checkout exist.", ["Redo, scheduled autosave/retention and a crash-session restore-selection workflow are absent.", "No interruption/fault-injection acceptance for the complete requirement is recorded."])
assess("PP:213", "integrated", "edit project mcp render", "Structured diagnostics name operations/objects, preserve failed-edit atomicity and report render progress/cancellation/failure details.")
assess("PP:214", "partial", "graph render", "RAM frame reuse has bounded LRU behavior and disk chunks are content addressed.", ["Age/size disk-media-cache eviction policy and project-integrity eviction acceptance are absent."])
assess("PP:227", "partial", "mcp schema edit", "External AI clients can discover and execute the native project assembly/editing API.", ["No embedded Premiere-style cloud assistant, preparation dialogue or hosted assistant state is implemented."], "Ferrocut is designed for external agents; decide whether a separate embedded assistant is useful rather than requiring Adobe's beta service.")

assess("AE:1-2", "partial", "project timeline", "Native JSON project state, content snapshots, history and branch recovery exist.", ["Adobe AEP/AET compatibility, reusable template packaging and full crash-session recovery/version UI are absent."])
assess("AE:4", "partial", "edit project mcp", "Relink replaces explicit paths/prefixes or file-name matches and preserves referenced clip timing.", ["General footage-item replacement/interpretation and source-metadata disambiguation are incomplete."])
assess("AE:5", "partial", "proxy compile main", "Generated proxies can substitute sources for draft rendering; final rendering uses originals.", ["Arbitrary user-attached proxy relationships and per-item proxy toggles are not modeled."])
assess("AE:7", "partial", "probe decode", "FFmpeg probes/imports supported audiovisual/still containers through the media path.", ["No comprehensive tested still/camera/codec matrix or high-depth import contract is recorded."])
assess("AE:13", "partial", "markers timeline", "Native source/timeline markers persist rational times and comments.", ["XMP import/export and cross-application temporal metadata are absent."])
assess("AE:14", "partial", "shots index mcp", "Optional shot detection supplies source boundaries and confidence or explicit unavailability.", ["Available detector binaries and production scene-detection accuracy require a recorded execution test."])
assess("AE:22-23", "partial", "timeline graph fx render", "Composition frame geometry/rate and internal render-region/data-window contracts exist.", ["Independent work-area/duration controls and agent-exposed ROI jobs are missing."])
assess("AE:26", "partial", "edit comp compile", "Nest/unnest moves timeline content into composition files and resolves child render order.", ["Precompose move-attributes versus leave-attributes selection and collapse-transform behavior are incomplete."])
assess("AE:29", "partial", "comp timeline mcp", "Source traversal follows nested composition dependencies for compile/root/media inspection.", ["No dedicated queryable composition dependency graph or navigation query exists."], "Expose a structured dependency graph with stable composition IDs and cycle diagnostics.")
assess("AE:30", "partial", "timeline generator text vector layer3d", "Footage, solid/gradient, text, shape, adjustment and camera concepts exist in the native model.", ["Native null and light layers are missing."])
assess("AE:31", "partial", "compile fx timeline", "Adjustment clips process lower-track composites through native effect stacks.", ["Adjustment layers are rejected when the timeline contains 3D layers."])
assess("AE:32,34", "integrated", "timeline edit compile mcp", "Source reuse and explicitly timed multi-layer sequencing are non-destructive and available through normal typed edits.")
assess("AE:33", "partial", "edit timeline", "Native clips support split, trim, slip and movement between existing tracks.", ["A complete layer/track reorder operation is not exposed."])
assess("AE:36", "partial", "params audio timeline", "Clip opacity and clip/track audio mute control part of layer visibility/audibility.", ["Separate visual visibility/solo switches and a preview-specific visibility contract are absent."])
assess("AE:39,45-47,50", "integrated", "params keyframe transform markers edit compile", "Native transforms/opacity, persistent markers and rational keyframes include temporal Bezier, independent speed/influence and Easy Ease through normal editing.")
assess("AE:44", "partial", "native fx", "Ordered native glow/drop-shadow effects reproduce some common layer styling.", ["A layer-style model with inner styles, bevel/emboss and complete style compositing is absent."])
assess("AE:49", "partial", "keyframe diff mcp", "Serialized keys/easing and structured keyframe diffs are inspectable through the agent API.", ["No sampled value/velocity graph query or direct graph-edit workflow exists."], "Provide numerical curve samples, derivatives and edits rather than requiring a graphical curve editor.")
assess("AE:52", "partial", "keyframe edit", "Keyframes retain rational times, can be replaced/merged, and shift with trims/splits.", ["General time/value scaling and reusable animation application operations are missing."])
assess("AE:55-56", "integrated", "retime edit compile params", "Uniform speed/duration changes and nonlinear source-time remapping are native editable timeline properties.")
assess("AE:59", "partial", "expr comp", "Expressions reference parameters on other layers/tracks and the current timeline with cycle detection.", ["Cross-composition property references are not supported.", "Source expressions have documented restrictions for nonmonotonic/frozen timeline-dependent mappings."])
assess("AE:60,62,66", "integrated", "expr keyframe params compile schema", "Documented Rhai loop modes, seeded random/wiggle evaluation and owner/property/time/line errors are integrated into validated project compilation.")
assess("AE:61", "partial", "expr markers", "Expressions expose parameter time/frame variables and pre-expression value_at_time/loop helpers.", ["Neighboring key/marker query APIs and complete marker-aware expression helpers are absent."])
assess("AE:63", "partial", "expr transform", "Scalar interpolation helpers and independently expression-driven vector components exist.", ["Layer/comp/world coordinate conversion and parented transform hierarchies are missing."])
assess("AE:65", "partial", "expr params fx", "Typed numeric/color/vector native parameters can drive other properties through expression references.", ["User-defined expression-control objects including layer refs, checkbox/menu rig controls are not implemented."])
assess("AE:70-73", "partial", "text params compile", "Native Unicode shaping, automatic bidi, explicit fallback fonts, paragraph boxes/wrapping/alignment, tracking and line height are integrated.", ["Vertical text/direction overrides, mixed character style runs, baseline offsets, indentation/paragraph spacing and alternate composers are incomplete."])
assess("AE:74", "partial", "text params", "Source text content is editable while numeric layout/style parameters animate.", ["Keyframed/string-expression Source Text content changes are not supported."])
assess("AE:76-77", "partial", "text params", "Ordered character/word/line range animators control position, opacity and fill with fractional cluster-safe endpoints.", ["Per-character scale/rotation and complete percentage/falloff/selector shape controls are absent."])
assess("AE:78", "partial", "text", "Multiple ordered range animators can overlap on the same native text layer.", ["Explicit add/subtract/intersect selector combination modes are not implemented."])
assess("AE:79", "partial", "text expr", "Numeric selector/animator properties can use deterministic expressions.", ["Per-character expression/seed context and wiggly selectors are not implemented."])
assess("AE:82", "partial", "text expr params", "Numeric global/range text style leaves can be expression-driven.", ["Expression-produced text content and arbitrary substring style objects are not supported."])
assess("AE:84", "partial", "vector", "Editable rectangle/rounded rectangle, ellipse and quadratic/cubic path primitives exist.", ["Parametric polygon/star and the broader primitive collection are missing."])
assess("AE:85,87-88", "integrated", "vector generator compile params", "Native editable contours, solid/linear/radial gradient fills/strokes, fill rules, caps/joins/dashes and animated coordinates/paint are wired into normal generator clips.")
assess("AE:95", "partial", "vector params", "Path command control points are animatable, allowing compatible fixed-topology numeric path morphs.", ["First-vertex/correspondence editing, topology conversion and assisted anti-twist controls are absent."])
assess("AE:97-98", "partial", "vector generator", "An SVG path-data parser can produce editable native command geometry.", ["Full layered SVG document import, gradients/styles/transforms, Illustrator transfer and an agent-exposed SVG/paste importer are not implemented."])
assess("AE:101-104", "partial", "vector blend compile native", "Animated native paths can become adjacent alpha/luma mattes, with inversion and blur primitives.", ["A layer-attached multi-mask stack, mask modes, expansion, variable-width feather and per-effect attachments are missing."])
assess("AE:106", "partial", "blend compile", "The current adjacent matte source is consumed instead of composited.", ["Matte-source visibility cannot be independently enabled while reusing that source as a matte."])
assess("AE:107,116", "partial", "finishing native blend", "Chroma-key softness/despill and alpha-aware blur provide basic edge-processing primitives.", ["Temporal key cleanup/chatter suppression, matte choke and fine-detail edge decontamination are missing."])
assess("AE:115", "partial", "finishing fx compile", "An original native chroma-distance keyer with soft alpha and despill is integrated.", ["Uneven-screen/hair/motion-blur production acceptance and advanced screen/matte controls are missing.", "This is not Foundry Keylight and does not establish its algorithm/binary compatibility."], "Evaluate native keying against production output requirements; separately decide whether a licensed third-party adapter is needed.")
assess("AE:117", "partial", "finishing fx", "Native chroma and luma keyers exist with softness and optional luma inversion.", ["Difference/image-comparison keying is absent."])
assess("AE:151", "integrated", "fx params schema mcp", "The parameter registry lists native effects/docs/units/ranges, preserves instance IDs and returns structured validation diagnostics.", decision="Effect list/instance queries are the agent equivalent; a desktop effect-search panel is not required by this implementation.")
assess("AE:152", "partial", "fx ofx dependencies", "Native effect IDs/enable flags and unknown-effect errors exist; isolated OpenFX source is separate.", ["Installed plugin/version/dependency enumeration and normal timeline plugin enabling/disabling are not integrated."], "Define a supported plugin capability protocol and dependency report rather than assuming Adobe plugin ABI support.")
assess("AE:155-156", "partial", "finishing scopes fx", "Native primary exposure/contrast/saturation/lift-gamma-gain and technical scopes exist.", ["White-balance controls, RGB/secondary curves, selected-hue remapping and a complete Lumetri-like grading workflow are missing.", "No proprietary Lumetri equivalence is claimed."])
assess("AE:157", "partial", "native fx", "Gaussian and directional blur are native alpha-aware effects.", ["Box/radial blur are not implemented."])
assess("AE:159", "integrated", "native fx compile", "Native sharpen and unsharp-mask effects expose controlled contrast/blur parameters through the effect registry.")
assess("AE:163", "partial", "native layer3d fx", "Native drop shadow and planar 3D rotation exist.", ["Four-corner pin/perspective placement controls are absent."])
assess("AE:165", "partial", "generator vector compile", "Native solid/linear/radial gradients and editable geometric shape generators exist.", ["Grid/checkerboard and the broader generator family are missing."])
assess("AE:172", "partial", "native fx", "Native glow controls threshold/radius/strength on linear overrange pixels.", ["Edge/emboss/mosaic/paint stylization effects are absent."])
assess("AE:181", "partial", "analysis loudness perceive", "Audio reports expose block energy, integrated levels and peaks.", ["Agent-visible high-resolution waveform/envelope and onset queries are not implemented."], "Return numerical audio envelopes/onsets with exact sample/time mapping for agent synchronization.")
assess("AE:182-184", "partial", "audiofx audio retime", "Native compressor/gate/EQ, audio-only reverse/retiming and stereo mixing exist.", ["Distortion, delay/modulation/reverb, flexible channel routing and tone generation are missing from the broader audio family."])
assess("AE:186-187", "partial", "comp params fx", "Nested composition instances have local effects/transforms, and native parameter metadata is typed.", ["Exposed template controls and independent child-property overrides do not exist."])
assess("AE:192-193", "partial", "render proxy perceive main", "Offline renders, proxy drafts and contact-sheet/quality reports offer preview primitives; finals restore original media.", ["Frame/range/audio playback, adaptive preview resolution/cadence and interactive frame-accurate inspection are not exposed."])
assess("AE:194-195", "partial", "graph render", "Native RAM frame reuse and persistent lossless FFV1 chunk caches use dependency keys.", ["A compressed playback-frame cache, byte-budget UI/API and measured playback memory/quality comparison are absent."])
assess("AE:196,198", "partial", "graph compositor mcp", "Internal render APIs evaluate requested times and can return CPU pixel buffers with data windows.", ["A normal CLI/MCP exact-frame artifact, layer/channel/ROI/pixel query is not exposed."], "Ship direct frame and pixel/region inspection tools so agents can revise without exporting full movies.")
assess("AE:201", "partial", "render mcp", "Render reports contain aggregate/chunk timings and cache counters.", ["Per-layer/effect composition profiler timings and dominant-cost attribution are absent."], "Expose structured timing spans/cache state rather than duplicating a desktop profiler panel.")
assess("AE:202-204,206", "partial", "render deliver encode main", "One native job renders a full FFV1/PCM master and can derive an H.264/AAC review movie.", ["A preset/range render queue, arbitrary multiple output modules, alpha master, independent crop/field/color output transforms and complete bitrate presets are missing.", "H.264 depends on explicitly enabled runtime OpenH264."])
assess("AE:209", "integrated", "main render mcp", "Native headless CLI/MCP renders a composition timeline with output settings, progress, cancellation, structured reports and exit status.")
assess("AE:210", "partial", "render graph", "Local workers reuse completed content-addressed chunks and can resume interrupted local renders.", ["Distributed frame allocation, network workers and numbered image-sequence recovery are absent."])
assess("AE:212-213", "integrated", "edit project mcp schema main", "Typed project-mutating command arrays, dry runs, strict errors, discovery and reproducible journal ordering form the native agent automation surface.")
assess("AE:216", "partial", "compile decode text project", "Supported source/font/composition bytes participate in keys and refresh on recompilation.", ["PSD/Illustrator layered import and explicit external-editor source refresh workflows are absent."], "Define native file refresh/interchange behavior separately from Adobe editor integration.")
assess("AE:220", "partial", "mcp schema expr", "External AI clients can discover native project editing and documented expression syntax.", ["No embedded/cloud assistant service or in-app prompt workflow is implemented."], "Ferrocut's external agent API may meet this product goal; a hosted embedded assistant remains a separate scope decision.")
assess("AE:227", "partial", "params edit", "An explicit anchor parameter is settable and new clips can be built through typed operations.", ["A duplicate-layer operation that preserves transformed appearance while resetting anchor is incomplete."], "Expose reproducible anchor/duplicate semantics through commands; beta drag gestures are not required for an agent API.")
assess("AE:229", "partial", "project mcp", "Local retained snapshots and explicit branch checkout provide recovery primitives.", ["The complete interrupted-session recovery flow and account/Stock asset access are absent."], "Separate reliable local recovery from optional cloud/account asset integrations; this is beta watchlist scope.")


def audit_paths(row):
    family = row["family"].casefold()
    if "cloud" in row["class"] or row["class"] in {"ecosystem", "bundled"}:
        return ["dependencies", "mcp", "main"]
    if any(word in family for word in ("3d", "camera", "lighting", "stereo", "immersive")):
        return ["layer3d", "timeline", "compile"]
    if any(word in family for word in ("audio", "dialogue")):
        return ["audiofx", "audio", "params"]
    if any(word in family for word in ("text", "typography")):
        return ["text", "params", "generator"]
    if any(word in family for word in ("shape", "vector", "mask", "paint", "deformation", "roto", "track", "stabil", "repair", "removal")):
        return ["vector", "timeline", "compile"]
    if any(word in family for word in ("color", "hdr", "monitor", "scope")):
        return ["compositor", "finishing", "timeline"]
    if any(word in family for word in ("effect", "keying", "blur", "distortion", "perspective", "channel", "generation", "simulation", "noise", "grain", "stylization", "transition", "scaling")):
        return ["native", "finishing", "fx"]
    if any(word in family for word in ("ingest", "media", "proxy", "relink")):
        return ["decode", "probe", "timeline"]
    if any(word in family for word in ("render", "export", "delivery", "preview", "performance", "stream")):
        return ["main", "render", "encode"]
    if any(word in family for word in ("expression", "animation", "timing", "retime", "transform")):
        return ["expr", "keyframe", "params"]
    if any(word in family for word in ("transcript", "search", "analysis", "caption")):
        return ["index", "captions", "edit"]
    return ["timeline", "edit", "project", "mcp"]


PATHS["generator"] = "crates/ferrocut-engine/src/generator.rs"


def design_note(row):
    classification = row["class"]
    if "cloud" in classification:
        return "Choose an optional provider/account/job/provenance contract for this service behavior. No Adobe cloud API, credential, entitlement, paid generation or external publication is implemented or authorized by this ledger."
    if classification == "bundled":
        return "Choose a native output-quality equivalent or a separately licensed supported adapter. A bundled Adobe/third-party product name is not a requirement to copy its algorithm or claim binary compatibility."
    if "ecosystem" in classification:
        return "Define supported file/API/codec/hardware versions and licensing explicitly. Equivalent native behavior does not establish compatibility with Adobe applications, plugins or proprietary project formats."
    if "ui" in classification:
        return "Specify a structured agent query/edit/inspection equivalent. A desktop interaction need not be cloned, but its useful information or production semantics still need runtime evidence."
    if row["scope"] == "beta_watchlist":
        return "Beta watchlist only: decide adoption after stable behavior and dependency scope are clear; excluded from the stable baseline."
    return None


def seed_review(row):
    identifier = row["id"]
    review = fresh_review()
    if identifier in TRIAGE:
        status, path_keys, behavior, limitations, decision = TRIAGE[identifier]
        review.update(status=status, assessed_at="2026-10-08", limitations=limitations)
        for index, key in enumerate(path_keys):
            stage = "implemented" if status in {"implemented", "integrated"} and index == 0 else "audit"
            if status == "integrated" and index == len(path_keys) - 1:
                stage = "integrated"
            review["evidence"].append({"stage": stage, "path": PATHS[key], "locator": "", "claim": behavior})
        review["notes"] = ["Source audit 2026-10-08: implementation/integration state only; full requirement execution and creative acceptance remain unrecorded."]
        note = decision or design_note(row)
    elif identifier in BASELINE:
        status, evidence, limitations = BASELINE[identifier]
        review.update(status=status, assessed_at="2026-10-08", limitations=limitations)
        review["evidence"] = [{"stage": "audit", "path": path, "locator": locator, "claim": claim}
                              for path, locator, claim in evidence]
        review["notes"] = ["Conservative code audit only; test-source references are not fresh execution or creative-review claims."]
        note = design_note(row)
    else:
        review.update(status="missing", assessed_at="2026-10-08")
        review["limitations"] = [f"No supported model/handler/effect/provider adapter for the recorded capability was found in the audited source: {row['capability']}.",
                                 "The complete recorded agent acceptance criterion has no executable implementation evidence."]
        review["evidence"] = [{"stage": "audit", "path": PATHS[key], "locator": "",
                               "claim": f"Audited the current {row['family']} source/agent contract for {row['id']}; no corresponding supported runtime capability is exposed here."}
                              for key in audit_paths(row)]
        review["notes"] = ["Source audit 2026-10-08: explicit missing runtime capability at this checkpoint, not an estimate of engineering effort."]
        note = design_note(row)
    if note:
        review["needs_design"] = True
        review["notes"].append("Design decision: " + note)
    return review


def expected_document(root):
    rows, manifests = research(root)
    require(set(TRIAGE) | set(BASELINE) <= {row["id"] for row in rows}, "source triage refers to unknown research IDs")
    return {"schema_version": 1, "research_snapshot": "2026-10-08",
            "inventory_sources": manifests,
            "counts": {"total": len(rows), "stable": sum(row["scope"] == "stable" for row in rows),
                       "beta_watchlist": sum(row["scope"] == "beta_watchlist" for row in rows),
                       "by_product": {item[2]: item[1] for item in INVENTORIES}},
            "status_definitions": STATUSES,
            "capabilities": [dict(row, ferrocut=seed_review(row)) for row in rows]}


def local_file(root, name, context, allow_absolute=False):
    require(nonempty(name), f"{context}: file path required")
    path = Path(name)
    require(allow_absolute or not path.is_absolute(), f"{context}: expected repository-relative path")
    path = (root / path).resolve()
    require(allow_absolute or path.is_relative_to(root.resolve()), f"{context}: path escapes repository")
    require(path.is_file(), f"{context}: missing file {name}")
    return path


def check_shape(check, root, identifier):
    context = f"{identifier} check {check.get('id', '?')}"
    require(set(check) <= CHECK_KEYS, f"{context}: unknown fields {sorted(set(check)-CHECK_KEYS)}")
    require(re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_.-]*", check.get("id", "")) is not None, f"{context}: invalid check ID")
    argv = check.get("argv")
    require(isinstance(argv, list) and bool(argv) and all(nonempty(item) and "\0" not in item for item in argv), f"{context}: executable argv must be a nonempty string array")
    executable = Path(argv[0]).name
    require(executable not in SHELLS, f"{context}: shell command strings are not executable acceptance arrays")
    require(executable not in {"true", "false", "echo", "printf"}, f"{context}: trivial command cannot be an acceptance check")
    cwd = (root / check.get("cwd", ".")).resolve()
    require(cwd.is_relative_to(root.resolve()) and cwd.is_dir(), f"{context}: cwd must exist inside the repository")
    require(bool(shutil.which(argv[0])) or (cwd / argv[0]).is_file(), f"{context}: executable not found: {argv[0]}")
    environment = check.get("env", {})
    require(isinstance(environment, dict) and all(re.fullmatch(r"[A-Za-z_][A-Za-z_0-9]*", key) and isinstance(value, str) for key, value in environment.items()), f"{context}: invalid environment mapping")
    require(nonempty(check.get("covers")), f"{context}: criterion coverage explanation required")
    timeout = check.get("timeout_seconds", 900)
    require(type(timeout) is int and 1 <= timeout <= 14400, f"{context}: timeout must be 1..14400 seconds")


def execution_digest(row, check, root):
    evidence_files = {item["path"]: sha_file(local_file(root, item["path"], row["id"]))
                      for item in row["ferrocut"]["evidence"]}
    command = {key: value for key, value in check.items() if key != "result"}
    requirement = {key: value for key, value in row.items() if key != "ferrocut"}
    return canonical_digest({"requirement": requirement, "command": command, "evidence_files": evidence_files})


def validate_result(row, check, root):
    context = f"{row['id']} check {check['id']}"
    result = check.get("result")
    require(isinstance(result, dict), f"{context}: executed result required")
    require(result.get("executed") is True and type(result.get("exit_code")) is int and result["exit_code"] == 0, f"{context}: successful executed result required")
    require(nonempty(result.get("recorded_at")) and nonempty(result.get("revision")), f"{context}: execution time and revision required")
    require(result.get("input_digest") == execution_digest(row, check, root), f"{context}: command/research/source evidence changed after execution")
    path = local_file(root, result.get("log_path"), context, allow_absolute=True)
    require(result.get("log_sha256") == sha_file(path), f"{context}: execution log digest mismatch")
    with path.open() as stream:
        try:
            receipt = json.loads(stream.readline(), object_pairs_hook=no_duplicate_keys)
        except ValueError as error:
            raise Invalid(f"{context}: execution log receipt missing/invalid") from error
    expected_receipt = {"capability": row["id"], "argv": check["argv"],
                        "recorded_at": result["recorded_at"], "revision": result["revision"],
                        "input_digest": result["input_digest"]}
    require(receipt == expected_receipt, f"{context}: execution log receipt does not match the result/command")


def validate_review(review, root, context, production=False):
    require(isinstance(review, dict), f"{context}: review object required")
    require(review.get("approved") is True, f"{context}: approved inspection required")
    require(all(nonempty(review.get(key)) for key in ("reviewed_at", "reviewer", "findings")), f"{context}: inspection date/reviewer/findings required")
    path = local_file(root, review.get("artifact_path"), context, allow_absolute=True)
    require(review.get("artifact_sha256") == sha_file(path), f"{context}: artifact digest mismatch")
    if production:
        require(nonempty(review.get("brief")) and nonempty(review.get("agent_workflow")), f"{context}: production brief and executed agent-workflow check ID required")
        local_file(root, review.get("project_path"), context, allow_absolute=True)
    else:
        require(review.get("kind") in {"visual", "audio", "audiovisual"}, f"{context}: visual or listening inspection kind required")


def validate(document, root, require_execution=True):
    require(isinstance(document, dict), "ledger must be an object")
    expected = expected_document(root)
    require(type(document.get("schema_version")) is int, "schema_version must be an integer")
    require(set(document) == set(expected), f"unknown/missing ledger fields: {sorted(set(document)^set(expected))}")
    for key in expected.keys() - {"capabilities"}:
        require(document[key] == expected[key], f"stale or invalid {key}; compare inventories and regenerate")
    rows = document["capabilities"]
    require(isinstance(rows, list), "capabilities must be an array")
    identifiers = [row.get("id") for row in rows if isinstance(row, dict)]
    require(len(identifiers) == len(rows), "each capability must be an object")
    require(len(identifiers) == len(set(identifiers)), "duplicate capability IDs")
    expected_ids = [row["id"] for row in expected["capabilities"]]
    require(set(identifiers) == set(expected_ids), f"capability ID set differs: missing={sorted(set(expected_ids)-set(identifiers))}, unknown={sorted(set(identifiers)-set(expected_ids))}")
    require(identifiers == expected_ids, "capability order must match the inventories")
    for row, canonical in zip(rows, expected["capabilities"]):
        identifier = row["id"]
        require(set(row) == set(canonical), f"{identifier}: unknown/missing capability fields")
        for key in canonical.keys() - {"ferrocut"}:
            require(row[key] == canonical[key], f"{identifier}: research/source/acceptance field {key} differs from inventory")
        review = row["ferrocut"]
        require(isinstance(review, dict) and set(review) == FERROCUT_KEYS, f"{identifier}: unknown/missing Ferrocut review fields")
        status = review["status"]
        require(status in STATUSES, f"{identifier}: unknown status {status}")
        require(all(type(review[key]) is bool for key in ("acceptance_complete", "output_review_required", "needs_design")), f"{identifier}: review flags must be booleans")
        for key in ("evidence", "limitations", "acceptance_checks", "render_reviews", "production_reviews", "notes"):
            require(isinstance(review[key], list), f"{identifier}: {key} must be an array")
        require(all(nonempty(item) for item in review["limitations"] + review["notes"]), f"{identifier}: limitations/notes must be nonempty strings")
        if review["needs_design"]:
            require(any(item.startswith("Design decision: ") for item in review["notes"]), f"{identifier}: needs_design requires a scoped design decision")
        if status != "unreviewed":
            require(nonempty(review["assessed_at"]) and bool(review["evidence"]), f"{identifier}: assessed date and code evidence required")
        stages = set()
        for item in review["evidence"]:
            require(isinstance(item, dict) and set(item) == {"stage", "path", "locator", "claim"}, f"{identifier}: invalid evidence object")
            require(item["stage"] in STAGES and nonempty(item["claim"]), f"{identifier}: valid evidence stage/claim required")
            require(isinstance(item["locator"], str), f"{identifier}: evidence locator must be a string")
            path = local_file(root, item["path"], identifier)
            if item["locator"]:
                require(item["locator"] in path.read_text(errors="replace"), f"{identifier}: source locator no longer exists: {item['locator']}")
            stages.add(item["stage"])
        if status in {"partial", "missing"}:
            require(bool(review["limitations"]), f"{identifier}: partial/missing requires explicit limitations")
        if status in {"implemented", "integrated", "verified", "reviewed"}:
            require("implemented" in stages, f"{identifier}: implementation evidence required")
        if status in {"integrated", "verified", "reviewed"}:
            require("integrated" in stages, f"{identifier}: actual project/agent integration evidence required")
        checks = review["acceptance_checks"]
        require(all(isinstance(check, dict) for check in checks), f"{identifier}: acceptance checks must be objects")
        check_ids = [check.get("id") for check in checks]
        require(len(check_ids) == len(set(check_ids)), f"{identifier}: duplicate acceptance check IDs")
        for check in checks:
            check_shape(check, root, identifier)
        for rendered in review["render_reviews"]:
            validate_review(rendered, root, identifier + " render review")
        for production in review["production_reviews"]:
            validate_review(production, root, identifier + " production review", production=True)
            require(production["agent_workflow"] in check_ids, f"{identifier}: agent workflow must reference a registered acceptance check")
        if not review["output_review_required"]:
            require(nonempty(review["output_review_exemption"]), f"{identifier}: output-review exemption must explain why no visual/audio output applies")
        if status in {"verified", "reviewed"}:
            require(review["acceptance_complete"] is True and not review["limitations"], f"{identifier}: full acceptance must be complete without outstanding limitations")
            require("tested" in stages and bool(checks), f"{identifier}: tested evidence and executable acceptance checks required")
            if require_execution:
                for check in checks:
                    validate_result(row, check, root)
            if review["output_review_required"]:
                require("rendered" in stages and bool(review["render_reviews"]), f"{identifier}: rendered evidence and inspected artifact required")
        if status == "reviewed":
            require("reviewed" in stages and bool(review["production_reviews"]), f"{identifier}: agent production and creative-review evidence required")
    return document


def generate(root):
    document = expected_document(root)
    path = root / LEDGER
    if path.exists():
        previous = read_json(path)
        previous_rows = previous.get("capabilities", [])
        identifiers = [row["id"] for row in previous_rows]
        require(len(identifiers) == len(set(identifiers)), "existing ledger has duplicate capability IDs")
        reviews = {row["id"]: row["ferrocut"] for row in previous_rows}
        expected_ids = {row["id"] for row in document["capabilities"]}
        require(set(reviews) == expected_ids, f"refusing to drop or silently recreate existing IDs: missing={sorted(expected_ids-set(reviews))}, unknown={sorted(set(reviews)-expected_ids)}")
        for row in document["capabilities"]:
            row["ferrocut"] = reviews[row["id"]]
            row["ferrocut"].setdefault("needs_design", False)
    validate(document, root)
    write_json(path, document)
    print(f"Generated {LEDGER}: {document['counts']['total']} requirements; existing per-ID reviews preserved.")


def triage(root):
    document = read_json(root / LEDGER)
    expected = expected_document(root)
    expected_ids = {row["id"] for row in expected["capabilities"]}
    require(set(TRIAGE) | set(BASELINE) <= expected_ids, "source triage refers to unknown research IDs")
    replaced = 0
    for row in document["capabilities"]:
        review = row["ferrocut"]
        review.setdefault("needs_design", False)
        if review["status"] == "unreviewed":
            row["ferrocut"] = seed_review(row)
            replaced += 1
    validate(document, root)
    write_json(root / LEDGER, document)
    print(f"Source-triaged {replaced} unreviewed requirements; all existing assessments preserved.")


def summary(document):
    print(f"Research: {document['counts']['total']} requirements ({document['counts']['stable']} stable, {document['counts']['beta_watchlist']} beta watchlist).")
    for scope in ("stable", "beta_watchlist"):
        rows = [row for row in document["capabilities"] if row["scope"] == scope]
        totals = {status: sum(row["ferrocut"]["status"] == status for row in rows) for status in STATUSES}
        print(scope + ": " + ", ".join(f"{status}={count}" for status, count in totals.items() if count))
    print(f"Needs a scoped design/dependency decision: {sum(row['ferrocut']['needs_design'] for row in document['capabilities'])} requirements.")
    print("Counts describe evidence states, not percent parity or engineering effort.")


def run_checks(document, root, identifier, selected, log_directory, *, output_path=None, quiet=False):
    validate(document, root, require_execution=False)
    row = next((row for row in document["capabilities"] if row["id"] == identifier), None)
    require(row is not None, f"unknown capability ID {identifier}")
    checks = [check for check in row["ferrocut"]["acceptance_checks"] if selected is None or check["id"] == selected]
    require(bool(checks), f"{identifier}: no matching registered executable acceptance checks")
    log_directory = log_directory.resolve()
    log_directory.mkdir(parents=True, exist_ok=True)
    revision = subprocess.run(["git", "rev-parse", "HEAD"], cwd=root, text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, check=True).stdout.strip()
    failed = False
    for check in checks:
        stamp = datetime.now(timezone.utc)
        log_path = log_directory / f"{identifier}-{check['id']}-{stamp.strftime('%Y%m%dT%H%M%S%fZ')}.log"
        digest = execution_digest(row, check, root)
        with log_path.open("w") as output:
            output.write(json.dumps({"capability": identifier, "argv": check["argv"], "recorded_at": stamp.isoformat(), "revision": revision, "input_digest": digest}) + "\n")
            output.flush()
            try:
                result = subprocess.run(check["argv"], cwd=root / check.get("cwd", "."),
                                        env=dict(os.environ, **check.get("env", {})),
                                        stdout=output, stderr=subprocess.STDOUT, shell=False,
                                        timeout=check.get("timeout_seconds", 900), check=False)
                exit_code = result.returncode
            except subprocess.TimeoutExpired:
                output.write("\nAcceptance command timed out.\n")
                exit_code = 124
        check["result"] = {"executed": True, "exit_code": exit_code, "recorded_at": stamp.isoformat(),
                           "revision": revision, "input_digest": digest, "log_path": str(log_path),
                           "log_sha256": sha_file(log_path)}
        failed |= exit_code != 0
        if not quiet:
            print(f"{identifier}/{check['id']}: exit {exit_code}; {log_path}")
    if failed and row["ferrocut"]["status"] in {"verified", "reviewed"}:
        row["ferrocut"]["status"] = "integrated"
        row["ferrocut"]["acceptance_complete"] = False
        row["ferrocut"]["notes"].append("Latest acceptance execution failed; previous completion status revoked.")
    write_json(output_path or root / LEDGER, document)
    if not quiet:
        print("Execution recorded; capability status was not promoted.")
    return 1 if failed else 0


def self_test(root):
    baseline = expected_document(root)
    validate(baseline, root)
    checks = []

    def rejects(label, mutate):
        candidate = copy.deepcopy(baseline)
        mutate(candidate)
        try:
            validate(candidate, root)
        except (Invalid, TypeError):
            checks.append(label)
        else:
            raise Invalid(f"self-test failed to reject {label}")

    rejects("duplicate ID", lambda doc: doc["capabilities"].append(copy.deepcopy(doc["capabilities"][0])))
    rejects("missing ID", lambda doc: doc["capabilities"].pop())
    rejects("unknown ID", lambda doc: doc["capabilities"][0].update(id="PP-999"))
    rejects("changed source URL", lambda doc: doc["capabilities"][0]["sources"][0].update(url="https://example.com/unsupported"))
    rejects("changed acceptance", lambda doc: doc["capabilities"][0].update(proposed_acceptance="done"))
    rejects("false count", lambda doc: doc["counts"].update(total=457))
    rejects("stale inventory digest", lambda doc: doc["inventory_sources"][0].update(sha256="0" * 64))
    rejects("unknown status", lambda doc: doc["capabilities"][0]["ferrocut"].update(status="complete"))
    rejects("verified without evidence", lambda doc: doc["capabilities"][0]["ferrocut"].update(status="verified", assessed_at="2026-10-08"))
    rejects("partial without limits", lambda doc: doc["capabilities"][4]["ferrocut"].update(limitations=[]))
    rejects("design flag without decision", lambda doc: doc["capabilities"][0]["ferrocut"].update(needs_design=True, notes=[]))
    rejects("unsupported percent parity", lambda doc: doc.update(percent_parity=100))
    rejects("shell acceptance string", lambda doc: doc["capabilities"][0]["ferrocut"]["acceptance_checks"].append({"id": "bad", "argv": "cargo test", "covers": "criterion"}))

    # A fully formed synthetic review checks enforcement beyond the status name.
    # Temporary files are validator fixtures, never real capability evidence.
    with tempfile.TemporaryDirectory(prefix="ferrocut-ledger-self-test-") as temporary:
        artifact = Path(temporary) / "fixture-artifact.txt"
        artifact.write_text("Synthetic validator fixture; not a rendered production.\n")
        log = Path(temporary) / "fixture-execution.log"
        complete = copy.deepcopy(baseline)
        row = complete["capabilities"][0]
        review = row["ferrocut"]
        evidence_path = "crates/ferrocut-engine/tests/nest.rs"
        review.update(status="verified", assessed_at="2026-10-08", acceptance_complete=True, limitations=[],
                      evidence=[{"stage": stage, "path": evidence_path, "locator": "", "claim": "Synthetic schema validation fixture."}
                                for stage in ("implemented", "integrated", "tested", "rendered")])
        check = {"id": "fixture", "argv": [sys.executable, "scripts/parity-inventory.py", "summary"],
                 "cwd": ".", "covers": "Synthetic full-criterion schema fixture."}
        review["acceptance_checks"] = [check]
        check["result"] = {"executed": True, "exit_code": 0, "recorded_at": "2026-10-08T12:00:00+00:00",
                           "revision": "synthetic-validator-fixture", "input_digest": execution_digest(row, check, root),
                           "log_path": str(log)}
        log.write_text(json.dumps({"capability": row["id"], "argv": check["argv"],
                                   **{key: check["result"][key] for key in ("recorded_at", "revision", "input_digest")}}) + "\nSynthetic validator fixture.\n")
        check["result"]["log_sha256"] = sha_file(log)
        review["render_reviews"] = [{"kind": "visual", "approved": True, "reviewed_at": "2026-10-08",
                                     "reviewer": "synthetic validator fixture", "findings": "Tests schema, not creative quality.",
                                     "artifact_path": str(artifact), "artifact_sha256": sha_file(artifact)}]
        validate(complete, root)

        def rejects_complete(label, mutate):
            candidate = copy.deepcopy(complete)
            mutate(candidate["capabilities"][0]["ferrocut"])
            try:
                validate(candidate, root)
            except Invalid:
                checks.append(label)
            else:
                raise Invalid(f"self-test failed to reject {label}")

        rejects_complete("verified without executable acceptance", lambda value: value.update(acceptance_checks=[]))
        rejects_complete("verified without successful execution", lambda value: value["acceptance_checks"][0]["result"].update(exit_code=1))
        rejects_complete("verified with stale command", lambda value: value["acceptance_checks"][0]["argv"].append("unexpected"))
        rejects_complete("verified with modified log", lambda value: value["acceptance_checks"][0]["result"].update(log_sha256="0" * 64))
        rejects_complete("verified without render inspection", lambda value: value.update(render_reviews=[]))
        rejects_complete("verified with modified artifact", lambda value: value["render_reviews"][0].update(artifact_sha256="0" * 64))
        rejects_complete("verified with remaining limitations", lambda value: value.update(limitations=["Still missing behavior."]))
        rejects_complete("reviewed without production workflow", lambda value: value.update(status="reviewed"))

        fixture = Path(temporary) / "acceptance-fixture.py"
        fixture.write_text("import sys\nprint('Runner exit-code fixture')\nraise SystemExit(int(sys.argv[1]))\n")
        runner_document = copy.deepcopy(complete)
        runner_row = runner_document["capabilities"][0]
        runner_review = runner_row["ferrocut"]
        runner_check = runner_review["acceptance_checks"][0]
        runner_check["argv"] = [sys.executable, str(fixture), "0"]
        runner_review.update(status="integrated", acceptance_complete=False)
        output_path = Path(temporary) / "fixture-ledger.json"
        require(run_checks(runner_document, root, runner_row["id"], None, Path(temporary), output_path=output_path, quiet=True) == 0, "runner must record successful child execution")
        require(runner_review["status"] == "integrated", "successful execution must not promote status")
        runner_review.update(status="verified", acceptance_complete=True)
        validate(runner_document, root)
        runner_check["argv"][-1] = "1"
        require(run_checks(runner_document, root, runner_row["id"], None, Path(temporary), output_path=output_path, quiet=True) == 1, "runner must report failed child execution")
        require(runner_review["status"] == "integrated" and not runner_review["acceptance_complete"], "failed execution must revoke prior completion")
        require(read_json(output_path)["capabilities"][0]["ferrocut"]["acceptance_checks"][0]["result"]["exit_code"] == 1, "failed execution must persist its receipt")
        checks.extend(["actual success does not promote", "actual failure revokes prior completion"])
    try:
        json.loads('{"status":"unreviewed","status":"reviewed"}', object_pairs_hook=no_duplicate_keys)
    except Invalid:
        checks.append("duplicate JSON keys")
    else:
        raise Invalid("self-test failed to reject duplicate JSON keys")
    print(f"Self-test passed: canonical ledger, complete evidence fixture, and {len(checks)} evidence/research/runner cases.")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=("generate", "triage", "check", "summary", "run", "self-test"))
    parser.add_argument("id", nargs="?")
    parser.add_argument("--check", dest="check_id")
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--log-dir", type=Path, default=Path("/tmp/ferrocut-capability-checks"))
    args = parser.parse_args()
    root = args.root.resolve()
    try:
        if args.command == "generate":
            generate(root)
        elif args.command == "triage":
            triage(root)
        elif args.command == "self-test":
            self_test(root)
        elif args.command == "run":
            require(nonempty(args.id), "run requires a capability ID")
            return run_checks(read_json(root / LEDGER), root, args.id, args.check_id, args.log_dir)
        else:
            document = validate(read_json(root / LEDGER), root)
            if args.command == "summary":
                summary(document)
            else:
                print(f"Ledger valid: {document['counts']['total']} exact IDs, canonical research, and scoped evidence.")
        return 0
    except (Invalid, OSError, ValueError, TypeError, KeyError, subprocess.CalledProcessError) as error:
        print(f"parity-inventory: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
