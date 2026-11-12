#!/bin/bash
# PROTOTYPE (ratect#875), throwaway. Runs each case and prints what the probe saw.
cd "$(dirname "$0")"
BIN=../target/debug/ratect-compat
export RATECT_PROTOTYPE_RUNS_DIR="$PWD/runs"
rm -rf runs; mkdir runs
probe() { echo "  [$1]"; python3 probe.py "$RATECT_PROTOTYPE_RUNS_DIR" | sed 's/^/    /'; }
wipe() { docker ps -aq --filter label=eu.orican.ratect.project=ratect-875-probe | xargs -r docker rm -f >/dev/null
         docker network ls -q --filter label=eu.orican.ratect.project=ratect-875-probe | xargs -r docker network rm >/dev/null; }
wait_for() { for _ in $(seq 100); do eval "$1" && return 0; sleep 0.2; done; echo "  TIMED OUT waiting: $1"; return 1; }
count() { docker ps -a --filter label=eu.orican.ratect.project=ratect-875-probe --filter "label=eu.orican.ratect.role=$1" ${2:+--filter status=$2} -q | wc -l | tr -d ' '; }
wipe

echo "CASE control: live run, task container running"
$BIN -f probe.yml -o simple long >/dev/null 2>&1 & pid=$!
wait_for '[ "$(count task running)" = 1 ]'; probe "mid-task"
kill -9 $pid; wait $pid 2>/dev/null; sleep 0.5; probe "after SIGKILL mid-task"; wipe

echo "CASE 1a: kill during cleanup (dependency running, task container gone)"
$BIN -f probe.yml -o simple quick-with-slow-stop >/dev/null 2>&1 & pid=$!
wait_for '[ "$(count task running)" = 1 ] || [ "$(count task exited)" = 1 ]'
wait_for '[ "$(count task)" = 0 ]'; probe "cleanup under way, process alive"
kill -9 $pid; wait $pid 2>/dev/null; sleep 0.5; probe "after SIGKILL during cleanup"; wipe

echo "CASE 1b: task container exited, output still draining into a stalled pipe"
( $BIN -f probe.yml -o simple big-output 2>/dev/null & echo $! > pid; wait ) | ( sleep 12; wc -c ) & pipe=$!
sleep 8; probe "8s in, reader has read nothing; ratect pid $(cat pid) alive: $(kill -0 $(cat pid) 2>/dev/null && echo yes || echo no)"
wait $pipe; sleep 0.5; probe "after drain and exit"; wipe

echo "CASE 1c: killed while starting (dependency setup command running, no task container)"
$BIN -f probe.yml -o simple slow-starting >/dev/null 2>&1 & pid=$!
wait_for '[ "$(count dependency running)" = 1 ]'; sleep 2; probe "starting, process alive"
kill -9 $pid; wait $pid 2>/dev/null; sleep 0.5; probe "after SIGKILL while starting"; wipe

echo "CASE 2: one invocation, two task runs (prerequisite left behind with --no-cleanup-after-success)"
$BIN -f probe.yml -o simple --no-cleanup-after-success with-prereq >/dev/null 2>&1 & pid=$!
wait_for '[ "$(count task running)" = 1 ]'; sleep 1; probe "main task running, prerequisite's run finished, same process alive"
kill -9 $pid; wait $pid 2>/dev/null; sleep 0.5; probe "after SIGKILL"; wipe
echo "lock files left: $(ls runs | wc -l | tr -d ' ')"
