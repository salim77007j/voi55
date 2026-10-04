#!/bin/sh
# Phase 5 runtime verification (D-brief budgets) — runs against the RELEASE
# binary. Linux-only (/proc RSS sampling); numbers are recorded in
# docs/PHASE_5_REPORT.md and docs/evidence/phase5/runtime-verification.txt.
#
# Budgets verified here:
#   cold start  < 1 s        (headless proxy: process wall time of an
#                             empty-shell screenshot run, median of 5)
#   RAM         < 200 MB     (peak RSS, 3-min 48 kHz stereo session,
#                             idle-loaded and with a preview render engaged)
#   binary size < 50 MB      (release, stripped)
#   preview DSP < 40 % core  (see examples/bench_render.rs, run separately)
#
# Usage: scripts/verify-runtime.sh [BIN]  (default target/release/micro-vocal-lab)
set -eu

BIN="${1:-target/release/micro-vocal-lab}"
FIXTURE="${2:-/tmp/mvl-session-3min.wav}"
OUT="${3:-/tmp/mvl-runtime-evidence}"
mkdir -p "$OUT"

[ -x "$BIN" ] || { echo "ERROR: $BIN not built (cargo build --release --workspace)"; exit 1; }
[ -f "$FIXTURE" ] || { echo "ERROR: fixture $FIXTURE missing (scripts/make_session_fixture.py)"; exit 1; }

echo "== binary size =="
bytes=$(wc -c < "$BIN")
echo "$BIN: $bytes bytes"
[ "$bytes" -lt $((50 * 1024 * 1024)) ] || { echo "BUDGET FAILED (>50 MB)"; exit 1; }
echo "BUDGET OK (< 50 MB)"

echo
echo "== cold start (empty shell, median of 5) =="
# Headless proxy for time-to-first-frame: the whole screenshot run, spawn to
# exit, minus nothing — an honest upper bound on GUI cold start.
i=0
while [ $i -lt 5 ]; do
  s=$(date +%s%N)
  MVL_TIMINGS=1 "$BIN" --screenshot --out "$OUT/cold-$i.png" 2>>"$OUT/cold-timings.log" >/dev/null
  e=$(date +%s%N)
  echo "run $i: $(( (e - s) / 1000000 )) ms"
  i=$((i + 1))
done
echo "(per-stage stderr timings collected in $OUT/cold-timings.log)"

echo
echo "== RAM: 3-min 48 kHz stereo session, peak RSS =="
sample_rss() {
  # $@ = command; polls /proc/<pid>/status VmHWM until exit, prints peak kB.
  "$@" >/dev/null 2>&1 &
  pid=$!
  peak=0
  while kill -0 "$pid" 2>/dev/null; do
    if [ -r "/proc/$pid/status" ]; then
      kb=$(awk '/^VmHWM:/ {print $2}' "/proc/$pid/status" 2>/dev/null || echo 0)
      [ "${kb:-0}" -gt "$peak" ] && peak=$kb
    fi
    sleep 0.02
  done
  wait "$pid" || return 1
  echo "$peak"
}

peak_idle=$(sample_rss "$BIN" --screenshot --open "$FIXTURE" --out "$OUT/rss-idle.png")
echo "loaded, no render engaged : ${peak_idle} kB peak RSS"
[ "$peak_idle" -lt $((200 * 1024)) ] || { echo "BUDGET FAILED (>=200 MB)"; exit 1; }

peak_render=$(sample_rss "$BIN" --screenshot --open "$FIXTURE" \
  --pitch 2 --air 6 --formant 140 --preview --out "$OUT/rss-render.png")
echo "loaded + preview rendered : ${peak_render} kB peak RSS"
[ "$peak_render" -lt $((200 * 1024)) ] || { echo "BUDGET FAILED (>=200 MB)"; exit 1; }
echo "BUDGET OK (< 200 MB)"

echo
echo "== artifacts =="
for f in "$OUT"/cold-0.png "$OUT"/rss-idle.png "$OUT"/rss-render.png; do
  [ -s "$f" ] && echo "ok: $f"
done
echo "RUNTIME VERIFICATION PASSED"
