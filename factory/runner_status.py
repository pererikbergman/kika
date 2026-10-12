#!/usr/bin/env python3
"""Write factory/runner.json for the dashboard's CI runner panel.

Collects, for the repo in board.json:
  - the repo's self-hosted runners (name, status, busy)       gh api .../actions/runners
  - the latest workflow runs                                  gh run list
  - whether each local runner's launchd service is loaded     launchctl list
  - the tail of each local runner's newest Runner_*.log       ~/actions-runners/*/_diag

Run it next to the dashboard server, from the factory folder:
    python3 runner_status.py            # refresh every 20 s
    python3 runner_status.py --once     # write once and exit
runner.json is runtime state and is git-ignored, like board.json.
"""

import argparse
import glob
import json
import os
import subprocess
import sys
import time
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "runner.json")
RUNNERS_HOME = os.path.expanduser("~/actions-runners")
LOG_LINES = 12
RUN_LIMIT = 10


def sh(args, timeout=20):
    """Run a command (argv, no shell); return (ok, stdout or error text)."""
    try:
        p = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired) as e:
        return False, str(e)
    if p.returncode != 0:
        return False, (p.stderr or p.stdout).strip()[-300:]
    return True, p.stdout


def repo_name():
    try:
        with open(os.path.join(HERE, "board.json")) as f:
            return json.load(f)["repo"]
    except (OSError, ValueError, KeyError):
        ok, out = sh(["gh", "repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner"])
        return out.strip() if ok else None


def runners(repo):
    ok, out = sh(["gh", "api", f"repos/{repo}/actions/runners",
                  "--jq", "[.runners[] | {name, status, busy, labels: [.labels[].name]}]"])
    return (json.loads(out), None) if ok else ([], out)


def runs(repo):
    ok, out = sh(["gh", "run", "list", "-R", repo, "--limit", str(RUN_LIMIT), "--json",
                  "databaseId,number,displayTitle,status,conclusion,workflowName,"
                  "headBranch,event,createdAt,url"])
    return (json.loads(out), None) if ok else ([], out)


def local_runners():
    """Each runner folder under ~/actions-runners: service state and log tail."""
    ok, launchd = sh(["launchctl", "list"])
    services = launchd.splitlines() if ok else []
    found = []
    for folder in sorted(glob.glob(os.path.join(RUNNERS_HOME, "*"))):
        svc_file = os.path.join(folder, ".service")
        if not os.path.isfile(svc_file):
            continue
        with open(svc_file) as f:
            label = os.path.basename(f.read().strip()).removesuffix(".plist")
        line = next((l for l in services if l.split("\t")[-1] == label), None)
        pid = line.split("\t")[0] if line else None
        logs = sorted(glob.glob(os.path.join(folder, "_diag", "Runner_*.log")))
        tail = []
        if logs:
            with open(logs[-1], errors="replace") as f:
                tail = [l.rstrip("\n") for l in f.readlines()[-LOG_LINES:]]
        found.append({
            "folder": folder.replace(os.path.expanduser("~"), "~", 1),
            "service": label,
            "loaded": line is not None,
            "running": pid not in (None, "-"),
            "log": os.path.basename(logs[-1]) if logs else None,
            "tail": tail,
        })
    return found


def snapshot():
    repo = repo_name()
    data = {"updated": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "repo": repo, "errors": []}
    if repo:
        data["runners"], err = runners(repo)
        if err:
            data["errors"].append(f"runners: {err}")
        data["runs"], err = runs(repo)
        if err:
            data["errors"].append(f"runs: {err}")
    else:
        data["runners"], data["runs"] = [], []
        data["errors"].append("no repo in board.json and gh repo view failed")
    data["local"] = local_runners()
    return data


def write(data):
    tmp = OUT + ".tmp"
    with open(tmp, "w") as f:
        json.dump(data, f, indent=2)
    os.replace(tmp, OUT)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--once", action="store_true", help="write once and exit")
    ap.add_argument("--interval", type=int, default=20, help="seconds between refreshes")
    a = ap.parse_args()
    while True:
        write(snapshot())
        if a.once:
            return
        time.sleep(max(5, a.interval))


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(0)
