#!/usr/bin/env python3
"""Run a creative brief with the user's installed, authenticated Codex."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import time
import tomllib

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--work", type=Path, required=True)
parser.add_argument("--timeout", type=int, default=3600)
parser.add_argument("--model", help="Explicit user-authorized model for this run only")
parser.add_argument("--brief", type=Path, help="Creative brief to use instead of the local-only baseline")
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
work = args.work.resolve()
work.mkdir(parents=True, exist_ok=False)
evidence = work / "evidence"
evidence.mkdir()
brief = (args.brief or root / "eval/creative/agent-editor-60s/BRIEF.md").resolve()
if not brief.is_file():
    raise SystemExit(f"Creative brief does not exist: {brief}")
shutil.copy2(brief, work / "BRIEF.md")
config = tomllib.loads((Path.home() / ".codex/config.toml").read_text())
codex = shutil.which("codex")
if not codex:
    raise SystemExit("Codex is not installed")
model = args.model or config.get("model")
effort = config.get("model_reasoning_effort")
command = [codex, "exec", "--cd", str(work), "--skip-git-repo-check", "--approve-for-me",
           "--json", "-o", str(evidence / "codex-final.txt"),
           "-c", 'mcp_servers.ferrocut.args=' + json.dumps(["--root", str(work)]),
           "-c", 'mcp_servers.ferrocut.cwd=' + json.dumps(str(work)),
           "-c", 'sandbox_workspace_write.network_access=true',
           *(["--model", model] if model else []),
           "Read BRIEF.md and create the requested finished video yourself using Ferrocut. "
           "Begin working now; do not stop at a plan. Preserve evidence and deliver all files in delivery/."]
env = os.environ.copy()
for key in ("OPENAI_API_KEY", "CODEX_API_KEY"):
    env.pop(key, None)
env["CODEX_ROUTER"] = "off"
login = subprocess.run([codex, "login", "status"], env=env, capture_output=True, text=True)
if login.returncode or "chatgpt" not in (login.stdout + login.stderr).lower():
    raise SystemExit("An existing ChatGPT Codex login is required; no paid API fallback")
manifest = {
    "task": brief.parent.name, "started_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "source_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
    "brief_sha256": hashlib.sha256(brief.read_bytes()).hexdigest(),
    "codex_version": subprocess.check_output([codex, "--version"], text=True, env=env).strip(),
    "model": model, "configured_model": config.get("model"),
    "reasoning_effort": effort, "auth": "existing ChatGPT login",
    "timeout_seconds": args.timeout, "work": str(work), "command": command,
    "binaries": {name: hashlib.sha256((Path.home() / ".local/bin" / name).read_bytes()).hexdigest()
                 for name in ("ferrocut", "ferrocut-mcp", "ferrocut-perceive", "ferrocut-deliver")},
    "producer_assistance": ["Installed release binaries and registered the Ferrocut MCP server", "Provided the fixed creative brief; no script or timeline supplied"],
    "status": "running",
}
manifest_path = evidence / "run-manifest.json"
manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
print(json.dumps({"work": str(work), "model": model, "reasoning_effort": effort, "status": "launching"}), flush=True)
started = time.monotonic()
with (evidence / "codex.jsonl").open("w") as stdout, (evidence / "codex.stderr.log").open("w") as stderr:
    process = subprocess.Popen(command, cwd=work, env=env, stdin=subprocess.DEVNULL,
                               stdout=stdout, stderr=stderr, start_new_session=True)
    try:
        code = process.wait(timeout=args.timeout)
        manifest.update(status="exited", exit_code=code)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=20)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        manifest.update(status="timed_out", exit_code=process.returncode)
manifest["elapsed_seconds"] = round(time.monotonic() - started, 2)
manifest["finished_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
print(json.dumps({"status": manifest["status"], "exit_code": manifest["exit_code"], "elapsed_seconds": manifest["elapsed_seconds"]}), flush=True)
raise SystemExit(manifest["exit_code"] or 0)
