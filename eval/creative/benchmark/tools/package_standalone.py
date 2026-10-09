"""Make a standalone, portable copy of a Ferrocut project directory.

usage: package_standalone.py PROJECT_DIR OUT_DIR [--timeline project.json]

1. Read-only preflight on PROJECT_DIR (nothing is written if it fails, exit 2):
   - walks the timeline and every nested comp it reaches;
   - collects every path reference: `source` (media and comps), `font`, and each entry of `fallback_fonts`;
   - refuses absolute paths, missing files, comps outside the directory, and fonts (incl. fallbacks) outside it
     (fonts have no relink op; copy them into the project first);
   - plans one destination per distinct external media file under media/. Two different files with the same
     name get distinct destinations (`<stem>-<sha8><ext>`). Identical bytes share one copy.
2. Copies PROJECT_DIR (except drafts/, delivery/, stills/, work/, scratch/, survey/), journal and .ferrocut/
   included, copies the planned media, and re-points clips with journaled `relink` ops (`ferrocut edit`, per
   timeline/comp, undoable).
3. Verifies again that every reference resolves inside OUT_DIR, and that each relinked file's bytes equal its
   origin's (sha256). Writes PACKAGE.md and MANIFEST.sha256.

It never edits PROJECT_DIR. Relocation equivalence is NOT shown by chunk keys (font paths enter text keys,
FINDINGS P4). Compare rendered pixels of the same frames from the original and the package.
"""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("src", type=Path)
ap.add_argument("out", type=Path)
ap.add_argument("--timeline", default="project.json")
a = ap.parse_args()
src, out = a.src.resolve(), a.out.resolve()
SKIP = {"drafts", "delivery", "stills", "work", "scratch", "survey"}
MEDIA_EXT = (".mkv", ".mp4", ".mov", ".webm", ".wav", ".flac", ".mp3", ".aac", ".m4a", ".png", ".jpg", ".jpeg",
             ".tif", ".tiff", ".exr")


def die(msg):
    print(msg, file=sys.stderr)
    sys.exit(2)


def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def refs(obj):
    """(kind, path) for every source, font and fallback_fonts entry, at any depth."""
    if isinstance(obj, dict):
        for k, v in obj.items():
            if k in ("source", "font") and isinstance(v, str):
                yield k, v
            elif k == "fallback_fonts" and isinstance(v, list):
                for x in v:
                    if isinstance(x, str):
                        yield "fallback_font", x
                    else:
                        yield from refs(x)
            else:
                yield from refs(v)
    elif isinstance(obj, list):
        for v in obj:
            yield from refs(v)


def inside(p: Path, root: Path):
    return str(p).startswith(str(root) + os.sep)


def walk(root: Path, first: Path):
    """[(timeline path, [(kind, ref, resolved)])] for the timeline and every nested comp it reaches."""
    seen, todo, res = set(), [first], []
    while todo:
        t = todo.pop()
        if t in seen:
            continue
        seen.add(t)
        items = []
        for k, v in refs(json.load(open(t))):
            if os.path.isabs(v):
                die(f"{t}: absolute {k} path {v}")
            r = (t.parent / v).resolve()
            items.append((k, v, r))
            if k == "source" and v.endswith(".json"):
                if not inside(r, root):
                    die(f"{t}: comp {v} is outside {root}")
                todo.append(r)
        res.append((t, items))
    return res


# ---- 1. read-only preflight on the source --------------------------------------------------------
if out.exists():
    die(f"{out} exists; choose a new directory")
if out == src or inside(out, src):
    die(f"{out} is inside the project {src}; the copy would include itself. Choose a directory outside it")
if inside(src, out):
    die(f"the project {src} is inside {out}; choose a separate output directory")
tl0 = (src / a.timeline).resolve()
if not inside(tl0, src) or not tl0.is_file():
    die(f"--timeline {a.timeline} must be a file inside {src}")
plan = walk(src, tl0)
dest_of = {}           # resolved origin -> media/<name>
taken = {}             # media/<name> -> origin sha256
for t, items in plan:
    for k, v, r in items:
        if not r.exists():
            die(f"{t}: {k} {v} does not exist")
        if k in ("font", "fallback_font") and not inside(r, src):
            die(f"{t}: {k} {v} is outside the project; copy it into the project and point the clip there first")
        if k == "source" and v.lower().endswith(MEDIA_EXT) and not inside(r, src) and r not in dest_of:
            h = sha(r)
            name = r.name
            if name in taken and taken[name] != h:
                name = f"{r.stem}-{h[:8]}{r.suffix}"
            if name in taken and taken[name] != h:
                die(f"cannot allocate a unique destination for {r}")
            taken[name] = h
            dest_of[r] = name

# ---- 2. copy and relink ----------------------------------------------------------------------------
shutil.copytree(src, out, ignore=lambda d, names: [n for n in names if Path(d) == src and n in SKIP])
if dest_of:
    (out / "media").mkdir(exist_ok=True)
for origin, name in dest_of.items():
    if not (out / "media" / name).exists():
        shutil.copy2(origin, out / "media" / name)
relocated = []
for t, items in plan:
    tout = out / t.relative_to(src)
    ops, done = [], set()
    for k, v, r in items:
        if r in dest_of and v not in done:
            new = os.path.relpath(out / "media" / dest_of[r], tout.parent)
            ops.append({"op": "relink", "from": v, "to": new})
            done.add(v)
            relocated.append({"timeline": str(tout.relative_to(out)), "from": v, "to": new, "origin": str(r),
                              "sha256": taken[dest_of[r]]})
    if ops:
        opsf = out / "authoring" / f"package-relink-{tout.stem}.ops.json"
        opsf.parent.mkdir(exist_ok=True)
        opsf.write_text(json.dumps(ops, indent=1) + "\n")
        r = subprocess.run(["ferrocut", "edit", str(tout), str(opsf), "--json"], capture_output=True, text=True)
        if r.returncode:
            die(f"relink failed for {tout}:\n{r.stderr}")

# ---- 3. verify the package -------------------------------------------------------------------------
bad = []
for t, items in walk(out, out / tl0.relative_to(src)):
    for k, v, r in items:
        if not inside(r, out) or not r.exists():
            bad.append(f"{t.relative_to(out)}: {k} {v}")
for rel in relocated:
    p = (out / rel["timeline"]).parent / rel["to"]
    if sha(p) != rel["sha256"]:
        bad.append(f"{rel['timeline']}: {rel['to']} bytes differ from {rel['origin']}")
if bad:
    die("package verification failed:\n  " + "\n  ".join(bad))

lines = [f"# Standalone package of {src.name}", "",
         f"Made from `{src}` by tools/package_standalone.py. Every media, comp, font and fallback-font reference "
         "resolves inside this directory. External media were copied to `media/` (distinct files with the same "
         "name get distinct names) and re-pointed with journaled `relink` ops (`ferrocut log`). Each copy's "
         "sha256 equals its origin's.", "", "| Timeline | From | To | sha256 |", "|---|---|---|---|"]
lines += [f"| {r['timeline']} | `{r['from']}` | `{r['to']}` | {r['sha256'][:16]}… |" for r in relocated]
lines += ["", "Licenses to carry with this package: footage from Sintel / Tears of Steel is CC-BY 3.0, "
          "Blender Foundation (attribution as in the film's end card; Tears of Steel picture only, its soundtrack "
          "is CC-BY-ND and not included). Fonts: see the OFL file in assets/fonts/. Other assets: SOURCES.md.",
          "", "Equivalence to the original: compare rendered pixels of the same frames (not chunk keys)."]
(out / "PACKAGE.md").write_text("\n".join(lines) + "\n")
with open(out / "MANIFEST.sha256", "w") as m:
    for p in sorted(out.rglob("*")):
        if p.is_file() and p.name != "MANIFEST.sha256" and ".ferrocut" not in p.parts:
            m.write(f"{sha(p)}  {p.relative_to(out)}\n")
print(f"packaged {out}: {len(relocated)} relinks, {len(set(dest_of.values()))} media files")
