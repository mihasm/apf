#!/bin/sh
# Loads the installed plugin in REAPER as CLAP, VST3 and AU (scripts/reaper-render-test.lua),
# renders a chord through each format and compares the three results.
#
#   scripts/reaper-render-test.sh "APF Demo Synth" "Miha Smrekar" [out-dir]
#
# REAPER must not be running already (the test starts its own instance and quits it).
set -eu
name=$1
vendor=$2
out=${3:-${TMPDIR:-/tmp}/apf-reaper-test}
here=$(cd "$(dirname "$0")" && pwd)
reaper=/Applications/REAPER.app/Contents/MacOS/REAPER

[ -x "$reaper" ] || { echo "REAPER is not installed"; exit 2; }
if pgrep -x REAPER >/dev/null; then echo "REAPER is running; close it first"; exit 2; fi
rm -rf "$out" && mkdir -p "$out"

APF_REAPER_PLUGIN="$name ($vendor)" APF_REAPER_AU_NAME="$vendor: $name" APF_REAPER_OUT="$out" \
    "$reaper" -new -nosplash "$here/reaper-render-test.lua" >"$out/reaper.log" 2>&1 &
pid=$!
i=0
until grep -q '^rendered' "$out/report.txt" 2>/dev/null; do
    i=$((i + 1))
    if [ $i -gt 120 ]; then echo "REAPER did not finish in 120 s"; kill "$pid"; exit 1; fi
    sleep 1
done
# The test project is unsaved; end REAPER without its save prompt.
kill "$pid"
wait "$pid" 2>/dev/null || true
cat "$out/report.txt"

python3 - "$out" <<'PY'
import math, sys, wave
out = sys.argv[1]
levels = {}
for f in ("CLAP", "VST3", "AU"):
    try:
        w = wave.open(f"{out}/{f}.wav")
    except FileNotFoundError:
        print(f"{f}: no render"); continue
    n, ch, sw, sr = w.getnframes(), w.getnchannels(), w.getsampwidth(), w.getframerate()
    raw = w.readframes(n)
    scale = float(1 << (8 * sw - 1))
    vals = [int.from_bytes(raw[i:i + sw], "little", signed=True) / scale for i in range(0, len(raw), sw)]
    held = vals[: int(1.4 * sr) * ch]
    rms = 20 * math.log10(math.sqrt(sum(x * x for x in held) / len(held)) + 1e-12)
    peak = 20 * math.log10(max(abs(x) for x in vals) + 1e-12)
    levels[f] = rms
    print(f"{f}: {n / sr:.2f} s, RMS while held {rms:.1f} dBFS, peak {peak:.1f} dBFS")
ok = len(levels) == 3 and all(v > -40 for v in levels.values()) and max(levels.values()) - min(levels.values()) < 1.0
print("REAPER: OK, all three formats sound the same" if ok else "REAPER: FAILED")
sys.exit(0 if ok else 1)
PY
