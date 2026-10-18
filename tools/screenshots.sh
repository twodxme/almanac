#!/bin/sh
# Regenerate docs/screenshots/*.png from synthetic data (ALMANAC_DEMO=1).
#
# Each page runs on a Broadway display -- GTK drawing to a web page, with no
# screen -- in its own D-Bus session, so it can neither reach nor be reached
# by an Almanac you have open. A headless Chromium photographs the page, and
# ImageMagick crops it to the window. Nothing from this machine is shown:
# the inventory, health results, AI estate and history are all fixtures.
#
# Needs: gtk4 (gtk4-broadwayd), chromium, imagemagick, dbus.
set -eu
cd "$(dirname "$0")/.."
out=docs/screenshots
tmp=$(mktemp -d)
broadway=
trap 'kill "$broadway" 2>/dev/null || true; rm -rf "$tmp"' EXIT

shoot() {   # page, file, seconds to settle -- retried if the frame is blank
  for attempt in 1 2 3; do
    shoot_once "$@"
    # A blank frame (Chromium photographed before Broadway drew anything)
    # is a single colour; a real window never is.
    colours=$(magick "$out/$2" -format %k info:)
    [ "$colours" -gt 1 ] && { echo "$out/$2"; return; }
    echo "$2: blank frame, attempt $attempt" >&2
  done
  echo "$2: still blank after 3 attempts" >&2
  exit 1
}

shoot_once() {
  page=$1 file=$2 settle=$3
  # A fresh display per page: a reused one replays the previous window.
  display=:17 port=8097                       # 8080 + display number
  gtk4-broadwayd "$display" >/dev/null 2>&1 &
  broadway=$!
  sleep 1
  ALMANAC_DEMO=1 ADW_DEBUG_COLOR_SCHEME=prefer-dark \
    XDG_STATE_HOME="$tmp/state" GDK_BACKEND=broadway \
    BROADWAY_DISPLAY="$display" timeout $((settle + 20)) \
    dbus-run-session -- python3 app.py --page "$page" >/dev/null 2>&1 &
  app=$!
  sleep "$settle"
  # Broadway can hand a new viewer an empty first frame; looking again at
  # the same window a few seconds later gets the real one.
  for look in 1 2 3 4; do
    chromium --headless=new --disable-gpu --hide-scrollbars \
      --user-data-dir="$tmp/chromium" --window-size=1100,840 \
      --virtual-time-budget=3000 --screenshot="$tmp/$file" \
      "http://127.0.0.1:$port/" >/dev/null 2>&1
    [ "$(magick "$tmp/$file" -format %k info:)" -gt 1 ] && break
    sleep 3
  done
  kill "$app" "$broadway" 2>/dev/null || true
  wait "$app" "$broadway" 2>/dev/null || true
  # Broadway places the window at 25,25, at its 1024x768 screen size.
  magick "$tmp/$file" -crop 1024x768+25+25 +repage "$out/$file"
}

shoot today     today.png     35
shoot store     app-store.png 50
shoot themes    themes.png    30
shoot updates   updates.png   18
shoot ai        ai.png        18
shoot health    health.png    18
shoot history   history.png   15
shoot plugins   plugins.png   15
shoot switchboard switchboard.png 15
