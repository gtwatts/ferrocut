#!/usr/bin/env bash
# Ferrocut in-house eval.
#
#   eval/run.sh <task|all> [--agent none|codex] [--model M] [--timeout SECS]
#
# --agent none   (default) apply the task's reference solution through
#                ferrocut-mcp, then grade it: validates tasks + grader + engine.
# --agent codex  run Codex CLI non-interactively on the task (Todd's ChatGPT
#                plan via the existing login; no API key, no paid API calls)
#                with ferrocut-mcp as its only MCP server, then grade.
#
# Every run gets fresh workdirs under $EVAL_WORK (default /tmp/ferrocut-eval/<run-id>/<task>/)
# with copies of the clips, timeline.json and BRIEF.md. Results land in
# eval/results/<run-id>/ (gitignored): one JSON per task plus summary.json.
#
# Codex isolation: CODEX_HOME is a scratch dir ($CODEX_SCRATCH, default
# ~/.cache/ferrocut-eval/codex-home) with its own config.toml; ~/.codex/config.toml
# is never read or written. The only link to ~/.codex is a symlink to auth.json
# (the existing login). Codex is never logged in from here: if `codex login status`
# fails non-interactively, the run stops.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd -P)"
EVAL="$REPO/eval"
BIN="${FERROCUT_BIN_DIR:-$REPO/target/release}"
AGENT=none MODEL="" TIMEOUT=1200 TASK=""
while [ $# -gt 0 ]; do
  case "$1" in
    --agent) AGENT=$2; shift 2 ;;
    --model) MODEL=$2; shift 2 ;;
    --timeout) TIMEOUT=$2; shift 2 ;;
    -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
    -*) echo "unknown option $1" >&2; exit 2 ;;
    *) TASK=$1; shift ;;
  esac
done
[ -n "$TASK" ] || { echo "usage: eval/run.sh <task|all> [--agent none|codex]" >&2; exit 2; }
case "$AGENT" in none|codex) ;; *) echo "--agent must be none or codex" >&2; exit 2 ;; esac
if [ "$TASK" = all ]; then
  mapfile -t TASKS < <(cd "$EVAL/tasks" && printf '%s\n' */ | tr -d / | sort)
else
  [ -d "$EVAL/tasks/$TASK" ] || { echo "no task $TASK (see eval/tasks/)" >&2; exit 2; }
  TASKS=("$TASK")
fi

for b in ferrocut ferrocut-mcp ferrocut-perceive; do
  [ -x "$BIN/$b" ] || { echo "missing $BIN/$b: cargo build --release -p ferrocut-engine -p ferrocut-mcp -p ferrocut-perceive" >&2; exit 2; }
done
[ -f "$EVAL/media/clips/s1.mkv" ] || "$EVAL/fetch-media.sh"

RUN="${RUN_ID:-$(date +%Y%m%d-%H%M%S)-$AGENT}"
WORK="${EVAL_WORK:-/tmp/ferrocut-eval}/$RUN"
RESULTS="$EVAL/results/$RUN"
mkdir -p "$WORK" "$RESULTS"
export FERROCUT_PERCEIVE="$BIN/ferrocut-perceive"

if [ "$AGENT" = codex ]; then
  CODEX="${CODEX:-$(command -v codex || echo "$HOME/.local/bin/codex")}"
  CH="${CODEX_SCRATCH:-$HOME/.cache/ferrocut-eval/codex-home}"
  mkdir -p "$CH"
  [ -e "$CH/auth.json" ] || ln -s "$HOME/.codex/auth.json" "$CH/auth.json"
  if ! CODEX_HOME="$CH" "$CODEX" login status </dev/null > "$RESULTS/codex-login-status.txt" 2>&1; then
    echo "codex auth is not usable non-interactively (see $RESULTS/codex-login-status.txt); not logging in" >&2
    exit 3
  fi
  grep -qi "api key" "$RESULTS/codex-login-status.txt" && {
    echo "codex is logged in with an API key (paid API); refusing to run" >&2; exit 3; }
fi

for t in "${TASKS[@]}"; do
  wd="$WORK/$t"
  echo "== $t ($AGENT) in $wd"
  python3 "$EVAL/lib/evallib.py" setup "$EVAL/tasks/$t" "$wd"
  start=$(date +%s)
  case "$AGENT" in
    none)
      python3 "$EVAL/lib/evallib.py" reference "$EVAL/tasks/$t" "$wd" > "$RESULTS/$t.agent.log" 2>&1 \
        || { echo "  reference solution failed:"; tail -n 5 "$RESULTS/$t.agent.log"; }
      ;;
    codex)
      cat > "$CH/config.toml" <<TOML
# Scratch config for eval/run.sh (CODEX_HOME=$CH); not ~/.codex/config.toml.
approval_policy = "never"
sandbox_mode = "workspace-write"

[mcp_servers.ferrocut]
command = "$BIN/ferrocut-mcp"
args = ["--root", "$wd"]
env = { FERROCUT_PERCEIVE = "$BIN/ferrocut-perceive" }
startup_timeout_sec = 30
tool_timeout_sec = 900
TOML
      prompt="You are editing a video project with the Ferrocut MCP server (tools: timeline_get, edit_apply, plan, render, report_read, quality_check, ...). The project is the current directory. Read BRIEF.md and do what it asks. Edit timeline.json (with edit_apply ops or by editing the JSON file directly), render with the MCP render tool to the output named in the brief, and verify the result with quality_check before you finish. Paths are relative to the project directory."
      set +e
      # No API key in the environment: Codex must use the ChatGPT login, never paid API billing.
      # CODEX_ROUTER=off: bypass the local clef-harness model router (if installed) so the
      # model is the scratch config default, not a per-prompt tier.
      env -u OPENAI_API_KEY -u CODEX_API_KEY CODEX_ROUTER=off CODEX_HOME="$CH" timeout "$TIMEOUT" "$CODEX" exec --cd "$wd" --skip-git-repo-check --json \
        ${MODEL:+-m "$MODEL"} -o "$RESULTS/$t.codex-last-message.txt" "$prompt" \
        </dev/null > "$RESULTS/$t.codex.jsonl" 2> "$RESULTS/$t.agent.log"
      echo "  codex exit $?"
      python3 "$EVAL/lib/agent_usage.py" "$RESULTS/$t.codex.jsonl" "$RESULTS/$t.usage.json" || true
      set -e
      ;;
  esac
  echo "  agent time $(( $(date +%s) - start )) s"
  python3 "$EVAL/lib/evallib.py" grade "$EVAL/tasks/$t" "$wd" "$RESULTS/$t.json" || true
done

python3 - "$RESULTS" "$AGENT" "${TASKS[@]}" <<'PY'
import json, os, sys
res, agent, tasks = sys.argv[1], sys.argv[2], sys.argv[3:]
rows = [json.load(open(os.path.join(res, f"{t}.json"))) for t in tasks]
summary = {"agent": agent, "tasks": {r["task"]: {"score": r["score"], "pass": r["pass"],
           "passed": r["passed"], "total": r["total"]} for r in rows},
           "passed_tasks": sum(r["pass"] for r in rows), "mean_score": round(sum(r["score"] for r in rows) / len(rows), 3)}
json.dump(summary, open(os.path.join(res, "summary.json"), "w"), indent=2)
print(f"\n{'task':<22} {'score':>7}  pass")
for r in rows:
    print(f"{r['task']:<22} {r['passed']:>3}/{r['total']:<3}  {'yes' if r['pass'] else 'NO'}")
print(f"{agent}: {summary['passed_tasks']}/{len(rows)} tasks passed, mean score {summary['mean_score']}  ({res})")
PY
