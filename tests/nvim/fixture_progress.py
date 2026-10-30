#!/usr/bin/env python3
"""Stream progress separately from fixture workspace responses and diagnostics."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

request = json.load(sys.stdin)
operation = request["op"]


def progress(stage, message, completed=None, total=None):
    event = dict(event="progress", stage=stage, message=message)
    if completed is not None:
        event.update(completed=completed, total=total)
    print(json.dumps(event), file=sys.stderr, flush=True)


if operation == "stream":
    sys.stderr.write('{"event":"pro')
    sys.stderr.flush()
    time.sleep(0.05)
    sys.stderr.write('gress","stage":"publish_comments","message":"Comments","completed":1,"total":2}\n')
    sys.stderr.flush()
    time.sleep(0.15)
    sys.stderr.write(json.dumps(dict(event="progress", stage="publish_comments", message="Comments", completed=2, total=2)))
    sys.stderr.flush()
    print(json.dumps(dict(ok=True)))
    sys.exit(0)

if operation == "failure":
    progress("publish_comments", "Comments", 1, 2)
    print(json.dumps(dict(event="progress", stage="bad", message="Bad", completed="wrong")), file=sys.stderr)
    sys.stderr.write("Actual provider diagnostic\nFinal diagnostic without newline")
    sys.stderr.flush()
    sys.exit(7)

if operation == "publish":
    progress("publish_comments", "Comments", 0, 2)
elif operation == "fetch_refresh":
    progress("fetch_pr", "Refreshing PR state")
    time.sleep(0.1)
elif operation == "fetch_objects":
    progress("prepare_git", "Preparing Git objects", 0, 2)
    time.sleep(0.2)
elif operation == "apply_refresh":
    progress("apply_refresh", "Applying PR state")
elif operation in ("entry", "route", "select", "list"):
    progress("fetch_pr", "Fetching PR")

result = subprocess.run([str(Path(__file__).with_name("fixture_pr.py")), "workspace"],
                        input=json.dumps(request), capture_output=True, text=True)
if operation == "publish":
    progress("publish_comments", "Comments", 2, 2)
elif operation == "fetch_objects":
    progress("prepare_git", "Preparing Git objects", 2, 2)
sys.stdout.write(result.stdout)
sys.stderr.write(result.stderr)
sys.exit(result.returncode)
