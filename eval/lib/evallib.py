#!/usr/bin/env python3
"""Ferrocut in-house eval: workdir setup, reference solutions (through
ferrocut-mcp, like an agent), and the grader.

  evallib.py setup     <taskdir> <workdir>
  evallib.py reference <taskdir> <workdir>      # apply reference.json via MCP
  evallib.py grade     <taskdir> <workdir> <result.json>

Standard library only. Rationals are compared exactly (fractions.Fraction).
"""
import copy
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
from fractions import Fraction

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.environ.get("FERROCUT_BIN_DIR", os.path.join(REPO, "target", "release"))
FERROCUT = os.path.join(BIN, "ferrocut")
MCP = os.path.join(BIN, "ferrocut-mcp")
FFLIB = os.path.join(REPO, "third_party", "ffmpeg-lgpl", "lib")
FFMPEG = os.path.join(REPO, "third_party", "ffmpeg-lgpl", "bin", "ffmpeg")
CLIPS = os.path.join(REPO, "eval", "media", "clips")


def env():
    e = dict(os.environ)
    e["LD_LIBRARY_PATH"] = FFLIB + (":" + e["LD_LIBRARY_PATH"] if e.get("LD_LIBRARY_PATH") else "")
    return e


def R(x):
    return Fraction(str(x))


def load(p):
    with open(p) as f:
        return json.load(f)


# ---------------------------------------------------------------- setup
def setup(taskdir, workdir):
    task = load(os.path.join(taskdir, "task.json"))
    os.makedirs(os.path.join(workdir, "media"), exist_ok=True)
    for c in task["clips"]:
        src = os.path.join(CLIPS, c + ".mkv")
        if not os.path.exists(src):
            sys.exit(f"missing {src}: run eval/fetch-media.sh")
        # A copy, not a symlink: the MCP project root rejects symlinks that
        # point outside it.
        shutil.copyfile(src, os.path.join(workdir, "media", c + ".mkv"))
    shutil.copyfile(os.path.join(taskdir, "start.json"), os.path.join(workdir, "timeline.json"))
    shutil.copyfile(os.path.join(taskdir, "brief.md"), os.path.join(workdir, "BRIEF.md"))
    with open(os.path.join(workdir, ".start-timeline.json"), "w") as f:
        json.dump(load(os.path.join(taskdir, "start.json")), f)


# ---------------------------------------------------------------- MCP client
class Mcp:
    """Newline-delimited JSON-RPC over stdio (MCP stdio transport)."""

    def __init__(self, root):
        self.p = subprocess.Popen([MCP, "--root", root], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  env=env(), text=True, cwd=root)
        self.n = 0
        self.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                    "clientInfo": {"name": "ferrocut-eval", "version": "1"}})
        self.notify("notifications/initialized", {})

    def notify(self, method, params):
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": method, "params": params}) + "\n")
        self.p.stdin.flush()

    def request(self, method, params):
        self.n += 1
        rid = self.n
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params}) + "\n")
        self.p.stdin.flush()
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError("ferrocut-mcp exited")
            m = json.loads(line)
            if m.get("id") == rid:
                if "error" in m:
                    raise RuntimeError(f"{method}: {m['error']}")
                return m["result"]

    def call(self, tool, args):
        r = self.request("tools/call", {"name": tool, "arguments": args})
        out = r.get("structuredContent")
        if out is None:
            out = json.loads(r["content"][0]["text"])
        if r.get("isError"):
            raise RuntimeError(f"{tool}: {out}")
        return out

    def close(self):
        self.p.stdin.close()
        self.p.wait(timeout=30)


def pointer_parts(path):
    return [p.replace("~1", "/").replace("~0", "~") for p in path.lstrip("/").split("/")]


def apply_patch(doc, ops):
    """RFC 6902 add/replace/remove (the subset references use)."""
    doc = copy.deepcopy(doc)
    for op in ops:
        *parents, last = pointer_parts(op["path"])
        node = doc
        for p in parents:
            node = node[int(p)] if isinstance(node, list) else node[p]
        if isinstance(node, list):
            i = len(node) if last == "-" else int(last)
            if op["op"] == "add":
                node.insert(i, op["value"])
            elif op["op"] == "replace":
                node[i] = op["value"]
            else:
                del node[i]
        else:
            if op["op"] in ("add", "replace"):
                node[last] = op["value"]
            else:
                del node[last]
    return doc


def lookup(bound, ref):
    """`name.key.0.key` in the results bound by earlier reference steps."""
    name, *path = ref.split(".")
    v = bound[name]
    for p in path:
        v = v[int(p)] if isinstance(v, list) else v[p]
    return v


def substitute(v, bound):
    """Replace every string that is exactly `{{ref}}` with the bound value."""
    if isinstance(v, str):
        m = re.fullmatch(r"\{\{\s*([\w.]+)\s*\}\}", v)
        return lookup(bound, m.group(1)) if m else v
    if isinstance(v, list):
        return [substitute(x, bound) for x in v]
    if isinstance(v, dict):
        return {k: substitute(x, bound) for k, x in v.items()}
    return v


def reference(taskdir, workdir):
    """Run reference.json: `{"mcp": tool, "args": {...}, "bind": name}` steps
    (later args may use `"{{name.path}}"` to reuse a bound result) and
    `{"patch": [...]}` JSON patches of timeline.json."""
    steps = load(os.path.join(taskdir, "reference.json"))
    mcp = Mcp(workdir)
    bound = {}
    try:
        for s in steps:
            if "mcp" in s:
                r = mcp.call(s["mcp"], substitute(s["args"], bound))
                if "bind" in s:
                    bound[s["bind"]] = r
                print(f"  mcp {s['mcp']}: ok" + (f" ({r.get('final_blake3', '')[:16]})" if s["mcp"] == "render" else ""))
            else:
                p = os.path.join(workdir, "timeline.json")
                doc = apply_patch(load(p), s["patch"])
                with open(p, "w") as f:
                    json.dump(doc, f, indent=2)
                print(f"  patch: {len(s['patch'])} op(s)")
    finally:
        mcp.close()


# ---------------------------------------------------------------- grader
def norm_src(s):
    s = os.path.normpath(s)
    return s[2:] if s.startswith("./") else s


def opacity_at(clip, t):
    """Clip-local opacity at `t` (piecewise linear between keyframes: exact at
    keyframes, an approximation of eased curves in between)."""
    op = clip.get("opacity", 1)
    if not isinstance(op, dict):
        return float(R(op))
    ks = sorted(((R(k["t"]), float(R(k["v"]))) for k in op["keyframes"]), key=lambda k: k[0])
    if t <= ks[0][0]:
        return ks[0][1]
    for (t0, v0), (t1, v1) in zip(ks, ks[1:]):
        if t <= t1:
            return v0 + (v1 - v0) * float((t - t0) / (t1 - t0)) if t1 > t0 else v1
    return ks[-1][1]


def get_path(doc, path):
    node = doc
    for p in path.split("."):
        if not isinstance(node, dict) or p not in node:
            return None
        node = node[p]
    return node


def in_range(v, spec):
    lo = R(spec["min"]) if "min" in spec else None
    hi = R(spec["max"]) if "max" in spec else None
    return (lo is None or v >= lo) and (hi is None or v <= hi)


def luma(path, frame):
    vf = f"select=eq(n\\,{frame}),signalstats,metadata=print:key=lavfi.signalstats.YAVG:file=-"
    out = subprocess.run([FFMPEG, "-nostdin", "-v", "error", "-i", path, "-an", "-vf", vf, "-frames:v", "1",
                          "-f", "null", "-"], capture_output=True, text=True, env=env()).stdout
    m = re.search(r"YAVG=([0-9.]+)", out)
    return float(m.group(1)) if m else None


def grade(taskdir, workdir, result_path):
    task = load(os.path.join(taskdir, "task.json"))
    exp = task["expect"]
    checks = []

    def check(name, ok, detail=""):
        checks.append({"name": name, "pass": bool(ok), "detail": detail})

    tl_path = os.path.join(workdir, "timeline.json")
    try:
        tl = load(tl_path)
    except Exception as e:  # noqa: BLE001
        check("timeline.json parses", False, str(e))
        tl = None
    if tl is not None:
        tracks = {t.get("name", f"#{i}"): t for i, t in enumerate(tl.get("tracks", []))}
        clips_by_src = {}
        for t in tl.get("tracks", []):
            for c in t.get("clips", []):
                clips_by_src.setdefault(norm_src(c.get("source", "")), c)
        for name, want in exp.get("tracks", {}).items():
            got = sorted(tracks.get(name, {}).get("clips", []), key=lambda c: R(c.get("start", 0)))
            check(f"{name}: {len(want)} clips", len(got) == len(want),
                  f"got {[norm_src(c.get('source', '')) for c in got]}")
            for i, (w, g) in enumerate(zip(want, got)):
                bad = []
                if norm_src(g.get("source", "")) != w["source"]:
                    bad.append(f"source {g.get('source')} != {w['source']}")
                for k in ("start", "duration", "source_in"):
                    if k in w and R(g.get(k, 0)) != R(w[k]):
                        bad.append(f"{k} {g.get(k, 0)} != {w[k]}")
                if "end" in w and R(g.get("start", 0)) + R(g.get("duration", 0)) != R(w["end"]):
                    bad.append(f"end {R(g.get('start', 0)) + R(g.get('duration', 0))} != {w['end']}")
                if "transition_in" in w:
                    ti = g.get("transition_in") or {}
                    if ti.get("kind") != w["transition_in"]["kind"] or R(ti.get("duration", 0)) != R(w["transition_in"]["duration"]):
                        bad.append(f"transition_in {ti} != {w['transition_in']}")
                check(f"{name}[{i}] {w['source']}", not bad, "; ".join(bad))
        if "duration" in exp:
            ends = [R(c.get("start", 0)) + R(c.get("duration", 0))
                    for t in tl.get("tracks", []) + tl.get("audio_tracks", []) for c in t.get("clips", [])]
            d = max(ends) if ends else Fraction(0)
            check(f"duration {exp['duration']} s", d == R(exp["duration"]), f"got {d}")
        for a in exp.get("audio_regions", []):
            c = clips_by_src.get(a["source"])
            if c is None:
                check(f"audio region {a['source']}", False, "clip missing")
                continue
            au = c.get("audio") or {}
            s = R(c.get("start", 0)) + R(au.get("in_offset", 0))
            e = R(c.get("start", 0)) + R(c.get("duration", 0)) + R(au.get("out_offset", 0))
            check(f"audio of {a['source']} plays [{a['start']}, {a['end']})",
                  s == R(a["start"]) and e == R(a["end"]), f"got [{s}, {e})")
        for w in exp.get("source_windows", []):
            # A clip whose source range must start inside [in_min, in_max] and
            # end inside [out_min, out_max] (e.g. "keep this line, nothing else").
            got = sorted(tracks.get(w["track"], {}).get("clips", []), key=lambda c: R(c.get("start", 0)))
            i = w.get("index", 0)
            if i >= len(got):
                check(f"{w['track']}[{i}] source window", False, "clip missing")
                continue
            c = got[i]
            a = R(c.get("source_in", 0))
            b = a + R(c.get("duration", 0))
            ok = (norm_src(c.get("source", "")) == w["source"]
                  and R(w["in_min"]) <= a <= R(w["in_max"]) and R(w["out_min"]) <= b <= R(w["out_max"]))
            check(f"{w['track']}[{i}] keeps {w['source']} [{w['in_min']}..{w['in_max']}, {w['out_min']}..{w['out_max']}]"
                  + (f" ({w['why']})" if "why" in w else ""), ok,
                  f"got {norm_src(c.get('source', ''))} [{float(a):.3f}, {float(b):.3f}]")
        for f in exp.get("fields", []):
            v = get_path(tl, f["path"])
            ok = v is not None and in_range(R(v), f)
            check(f"{f['path']} in [{f.get('min', '-inf')}, {f.get('max', 'inf')}]", ok, f"got {v}")
        for o in exp.get("opacity", []):
            c = clips_by_src.get(o["source"])
            if c is None:
                check(f"opacity {o['source']}", False, "clip missing")
                continue
            t = R(o["at"]) if "at" in o else R(c.get("duration", 0)) + R(o["at_end"])
            v = opacity_at(c, t)
            where = f"t={o['at']}" if "at" in o else f"end{o['at_end']}"
            check(f"opacity of {o['source']} at {where} in [{o.get('min', 0)}, {o.get('max', 1)}]",
                  in_range(R(round(v, 6)), o), f"got {v:.3f}")
        start = load(os.path.join(workdir, ".start-timeline.json"))
        so, go = start.get("output", {}), tl.get("output", {})
        same = all(k in go and (R(go[k]) == R(v) if k == "fps" else go[k] == v) for k, v in so.items())
        check("output settings unchanged", same, f"got {go}")

    # ---- render: exists, matches the final timeline, expected frames
    out = os.path.join(workdir, "out.mkv")
    rep_path = os.path.join(workdir, "out.report.json")
    rexp = exp.get("render", {})
    have = os.path.exists(out)
    check("out.mkv rendered", have)
    regrade = os.path.join(workdir, ".grade")
    if have and tl is not None:
        os.makedirs(regrade, exist_ok=True)
        cpu = []
        if os.path.exists(rep_path) and "Cpu" in str(load(rep_path).get("adapter", "")):
            cpu = ["--cpu"]
        cache = os.path.join(workdir, ".ferrocut-cache")
        rr = subprocess.run([FERROCUT, "render", tl_path, "-o", os.path.join(regrade, "regrade.mkv"),
                             "--cache-dir", cache, "-j", "4", *cpu], capture_output=True, text=True, env=env())
        rerep = os.path.join(regrade, "regrade.report.json")
        if rr.returncode != 0 or not os.path.exists(rerep):
            check("final timeline renders", False, (rr.stderr or rr.stdout)[-500:])
        else:
            ref = load(rerep)
            # Same machine + adapter -> bit-exact: out.mkv must be the final timeline.
            with open(out, "rb") as fh:
                got_sha = hashlib.sha256(fh.read()).hexdigest()
            with open(os.path.join(regrade, "regrade.mkv"), "rb") as fh:
                want_sha = hashlib.sha256(fh.read()).hexdigest()
            check("out.mkv is a render of the final timeline.json", got_sha == want_sha,
                  "stale or edited render" if got_sha != want_sha else "bit-exact re-render")
            if "frames" in rexp:
                check(f"{rexp['frames']} frames", ref["total_frames"] == rexp["frames"], f"got {ref['total_frames']}")
        for L in rexp.get("luma", []):
            y = luma(out, L["frame"])
            ok = y is not None and (y <= L["max"] if "max" in L else True) and (y >= L["min"] if "min" in L else True)
            check(f"frame {L['frame']} mean luma {'<= ' + str(L['max']) if 'max' in L else '>= ' + str(L['min'])}",
                  ok, f"YAVG {y}")

    # ---- perceptual quality check (SeePlus's checker via the engine hook)
    cexp = exp.get("check")
    if cexp is not None and have and tl is not None:
        os.makedirs(regrade, exist_ok=True)
        args = []
        if "cuts" in cexp:
            p = os.path.join(regrade, "brief-cuts.json")
            json.dump({"cuts": cexp["cuts"]}, open(p, "w"))
            args += ["--brief-cuts", p]
        if "config" in cexp:
            p = os.path.join(regrade, "check-config.json")
            json.dump(cexp["config"], open(p, "w"))
            args += ["--config", p]
        args += cexp.get("args", [])
        cr = subprocess.run([FERROCUT, "check", out, "--timeline", tl_path, "--require", "--timeout", "600",
                             "--", *args], capture_output=True, text=True, env=env())
        try:
            c = json.loads(cr.stdout)
        except Exception:  # noqa: BLE001
            c = {"status": "error", "message": (cr.stderr or "")[-500:]}
        with open(os.path.join(regrade, "check.json"), "w") as fh:
            json.dump(c, fh, indent=2)
        probs = [f"{p['reason']} {p['range']}" + (f" ({p['message']})" if p.get("message") else "")
                 for p in c.get("problems", [])]
        check("quality check passes (ferrocut check --require)", c.get("status") == "pass",
              f"{c.get('status')}: {c.get('message') or probs}")
        m = (c.get("report") or {}).get("measured", {})
        warnings = [f"{w['reason']} {w['range']}" for w in c.get("warnings", [])]
    else:
        m, warnings = {}, []

    passed = sum(c["pass"] for c in checks)
    result = {"task": os.path.basename(taskdir.rstrip("/")), "passed": passed, "total": len(checks),
              "score": round(passed / len(checks), 3) if checks else 0.0,
              "pass": passed == len(checks), "checks": checks,
              "measured": {k: m.get(k) for k in ("integrated_lufs", "true_peak_dbtp", "detected_cuts")},
              "warnings": warnings}
    with open(result_path, "w") as fh:
        json.dump(result, fh, indent=2)
    for c in checks:
        print(f"  {'PASS' if c['pass'] else 'FAIL'} {c['name']}" + (f"  [{c['detail']}]" if c["detail"] and not c["pass"] else ""))
    print(f"  score {passed}/{len(checks)}")
    return result


if __name__ == "__main__":
    cmd, *a = sys.argv[1:]
    if cmd == "setup":
        setup(*a)
    elif cmd == "reference":
        reference(*a)
    elif cmd == "grade":
        r = grade(*a)
        sys.exit(0 if r["pass"] else 1)
    else:
        sys.exit(f"unknown command {cmd}")
