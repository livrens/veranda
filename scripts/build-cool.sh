#!/usr/bin/env bash
# Run a cargo command with a thermal watchdog for laptops that overheat, and a disk budget so
# `target/` never swallows the drive.
# The compiler is paused (SIGSTOP) when the CPU package passes $HOT and resumed (SIGCONT)
# once it cools below $COOL. One job, lowest priority.
#
#   scripts/build-cool.sh build -p veranda-desktop
#   HOT=78 COOL=62 scripts/build-cool.sh build --release -p veranda-desktop
#
# Disk: incremental compilation is off (CARGO_INCREMENTAL=0; it alone grew to 30 GB) unless
# INCREMENTAL=1. Before every run `target/` is measured; above $TARGET_MAX_MB (default 20 GB)
# the build caches are pruned (deps, build scripts, fingerprints, release and cross-compile
# outputs). The UI scratch root, screenshots and *.bundle / *.log files in target/ are kept.
set -u
HOT=${HOT:-80}; COOL=${COOL:-65}; JOBS=${JOBS:-1}
TARGET_MAX_MB=${TARGET_MAX_MB:-20480}
export CARGO_INCREMENTAL=${INCREMENTAL:-0}
cd "$(dirname "$0")/.." || exit 1

prune_target() {
  local used
  used=$(du -sm target 2>/dev/null | cut -f1); used=${used:-0}
  [ "$used" -le "$TARGET_MAX_MB" ] && return 0
  echo "[disk] target/ is ${used} MB (limit ${TARGET_MAX_MB} MB): pruning build caches"
  for d in target/debug/incremental target/release/incremental; do [ -d "$d" ] && rm -rf "$d"; done
  used=$(du -sm target 2>/dev/null | cut -f1); used=${used:-0}
  [ "$used" -le "$TARGET_MAX_MB" ] && { echo "[disk] now ${used} MB"; return 0; }
  for d in target/debug target/release target/x86_64-pc-windows-gnu target/aarch64-apple-darwin target/x86_64-apple-darwin target/tmp; do
    [ -d "$d" ] && rm -rf "$d"
  done
  echo "[disk] now $(du -sm target 2>/dev/null | cut -f1) MB (full rebuild follows)"
}
prune_target

zone=$(grep -l x86_pkg_temp /sys/class/thermal/thermal_zone*/type 2>/dev/null | head -1 | xargs -r dirname)
temp() { [ -n "$zone" ] && echo $(( $(cat "$zone/temp") / 1000 )) || echo 0; }
until [ "$(temp)" -le "$COOL" ]; do sleep 5; done
# put -j before any `--` so it reaches cargo, not rustc
sub="$1"; shift
nice -n 19 cargo "$sub" -j "$JOBS" "$@" &
pid=$!
paused=0
while kill -0 "$pid" 2>/dev/null; do
  t=$(temp)
  if [ "$paused" -eq 0 ] && [ "$t" -ge "$HOT" ]; then pkill -STOP -P "$pid"; kill -STOP "$pid"; paused=1; echo "[cool] paused at ${t}C"; fi
  if [ "$paused" -eq 1 ] && [ "$t" -le "$COOL" ]; then kill -CONT "$pid"; pkill -CONT -P "$pid"; paused=0; echo "[cool] resumed at ${t}C"; fi
  sleep 2
done
wait "$pid"
status=$?
echo "[disk] target/ is $(du -sm target 2>/dev/null | cut -f1) MB"
exit $status
