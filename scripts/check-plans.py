#!/usr/bin/env python3
"""Check the roadmap against the plans.

1. Every unticked roadmap item (`**R<n>.<nn>**`) is claimed by at least one
   plan line that also names a task ID (`NAMI-<X>-<n>`).
2. Every roadmap ID a plan names exists in the roadmap.
3. Plans that are not marked `**Status:** done` use no pre-restructure path,
   module, migration number or per-task-PR wording
   (docs/superpowers/decisions/2026-10-19-domain-restructure.md).

Run from the repository root: `python3 scripts/check-plans.py`.
Exit code 0 means clean. `-v` prints every lint hit.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ROADMAP = ROOT / "docs/superpowers/roadmap.md"
PLANS = sorted((ROOT / "docs/superpowers/plans").glob("*.md"))

ITEM = re.compile(r"^- \[( |x)\] \*\*(R\d+\.\d+)\*\*")
RID = re.compile(r"\bR\d+\.\d{2}\b")
TASK = re.compile(r"\bNAMI-[A-Z]+-\d+\b")

BASELINES = {"0001_auth.sql", "0002_projects.sql", "0003_uptime.sql", "0004_incidents.sql", "0005_logs.sql"}
STANDALONE_TESTS = {"clickhouse_proxy", "uptime_checker_proxy", "logs_decode_memory"}

OLD_LOGS = "store|normalize|msgpack|pipeline|pipeline_cache|rate_limit|budget|usage|search|tail|export|pattern|patterns|slots|saved_query"
OLD_UPTIME = "dns|ping|tls|expiry|mail|udp|pings|heartbeat|checker"
OLD_CORE = "config|crypto|db|clickhouse|email|error|serve|state|router|download"
OLD_UI = r"#lib/(uuid|uptime|availability|maintenance|logs)['\"]|#lib/server/(engine|relay-stream|fake-event|monitor-form|heartbeat-form|maintenance-form|import-form|availability|log-proxy|source-form)['\"]"

LINTS = [
    ("machine-specific path", re.compile(r"/Users/|/home/[a-z]+/")),
    ("per-task worktree", re.compile(r"\.worktrees/NAMI-")),
    ("per-task PR flow", re.compile(r"(?i)one worktree per task|auto-merge on a clean review|open the PR, and auto-merge")),
    ("old models/routes module", re.compile(r"\b(crate::)?(models|routes)::[a-z_]+")),
    ("old models/routes path", re.compile(r"\bsrc/(models|routes)/[a-z_]+\.rs")),
    ("old core file", re.compile(rf"\bsrc/({OLD_CORE})\.rs\b")),
    ("old logs file", re.compile(rf"\bsrc/logs/({OLD_LOGS})\.rs\b")),
    ("old uptime file", re.compile(rf"\bsrc/uptime/({OLD_UPTIME})\.rs\b")),
    ("old logs module", re.compile(rf"(?<![a-z_:])logs::({OLD_LOGS})::")),
    ("old core module", re.compile(rf"(?<![a-z_:])(crate::)?({OLD_CORE})::[A-Za-z_]")),
    ("old UI import", re.compile(OLD_UI)),
    ("hardcoded migration number", re.compile(r"\b\d{4}_[a-z0-9_]+\.sql\b")),
    ("old test binary", re.compile(r"\btests/([a-z_0-9]+)\.rs\b")),
]


def lint_hits(rule, rx, line):
    hits = []
    for m in rx.finditer(line):
        text = m.group(0)
        if rule == "hardcoded migration number" and text in BASELINES:
            continue
        if rule == "old test binary" and m.group(1) in STANDALONE_TESTS:
            continue
        hits.append(text)
    return hits


def main():
    verbose = "-v" in sys.argv
    items = {}
    for line in ROADMAP.read_text().splitlines():
        m = ITEM.match(line)
        if m:
            items[m.group(2)] = m.group(1) == "x"

    claimed = {}
    unknown = []
    lint = {}
    for plan in PLANS:
        text = plan.read_text()
        done = "**Status:** done" in text
        for no, line in enumerate(text.splitlines(), 1):
            for rid in RID.findall(line):
                if rid not in items:
                    unknown.append(f"{plan.name}:{no}: {rid}")
                elif TASK.search(line):
                    claimed.setdefault(rid, set()).add(plan.name)
            if done:
                continue
            for rule, rx in LINTS:
                for hit in lint_hits(rule, rx, line):
                    lint.setdefault(plan.name, {}).setdefault(rule, []).append(f"{no}: {hit}")

    missing = [rid for rid, ticked in items.items() if not ticked and rid not in claimed]
    print(f"roadmap items: {len(items)}, unticked: {sum(not t for t in items.values())}, unclaimed: {len(missing)}")
    for rid in missing:
        print(f"  UNCLAIMED {rid}")
    for u in unknown:
        print(f"  UNKNOWN ID {u}")
    for plan, rules in sorted(lint.items()):
        print(f"lint {plan}: {sum(len(v) for v in rules.values())} hits")
        for rule, hits in sorted(rules.items()):
            print(f"  {rule}: {len(hits)}")
            if verbose:
                for h in hits:
                    print(f"    {h}")
    sys.exit(1 if missing or unknown or lint else 0)


if __name__ == "__main__":
    main()
