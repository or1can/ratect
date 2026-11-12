#!/usr/bin/env python3
"""PROTOTYPE (ratect#875), throwaway. For every container carrying a run label,
print container state next to what the run's lock says: DEAD (lock taken),
ALIVE (lock refused), UNKNOWN (no lock file)."""
import fcntl, json, os, subprocess, sys
runs_dir = sys.argv[1]
project = sys.argv[2] if len(sys.argv) > 2 else None
out = subprocess.run(["docker", "ps", "-a", "--filter", "label=eu.orican.ratect.run",
                      "--format", "{{json .}}"], capture_output=True, text=True, check=True).stdout
seen = {}
for line in out.splitlines():
    c = json.loads(line)
    labels = dict(kv.split("=", 1) for kv in c["Labels"].split(",") if "=" in kv)
    if project and labels.get("eu.orican.ratect.project") != project:
        continue
    seen.setdefault(labels["eu.orican.ratect.run"], []).append(
        f'{labels.get("eu.orican.ratect.task","?")}/{labels.get("eu.orican.ratect.role","?")}:{c["State"]}')
for run, containers in seen.items():
    path = os.path.join(runs_dir, run + ".lock")
    try:
        fd = os.open(path, os.O_RDWR)
    except FileNotFoundError:
        verdict = "UNKNOWN"
    else:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            verdict = "DEAD"
        except BlockingIOError:
            verdict = "ALIVE"
        os.close(fd)
    print(f"{verdict:8} {run} {' '.join(sorted(containers))}")
if not seen:
    print("no labelled containers")
