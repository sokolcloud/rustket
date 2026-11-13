#!/usr/bin/env bash
# Seeds the local dev Rocket.Chat with a small demo workspace for the README
# screenshots (see dev/screenshots.sh). The viewer is `maya` / `mayapass`.
# Rerunning deletes and recreates the demo rooms, so the result is always the same.
set -euo pipefail

SERVER="${RUSTKET_TEST_SERVER:-http://localhost:3000}"
API="$SERVER/api/v1"

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

# as <user> <METHOD> <path> [body]
as() {
  local u=$1 method=$2 path=$3 body=${4:-}
  local args=(-s -X "$method" "$API/$path" -H "X-User-Id: ${UID_[$u]}" -H "X-Auth-Token: ${TOK[$u]}")
  [[ -n "$body" ]] && args+=(-H 'content-type: application/json' -d "$body")
  curl "${args[@]}"
}

# say <user> <rid> <text> [tmid] -> message id
say() {
  local body
  body=$(python3 -c 'import json,sys
m={"rid":sys.argv[1],"msg":sys.argv[2]}
if len(sys.argv)>3 and sys.argv[3]: m["tmid"]=sys.argv[3]
print(json.dumps({"message":m}))' "$2" "$3" "${4:-}")
  as "$1" POST chat.sendMessage "$body" | json 'd["message"]["_id"]'
  sleep 0.3
}

react() { as "$1" POST chat.react "{\"messageId\":\"$2\",\"emoji\":\"$3\"}" >/dev/null; }

for _ in $(seq 1 60); do
  curl -sf "$SERVER/api/info" >/dev/null && break
  sleep 2
done

auth admin adminpass

USERS=(maya:Maya\ Chen leo:Leo\ Fischer priya:Priya\ Raman tomas:Tomás\ Novak ines:Inês\ Duarte sam:Sam\ Okafor)
for entry in "${USERS[@]}"; do
  u=${entry%%:*} name=${entry#*:}
  as admin POST users.create "{\"username\":\"$u\",\"email\":\"$u@demo.rustket.local\",\"password\":\"${u}pass\",\"name\":\"$name\",\"verified\":true,\"requirePasswordChange\":false,\"joinDefaultChannels\":false}" >/dev/null || true
  auth "$u" "${u}pass"
done

for r in engineering design releases random; do
  as admin POST channels.delete "{\"roomName\":\"$r\"}" >/dev/null || true
done
as tomas POST groups.delete '{"roomName":"platform-team"}' >/dev/null || true

members='["maya","leo","priya","tomas","ines","sam"]'
rid_of() { json 'd.get("channel", d.get("group", {}))["_id"]'; }
ENG=$(as leo POST channels.create "{\"name\":\"engineering\",\"members\":$members,\"extraData\":{\"topic\":\"Client and protocol work\"}}" | rid_of)
DESIGN=$(as priya POST channels.create "{\"name\":\"design\",\"members\":$members}" | rid_of)
REL=$(as maya POST channels.create "{\"name\":\"releases\",\"members\":$members}" | rid_of)
RANDOM_=$(as sam POST channels.create "{\"name\":\"random\",\"members\":$members}" | rid_of)
TEAM=$(as tomas POST groups.create "{\"name\":\"platform-team\",\"members\":[\"maya\",\"leo\",\"tomas\",\"sam\"]}" | rid_of)

# #engineering: read by maya; a thread, code, reactions, formatting.
m=$(say leo "$ENG" "Morning! The DDP reconnect fix is on staging :rocket:")
react priya "$m" :tada:
react tomas "$m" :tada:
say priya "$ENG" "Nice. Did the backoff survive the laptop-sleep test?" >/dev/null
say leo "$ENG" "Yes, 40 suspend/resume cycles and no duplicate subscriptions." >/dev/null
code=$(say tomas "$ENG" $'This is the retry policy we ended up with:\n```rust\nlet delay = base * 2u32.pow(attempt.min(6));\nsleep(delay + jitter()).await;\n```')
say priya "$ENG" "Should the jitter be capped?" "$code" >/dev/null
say leo "$ENG" "It is, 250 ms at most." "$code" >/dev/null
say maya "$ENG" "Makes sense :+1:" "$code" >/dev/null
react maya "$code" :eyes:
say ines "$ENG" "@maya could you review the sidebar PR when you have a minute?" >/dev/null
m=$(say maya "$ENG" "Sure, right after lunch.")
react ines "$m" :pray:
say sam "$ENG" "**Heads-up:** the staging database is in maintenance tonight at 22:00 UTC. _Expect about 5 minutes of read-only mode._" >/dev/null
as maya POST rooms.favorite "{\"roomId\":\"$ENG\",\"favorite\":true}" >/dev/null

# #design: plain unread messages.
say priya "$DESIGN" "New icon set is in Figma, feedback welcome" >/dev/null
say ines "$DESIGN" "Love the outline weight. Can we get a 16 px variant?" >/dev/null
say priya "$DESIGN" "On it" >/dev/null

# #releases: only an unread thread reply.
p=$(say maya "$REL" "Release 0.9 checklist: changelog, packages, store listing")
say leo "$REL" "Changelog is ready for review" "$p" >/dev/null
as maya POST subscriptions.read "{\"rid\":\"$REL\"}" >/dev/null
say leo "$REL" "Packages are built and signed" "$p" >/dev/null

# #random: muted, with unread messages.
say sam "$RANDOM_" "Who's up for lunch at the new ramen place?" >/dev/null
say tomas "$RANDOM_" "Count me in" >/dev/null
as maya POST rooms.saveNotification "{\"roomId\":\"$RANDOM_\",\"notifications\":{\"disableNotifications\":\"1\"}}" >/dev/null

# platform-team: an @all mention.
say tomas "$TEAM" "@all retro moved to Thursday, same time" >/dev/null

# Direct messages.
# A direct message room, emptied by the authors (direct rooms are not recreated).
dm() {
  local rid
  rid=$(as "$1" POST im.create "{\"username\":\"$2\"}" | json 'd["room"]["_id"]')
  local mid author
  while read -r mid author; do
    [[ -n "${TOK[$author]:-}" ]] &&
      as "$author" POST chat.delete "{\"roomId\":\"$rid\",\"msgId\":\"$mid\"}" >/dev/null
  done < <(as "$1" GET "im.history?roomId=$rid&count=200" |
    json '"\n".join(m["_id"] + " " + m["u"]["username"] for m in d["messages"])')
  as maya POST subscriptions.read "{\"rid\":\"$rid\"}" >/dev/null
  echo "$rid"
}
D=$(dm maya priya)
say priya "$D" "Thanks for the quick review yesterday!" >/dev/null
say maya "$D" "Anytime. The new layout reads much better." >/dev/null
as maya POST rooms.favorite "{\"roomId\":\"$D\",\"favorite\":true}" >/dev/null
D=$(dm maya sam)
say sam "$D" "Can you join the incident review at 3?" >/dev/null
say maya "$D" "Yes, I'll be there" >/dev/null
D=$(dm leo maya)
say leo "$D" "Got a minute to pair on the thread panel?" >/dev/null

as maya POST subscriptions.read "{\"rid\":\"$ENG\"}" >/dev/null
GEN=$(as maya GET "rooms.info?roomName=general" | json 'd.get("room",{}).get("_id","")' || true)
[[ -n "$GEN" ]] && as maya POST rooms.hide "{\"roomId\":\"$GEN\"}" >/dev/null || true

echo "MAYA_ID=${UID_[maya]}"
echo "ENGINEERING=$ENG"
echo "DESIGN=$DESIGN"
