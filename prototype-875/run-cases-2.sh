#!/bin/bash
# PROTOTYPE (ratect#875), throwaway. Cases 1a and 2 again, with waits that hold.
cd "$(dirname "$0")"
BIN=../target/debug/ratect-compat
export RATECT_PROTOTYPE_RUNS_DIR="$PWD/runs"
P=ratect-875-probe
probe() { echo "  [$1]"; python3 probe.py "$RATECT_PROTOTYPE_RUNS_DIR" $P | sed 's/^/    /'; }
wipe() { docker ps -aq --filter label=eu.orican.ratect.project=$P | xargs -r docker rm -f >/dev/null
         docker network ls -q --filter label=eu.orican.ratect.project=$P | xargs -r docker network rm >/dev/null; }
wait_for() { for _ in $(seq 150); do eval "$1" && return 0; sleep 0.2; done; echo "  TIMED OUT waiting: $1"; return 1; }
runs() { docker ps -a --filter label=eu.orican.ratect.project=$P --format '{{.Label "eu.orican.ratect.run"}}' | sort -u | wc -l | tr -d ' '; }
wipe

echo "CASE 1a: kill during cleanup"
$BIN -f probe.yml -o simple quick-with-slow-stop >out.log 2>&1 & pid=$!
wait_for 'grep -q task-done out.log'; sleep 1.5
echo "  ratect alive: $(kill -0 $pid 2>/dev/null && echo yes || echo no); last line: $(tail -1 out.log)"
probe "after task output, process alive"
kill -9 $pid; wait $pid 2>/dev/null; sleep 0.5; probe "after SIGKILL"; wipe

echo "CASE 2: one invocation, two task runs"
$BIN -f probe.yml -o simple --no-cleanup-after-success with-prereq >out.log 2>&1 & pid=$!
wait_for '[ "$(runs)" = 2 ]'; sleep 2
echo "  ratect alive: $(kill -0 $pid 2>/dev/null && echo yes || echo no)"
probe "second task run under way, same process"
kill -9 $pid; wait $pid 2>/dev/null; sleep 0.5; probe "after SIGKILL"; wipe
