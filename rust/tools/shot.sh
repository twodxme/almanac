#!/bin/sh
# Photograph one page of either implementation, in demo mode, for comparison.
#   rust/tools/shot.sh py today /path/out.png          Python app
#   rust/tools/shot.sh rs today /path/out.png          Rust app (built binary)
#   SCHEME=prefer-light rust/tools/shot.sh rs today …   light mode
#   ALMANAC_RS=/path/to/almanac-rs                      which Rust binary
# Pages: today store themes updates apps all ai health history plugins
#        switchboard orphans
# Each call takes its own Broadway display (picked from the page and side), so
# Python and Rust shots can run at the same time. The window is 1024x768.
set -eu
side=$1 page=$2 out=$3
here=$(cd "$(dirname "$0")/../.." && pwd)
n=$(printf '%s%s' "$side" "$page" | cksum | cut -c1-2)
display=$((30 + n % 60)); port=$((8080 + display))
tmp=$(mktemp -d)
trap 'kill $app $bw 2>/dev/null || true; rm -rf "$tmp"' EXIT
gtk4-broadwayd ":$display" >/dev/null 2>&1 & bw=$!
sleep 1
if [ "$side" = py ]; then
  cmd="python3 $here/app.py"
else
  cmd="${ALMANAC_RS:-${CARGO_TARGET_DIR:-$here/rust/target}/debug/almanac-rs}"
fi
ALMANAC_DEMO=1 ADW_DEBUG_COLOR_SCHEME="${SCHEME:-prefer-dark}" \
  XDG_STATE_HOME="$tmp/state" GDK_BACKEND=broadway BROADWAY_DISPLAY=":$display" \
  timeout 90 dbus-run-session -- $cmd --page "$page" >"$tmp/log" 2>&1 & app=$!
sleep "${SETTLE:-25}"
for look in 1 2 3 4 5; do
  chromium --headless=new --disable-gpu --hide-scrollbars \
    --user-data-dir="$tmp/chromium" --window-size=1100,840 \
    --virtual-time-budget=3000 --screenshot="$tmp/raw.png" \
    "http://127.0.0.1:$port/" >/dev/null 2>&1 || true
  [ -f "$tmp/raw.png" ] && [ "$(magick "$tmp/raw.png" -format %k info:)" -gt 1 ] && break
  sleep 3
done
magick "$tmp/raw.png" -crop 1024x768+25+25 +repage "$out"
if ! kill -0 $app 2>/dev/null; then
  echo "app exited early; last log lines:" >&2; tail -20 "$tmp/log" >&2
fi
echo "$out"
