#!/usr/bin/env python3
"""Summarize how a Codex run drove the project, from its `codex exec --json` log:
MCP tool calls by tool, shell commands, and whether it stayed inside the tools
(no hand edits of timeline.json, no reading engine source).

    agent_usage.py <task>.codex.jsonl [out.json]
"""
import json
import re
import sys
from collections import Counter

WRITE_HINTS = ("json.dump", "write_text", ".write(", "sed -i", "> timeline.json", ">timeline.json",
               "tee timeline.json", "jq ", "mv ", "cp ", "perl -i")
SOURCE_HINTS = re.compile(r"(crates/|\.rs\b|Cargo\.toml|/src/|ferrocut-engine|seeplus)", re.I)


def summarize(path):
    mcp, cmds, edits = Counter(), [], []
    errors = 0
    for line in open(path, encoding="utf-8", errors="replace"):
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if e.get("type") != "item.completed":
            continue
        it = e.get("item") or {}
        t = it.get("type")
        if t == "mcp_tool_call":
            mcp[it.get("tool", "?")] += 1
            if it.get("status") == "failed" or (it.get("result") or {}).get("is_error"):
                errors += 1
        elif t == "command_execution":
            cmds.append(it.get("command", ""))
        elif t == "file_change":
            edits += [c.get("path", "") for c in it.get("changes", [])]
    hand_edits = [p for p in edits if p.endswith("timeline.json")]
    shell_writes = [c for c in cmds if "timeline.json" in c and any(h in c for h in WRITE_HINTS)]
    source_reads = [c for c in cmds if SOURCE_HINTS.search(c)]
    return {
        "mcp_calls": sum(mcp.values()),
        "mcp_by_tool": dict(sorted(mcp.items())),
        "mcp_errors": errors,
        "shell_commands": len(cmds),
        "timeline_hand_edits": len(hand_edits) + len(shell_writes),
        "timeline_hand_edit_examples": (hand_edits + shell_writes)[:3],
        "engine_source_reads": len(source_reads),
        "engine_source_read_examples": [c[:200] for c in source_reads[:3]],
        "stayed_inside_tools": not hand_edits and not shell_writes and not source_reads,
    }


if __name__ == "__main__":
    s = summarize(sys.argv[1])
    if len(sys.argv) > 2:
        json.dump(s, open(sys.argv[2], "w"), indent=2)
    print(f"  usage: {s['mcp_calls']} MCP calls {s['mcp_by_tool']}, {s['shell_commands']} shell commands, "
          f"{s['timeline_hand_edits']} hand edits of timeline.json, {s['engine_source_reads']} source reads"
          f" -> stayed inside tools: {s['stayed_inside_tools']}")
