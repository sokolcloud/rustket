#!/usr/bin/env bash
# Seeds the local dev Rocket.Chat with one room per unread state for the
# viewer `schemeview` / `schemeviewpass`, so the RC web client and Rustket can
# be compared side by side. Every run creates fresh rooms (prefix ind<N>) and
# the viewer leaves the rooms of earlier runs.
set -euo pipefail

SERVER="${RUSTKET_TEST_SERVER:-http://localhost:3000}"
API="$SERVER/api/v1"
N=$(date +%s | tail -c 6)
P="ind$N"

json() { python3 -c "import sys,json; d=json.load(sys.stdin); print($1)"; }
login() {
  curl -sf -X POST "$API/login" -H 'content-type: application/json' \
    -d "{\"user\":\"$1\",\"password\":\"$2\"}"
}
declare -A UID_ TOK
auth() {
  local r
  r=$(login "$1" "$2")
  UID_[$1]=$(printf '%s' "$r" | json 'd["data"]["userId"]')
  TOK[$1]=$(printf '%s' "$r" | json 'd["data"]["authToken"]')
}
as() {
  local u=$1 method=$2 path=$3 body=${4:-}
  local args=(-s -X "$method" "$API/$path" -H "X-User-Id: ${UID_[$u]}" -H "X-Auth-Token: ${TOK[$u]}")
  [[ -n "$body" ]] && args+=(-H 'content-type: application/json' -d "$body")
  curl "${args[@]}"
}
say() {
  local body
  body=$(python3 -c 'import json,sys
m={"rid":sys.argv[1],"msg":sys.argv[2]}
if len(sys.argv)>3 and sys.argv[3]: m["tmid"]=sys.argv[3]
print(json.dumps({"message":m}))' "$2" "$3" "${4:-}")
  as "$1" POST chat.sendMessage "$body" | json 'd["message"]["_id"]'
  sleep 0.4
}
rid_of() { json 'd.get("channel", d.get("group", {}))["_id"]'; }

auth admin adminpass
auth bob bobpass
as admin POST users.create '{"username":"schemeview","email":"schemeview@demo.rustket.local","password":"schemeviewpass","name":"Scheme Viewer","verified":true,"requirePasswordChange":false,"joinDefaultChannels":false}' >/dev/null || true
auth schemeview schemeviewpass
V=schemeview

# Leave rooms of earlier runs (DMs stay).
for rid in $(as $V GET "subscriptions.get" | python3 -c 'import sys,json
for s in json.load(sys.stdin).get("update",[]):
    if s.get("t") in ("c","p") and s.get("name","").startswith("ind"): print(s["rid"]+":"+s["t"])'); do
  r=${rid%%:*} t=${rid##*:}
  if [[ $t == c ]]; then as $V POST channels.leave "{\"roomId\":\"$r\"}" >/dev/null || true
  else as $V POST groups.leave "{\"roomId\":\"$r\"}" >/dev/null || true; fi
done

mk() { as bob POST channels.create "{\"name\":\"$P-$1\",\"members\":[\"$V\"]}" | rid_of; }
read_all() { as $V POST subscriptions.read "{\"rid\":\"$1\"}" >/dev/null; }

# 1. read room (no indicator)
R=$(mk read); say bob "$R" "already read" >/dev/null; read_all "$R"
# 2. plain channel message (Unread_Count default: mentions only -> alert only)
R=$(mk plain); say bob "$R" "plain message" >/dev/null; say bob "$R" "another one" >/dev/null
# 3. direct mention
R=$(mk mention); say bob "$R" "hey @$V please look" >/dev/null
# 4. @all and @here
R=$(mk all); say bob "$R" "@all release at 5" >/dev/null
R=$(mk here); say bob "$R" "@here anyone?" >/dev/null
# 5. plain thread reply in a thread the viewer follows
R=$(mk thread); root=$(say $V "$R" "my question"); read_all "$R"; say bob "$R" "answer in thread" "$root" >/dev/null
# 6. thread reply mentioning the viewer
R=$(mk thread-mention); root=$(say $V "$R" "another question"); read_all "$R"; say bob "$R" "@$V see this" "$root" >/dev/null
# 7. thread @all
R=$(mk thread-all); root=$(say $V "$R" "third question"); read_all "$R"; say bob "$R" "@all in a thread" "$root" >/dev/null
# 8. thread reply + @all in the main timeline (RC: threads rank above group mentions)
R=$(mk thread-and-all); root=$(say $V "$R" "fourth question"); read_all "$R"; say bob "$R" "thread reply" "$root" >/dev/null; say bob "$R" "@all main" >/dev/null
# 9. mention + thread + plain
R=$(mk mixed); root=$(say $V "$R" "fifth question"); read_all "$R"; say bob "$R" "reply" "$root" >/dev/null; say bob "$R" "@$V ping" >/dev/null
# 10. muted room (notifications off) with a mention and @all
R=$(mk muted); as $V POST rooms.saveNotification "{\"roomId\":\"$R\",\"notifications\":{\"disableNotifications\":\"1\"}}" >/dev/null
say bob "$R" "@$V muted mention" >/dev/null
R=$(mk muted-all); as $V POST rooms.saveNotification "{\"roomId\":\"$R\",\"notifications\":{\"disableNotifications\":\"1\"}}" >/dev/null
say bob "$R" "@all muted all" >/dev/null
R=$(mk muted-plain); as $V POST rooms.saveNotification "{\"roomId\":\"$R\",\"notifications\":{\"disableNotifications\":\"1\"}}" >/dev/null
say bob "$R" "muted plain" >/dev/null
# 11. hidden unread status
R=$(mk hidden); as $V POST rooms.saveNotification "{\"roomId\":\"$R\",\"notifications\":{\"hideUnreadStatus\":\"1\"}}" >/dev/null
say bob "$R" "hidden plain" >/dev/null; say bob "$R" "@$V hidden mention" >/dev/null
# 12. muted group mentions
R=$(mk mute-group); as $V POST rooms.saveNotification "{\"roomId\":\"$R\",\"notifications\":{\"muteGroupMentions\":\"1\"}}" >/dev/null
say bob "$R" "@all muted group mention" >/dev/null
# 13. private group with a mention
G=$(as bob POST groups.create "{\"name\":\"$P-private\",\"members\":[\"$V\"]}" | rid_of); say bob "$G" "@$V private ping" >/dev/null
# 14. DM: plain messages (Unread_Count_DM default: all messages)
D=$(as bob POST im.create "{\"username\":\"$V\"}" | json 'd["room"]["_id"]')
read_all "$D"; say bob "$D" "dm one" >/dev/null; say bob "$D" "dm two" >/dev/null

echo "seeded prefix $P for $V"
