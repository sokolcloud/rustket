#!/usr/bin/env bash
# Captures the README screenshots from the demo workspace (dev/seed-demo.sh) in
# the light and dark themes. Needs an X11 session, wmctrl, xwd and Python PIL.
# The app runs from a throwaway profile (XDG_* dirs, including the runtime dir
# of the single-instance socket) with the CPU renderer, so a running Rustket and
# its settings are not touched.
#
#   dev/seed-demo.sh && dev/screenshots.sh [binary] [out dir]
set -euo pipefail

BIN="${1:-target/release/rustket}"
OUT="${2:-docs/screenshots}"
SERVER="${RUSTKET_TEST_SERVER:-http://localhost:3000}"
WIDTH="${SHOT_WIDTH:-1360}"
HEIGHT="${SHOT_HEIGHT:-860}"
HERE="$(cd "$(dirname "$0")" && pwd)"

json() { python3 -c "import sys,json; d=json.load(sys.stdin); print($1)"; }

login=$(curl -sf -X POST "$SERVER/api/v1/login" -H 'content-type: application/json' \
  -d '{"user":"maya","password":"mayapass"}')
USER_ID=$(printf '%s' "$login" | json 'd["data"]["userId"]')
TOKEN=$(printf '%s' "$login" | json 'd["data"]["authToken"]')
room_id() {
  curl -sf "$SERVER/api/v1/rooms.info?roomName=$1" -H "X-User-Id: $USER_ID" -H "X-Auth-Token: $TOKEN" |
    json 'd["room"]["_id"]'
}

# The first thread of a room (its parent message id).
thread_id() {
  curl -sf "$SERVER/api/v1/chat.getThreadsList?rid=$1&count=1" -H "X-User-Id: $USER_ID" -H "X-Auth-Token: $TOKEN" |
    json 'd["threads"][0]["_id"]'
}

# Colored flags, as pending edits of this device (the app shows them at once
# and stores them in the user's sync room on the server).
flags_blob() {
  local now rel eng
  now=$(date +%s%3N)
  rel=$(room_id releases)
  eng=$(room_id engineering)
  python3 - "$now" "$rel" "$(room_id design)" "$(room_id platform-team)" \
    "$(thread_id "$rel")" "$eng" "$(thread_id "$eng")" <<'PY'
import json, sys
now, releases, design, team, rel_thread, eng, eng_thread = sys.argv[1:]
st = {"t": int(now), "d": "screenshots"}
print(json.dumps({
    "v": 1,
    "rooms": {r: {"c": c, **st} for r, c in
              [(releases, "green"), (design, "green"), (team, "orange")]},
    "threads": {rel_thread: {"c": "green", "rid": releases, **st},
                eng_thread: {"c": "purple", "rid": eng, **st}},
    "names": {c: {"n": n, **st} for c, n in
              [("green", "This sprint"), ("orange", "On call"), ("purple", "Follow up")]},
}, separators=(",", ":")))
PY
}

mkdir -p "$OUT"
FLAGS=$(flags_blob)
PROFILE=$(mktemp -d)
trap 'rm -rf "$PROFILE"' EXIT

dm_id() {
  curl -sf -X POST "$SERVER/api/v1/im.create" -H "X-User-Id: $USER_ID" -H "X-Auth-Token: $TOKEN" \
    -H 'content-type: application/json' -d "{\"username\":\"$1\"}" | json 'd["room"]["_id"]'
}

# shot <name> <theme> <room id> <density> [collapsed section keys...]
shot() {
  local name=$1 theme=$2 rid=$3 density=$4
  shift 4
  local collapsed=""
  for k in "$@"; do collapsed+="\"$k\", "; done
  rm -rf "$PROFILE"/*
  mkdir -p "$PROFILE/config/rustket" "$PROFILE/data/rustket" "$PROFILE/cache" "$PROFILE/run"
  chmod 700 "$PROFILE/run"
  cat >"$PROFILE/config/rustket/config.toml" <<EOF
version = 1
active_server = "demo"

[[servers]]
id = "demo"
url = "$SERVER"
display_name = "Northwind"
user_id = "$USER_ID"
username = "maya"
token_kind = "login"
last_room = "$rid"
added_at = 1700000000
sort_order = 0

[[servers.pending_flags]]
user_id = "$USER_ID"
blob = '$FLAGS'

[ui]
theme = "$theme"
language = "en"
density = "$density"
use_real_names = true
sidebar_threads = "following"
desktop_notifications = false
sound = false
check_updates = false
minimize_to_tray = false
close_to_tray = false

[window]
x = 120
y = 120
width = $WIDTH
height = $HEIGHT
maximized = false
EOF
  printf 'collapsed_sections = [%s]\n' "${collapsed%, }" >"$PROFILE/config/rustket/ui-state.toml"
  printf '{"rustket:demo:%s":"%s"}' "$USER_ID" "$TOKEN" >"$PROFILE/data/rustket/secrets.json"
  chmod 600 "$PROFILE/data/rustket/secrets.json"

  env -u XAUTHORITY ICED_BACKEND=tiny-skia RUST_LOG=warn \
    XDG_CONFIG_HOME="$PROFILE/config" XDG_DATA_HOME="$PROFILE/data" XDG_CACHE_HOME="$PROFILE/cache" \
    XDG_RUNTIME_DIR="$PROFILE/run" "$BIN" >"$PROFILE/app.log" 2>&1 &
  local pid=$! wid=""
  for _ in $(seq 1 40); do
    sleep 0.5
    local pids
    pids=" $pid $( (pgrep -P "$pid" || true) | tr "\n" " ")"
    wid=$(wmctrl -lp | awk -v p="$pids" 'index(p, " " $3 " ") { print $1; exit }')
    [[ -n "$wid" ]] && break
  done
  if [[ -z "$wid" ]]; then
    kill "$pid" 2>/dev/null || true
    echo "no window for $name" >&2
    cat "$PROFILE/app.log" >&2
    return 1
  fi
  sleep "${SHOT_DELAY:-40}"
  # Keep it above other windows while it is captured (without taking focus).
  wmctrl -i -r "$wid" -b add,above || true
  sleep 1
  xwd -silent -id "$wid" -out "$PROFILE/shot.xwd"
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  python3 "$HERE/xwd2png.py" "$PROFILE/shot.xwd" "$OUT/$name.png" "${SHOT_MAX_WIDTH:-1400}"
  echo "$OUT/$name.png"
}

ENGINEERING=$(room_id engineering)
PRIYA=$(dm_id priya)
for theme in light dark; do
  shot "main-$theme" "$theme" "$ENGINEERING" extended
  shot "compact-$theme" "$theme" "$PRIYA" condensed \
    sidebar.channels sidebar.private_groups sidebar.direct_messages
done
