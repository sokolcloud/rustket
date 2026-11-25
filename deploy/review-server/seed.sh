#!/usr/bin/env bash
# Seeds the review workspace with fictional content ("Quillmoor Labs"): rooms,
# a team with a discussion, threads, reactions, a poll, a pinned message, DMs,
# a queued omnichannel chat, and a spammer the reviewer can block and report.
#
#   ./seed.sh           idempotent: a room gets its content only while it has
#                       no user messages; memberships are healed every run
#   ./seed.sh --reset   delete the seeded rooms and DM messages, then seed again
#   ./seed.sh --clear   only delete them
#
# Run setup.sh first. Do not run it while a review is open (change freeze).
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

RESET=0 CLEAR_ONLY=0
for arg in "$@"; do
  case $arg in
    --reset) RESET=1 ;;
    --clear) RESET=1 CLEAR_ONLY=1 ;;
    -h | --help)
      sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) die "unknown argument: $arg" ;;
  esac
done

load_config
REVIEW_E2E=${REVIEW_E2E:-true}
require_credentials REVIEWER_USER REVIEW_ADMIN_USER AGENT_USER SEED_PASSWORD VISITOR_TOKEN
wait_for_server
login_operator

REVIEWER=${CRED[REVIEWER_USER]}
RADMIN=${CRED[REVIEW_ADMIN_USER]}
AGENT=${CRED[AGENT_USER]}
VISITOR_TOKEN="${CRED[VISITOR_TOKEN]}$SFX"

# --- REST rate limiter: let seeded users post at full speed, then restore ----

BYPASS_RESTORE=""
restore_bypass() {
  [[ -n "$BYPASS_RESTORE" ]] || return 0
  if ok "$(as ops POST permissions.update "$BYPASS_RESTORE")"; then
    log "restored api-bypass-rate-limit"
  else
    log "warning: could not restore api-bypass-rate-limit; remove the role user from it in Administration > Permissions"
  fi
}
finish() {
  restore_bypass
  logout ops
}
trap finish EXIT
if [[ "$(as ops GET settings/API_Enable_Rate_Limiter | jget 'd.get("value")')" == true ]]; then
  perms=$(as ops GET permissions.listAll)
  read -r BYPASS_RESTORE grant < <(python3 -I -c '
import json, sys
d = json.loads(sys.stdin.read())
for p in d.get("update", []):
    if p["_id"] == "api-bypass-rate-limit":
        roles = p.get("roles", [])
        if "user" in roles:
            break
        restore = {"permissions": [{"_id": p["_id"], "roles": roles}]}
        grant = {"permissions": [{"_id": p["_id"], "roles": roles + ["user"]}]}
        print(json.dumps(restore, separators=(",", ":")), json.dumps(grant, separators=(",", ":")))
        break
' <<<"$perms") || true
  if [[ -n "${grant:-}" ]]; then
    ok "$(as ops POST permissions.update "$grant")" && log "temporarily granted api-bypass-rate-limit to role user"
  fi
fi

# --- people ------------------------------------------------------------------

declare -A U=() UIDS=()
PEOPLE=(
  "nora|nora.quill|Nora Quill"
  "felix|felix.adeyemi|Felix Adeyemi"
  "hana|hana.sato|Hana Sato"
  "mateo|mateo.ruiz|Mateo Ruiz"
  "ingrid|ingrid.berg|Ingrid Berg"
  "rex|rex.hollis|Rex Hollis"
)

for entry in "${PEOPLE[@]}"; do
  IFS='|' read -r key user name <<<"$entry"
  user="$user$SFX"
  U[$key]=$user
  info=$(as ops GET "users.info?username=$user")
  if ! ok "$info"; then
    r=$(as ops POST users.create "$(jb username="$user" email="$user@quillmoor.example" \
      password="${CRED[SEED_PASSWORD]}" name="$name" verified:=true requirePasswordChange:=false \
      joinDefaultChannels:=true sendWelcomeEmail:=false roles:='["user"]')")
    ok "$r" || die "creating $user failed: $(jget 'd.get("error")' <<<"$r")"
    log "created $user"
  fi
  if ! login "$key" "$user" "${CRED[SEED_PASSWORD]}"; then
    uid=$(user_id "$user")
    ok "$(as ops POST users.update "$(jb userId="$uid" data:="$(jb password="${CRED[SEED_PASSWORD]}" active:=true)")")" ||
      die "resetting $user failed"
    login "$key" "$user" "${CRED[SEED_PASSWORD]}" || die "cannot log in as $user"
  fi
  UIDS[$user]=${AUTH_UID[$key]}
done
for user in "$REVIEWER" "$RADMIN" "$AGENT"; do
  UIDS[$user]=$(user_id "$user")
  [[ -n "${UIDS[$user]}" ]] || die "$user does not exist: run setup.sh"
done

# --- helpers -------------------------------------------------------------------

json_list() { python3 -I -c 'import json,sys; print(json.dumps(sys.argv[1:]))' "$@"; }

room_info() { as "$1" GET "rooms.info?roomName=$2"; }

# user_messages IDENTITY ENDPOINT RID -> number of non-system messages ("" on error)
user_messages() {
  as "$1" GET "$2.history?roomId=$3&count=100" | jget 'sum(1 for m in d["messages"] if not m.get("t"))'
}

# ensure_room c|p OWNER NAME [MEMBER...] -- sets RID and FRESH (1 = no user messages).
# EXTRA (JSON) is passed as extraData on creation.
EXTRA='{}'
ensure_room() {
  local kind=$1 owner=$2 name=$3 ep r m n
  shift 3
  [[ "$kind" == c ]] && ep=channels || ep=groups
  RID=$(room_info "$owner" "$name" | jget 'd["room"]["_id"]')
  if [[ -z "$RID" ]]; then
    r=$(as "$owner" POST "$ep.create" "$(jb name="$name" members:="$(json_list "$@")" extraData:="$EXTRA")")
    RID=$(jget 'd.get("channel") or d.get("group") or {}' <<<"$r" | jget 'd.get("_id")')
    [[ -n "$RID" ]] || die "creating $name failed: $(jget 'd.get("error")' <<<"$r")"
    log "created $name"
  fi
  EXTRA='{}'
  for m in "$@"; do
    as "$owner" POST "$ep.invite" "$(jb roomId="$RID" userId="${UIDS[$m]}")" >/dev/null
  done
  n=$(user_messages "$owner" "$ep" "$RID")
  FRESH=0
  if [[ "$n" == 0 ]]; then FRESH=1; elif [[ -z "$n" ]]; then log "warning: cannot read $name history, skipping its content"; fi
}

# say IDENTITY RID TEXT [TMID] -> message id
say() {
  local body
  body=$(python3 -I -c '
import json, sys
m = {"rid": sys.argv[1], "msg": sys.argv[2]}
if len(sys.argv) > 3 and sys.argv[3]:
    m["tmid"] = sys.argv[3]
print(json.dumps({"message": m}, ensure_ascii=False))
' "$2" "$3" "${4:-}")
  as "$1" POST chat.sendMessage "$body" | jget 'd["message"]["_id"]'
  sleep 0.3
}

react() { [[ -z "$2" ]] || as "$1" POST chat.react "$(jb messageId="$2" emoji="$3")" >/dev/null; }

# dm A B: opens the direct room between identity A and username B; sets D and
# FRESH as above.
dm() {
  local n
  D=$(as "$1" POST im.create "$(jb username="$2")" | jget 'd["room"]["_id"]')
  [[ -n "$D" ]] || die "opening a DM between ${U[$1]} and $2 failed"
  n=$(user_messages "$1" im "$D")
  FRESH=0
  if [[ "$n" == 0 ]]; then FRESH=1; fi
}

# --- reset ---------------------------------------------------------------------

delete_room() { # OWNER NAME
  local info rid t ep
  info=$(room_info "$1" "$2")
  rid=$(jget 'd["room"]["_id"]' <<<"$info")
  [[ -n "$rid" ]] || return 0
  t=$(jget 'd["room"]["t"]' <<<"$info")
  [[ "$t" == c ]] && ep=channels || ep=groups
  if ok "$(as "$1" POST "$ep.delete" "$(jb roomId="$rid")")"; then log "deleted $2"; else log "warning: could not delete $2"; fi
}

clear_dm() { # IDENTITY USERNAME: deletes the seeded people's messages in that DM
  local rid mid author key
  rid=$(as "$1" POST im.create "$(jb username="$2")" | jget 'd["room"]["_id"]')
  [[ -n "$rid" ]] || return 0
  while read -r mid author; do
    [[ -n "$mid" ]] || continue
    for key in "${!U[@]}"; do
      if [[ "${U[$key]}" == "$author" ]]; then
        as "$key" POST chat.delete "$(jb roomId="$rid" msgId="$mid")" >/dev/null
      fi
    done
  done < <(as "$1" GET "im.history?roomId=$rid&count=200" |
    jget '"\n".join(m["_id"] + " " + m["u"]["username"] for m in d["messages"])')
  return 0
}

if ((RESET)); then
  log "reset: deleting seeded rooms"
  team_rid=$(as nora GET "teams.info?teamName=$TEAM_NAME" | jget 'd["teamInfo"]["roomId"]')
  if [[ -n "$team_rid" ]]; then
    while read -r dname; do
      [[ -n "$dname" ]] && delete_room nora "$dname" # discussions have generated names
    done < <(as nora GET "rooms.getDiscussions?roomId=$team_rid&count=100" |
      jget '"\n".join(r["name"] for r in d["discussions"])')
    ok "$(as nora POST teams.delete "$(jb teamName="$TEAM_NAME")")" && log "deleted team $TEAM_NAME"
  fi
  delete_room nora "announcements$SFX"
  delete_room felix "engineering$SFX"
  delete_room hana "random$SFX"
  delete_room ingrid "leadership$SFX"
  delete_room nora "$E2E_ROOM"
  clear_dm nora "$REVIEWER"
  clear_dm rex "$REVIEWER"
  clear_dm felix "${U[hana]}"
  while read -r lrid; do
    [[ -n "$lrid" ]] && http POST livechat/room.close "$(jb rid="$lrid" token="$VISITOR_TOKEN")" >/dev/null
  done < <(http GET "livechat/visitor/$VISITOR_TOKEN/room" |
    jget '"\n".join(r["_id"] for r in d["rooms"] if r.get("open"))')
  http DELETE "livechat/visitor/$VISITOR_TOKEN" >/dev/null
  log "reset done (moderation reports stay in the console; dismiss them there if needed)"
  if ((CLEAR_ONLY)); then
    for entry in "${PEOPLE[@]}"; do logout "${entry%%|*}"; done
    exit 0
  fi
fi

# --- rooms -----------------------------------------------------------------------

STAFF=("${U[nora]}" "${U[felix]}" "${U[hana]}" "${U[mateo]}" "${U[ingrid]}")
REVIEW=("$REVIEWER" "$RADMIN" "$AGENT")

# #general: created by Rocket.Chat; everyone joins it by default.
GEN=$(room_info ops general | jget 'd["room"]["_id"]')
if [[ -n "$GEN" && "$(user_messages ops channels "$GEN")" == 0 ]]; then
  m=$(say nora "$GEN" "Hello everyone, and welcome to the Quillmoor Labs workspace :wave:")
  react felix "$m" :wave:
  react hana "$m" :heart:
  say ingrid "$GEN" "Coffee machine on the 2nd floor is fixed. Celebrate responsibly :coffee:" >/dev/null
fi

ensure_room c nora "announcements$SFX" "${STAFF[@]}" "${REVIEW[@]}"
if ((FRESH)); then
  m=$(say nora "$RID" $'**Welcome to Quillmoor Labs!**\nThis workspace is where we plan, build and ship our sensor kits.\n- Questions about tools: #engineering\n- Everything else: #random\n- Be kind, stay on topic, report anything inappropriate to the admins.')
  as nora POST chat.pinMessage "$(jb messageId="$m")" >/dev/null
  react felix "$m" :+1:
  react mateo "$m" :+1:
  say ingrid "$RID" "Office hours this week: Tuesday and Thursday, 14:00 to 15:00 UTC." >/dev/null
fi

ensure_room c felix "engineering$SFX" "${STAFF[@]}" "${REVIEW[@]}"
if ((FRESH)); then
  m=$(say felix "$RID" "Firmware 2.4 is green on all three boards :rocket:")
  react hana "$m" :tada:
  react mateo "$m" :tada:
  react nora "$m" :+1:
  t=$(say felix "$RID" "Should we move sensor polling into its own async task? Collecting opinions in this thread.")
  say hana "$RID" "Yes please, the UI stutters every time a read times out." "$t" >/dev/null
  say nora "$RID" "Fine by me if it lands before the spring launch." "$t" >/dev/null
  say felix "$RID" "Great, I'll open a draft today." "$t" >/dev/null
  react hana "$t" :eyes:
  c=$(say felix "$RID" $'This is the backoff we settled on:\n```rust\nlet delay = base * 2u32.pow(attempt.min(5));\nsleep(delay + jitter()).await;\n```')
  react mateo "$c" :white_check_mark:
  say ingrid "$RID" "Reminder: the test lab is closed on Friday afternoon for the power audit." >/dev/null
fi

ensure_room c hana "random$SFX" "${STAFF[@]}" "${U[rex]}" "${REVIEW[@]}"
if ((FRESH)); then
  p=$(say hana "$RID" $'📊 Where should the spring team day be?\n1️⃣ Lakeside cabins\n2️⃣ Science museum\n3️⃣ Cooking class')
  react nora "$p" :one:
  react felix "$p" :two:
  react mateo "$p" :one:
  react ingrid "$p" :three:
  m=$(say mateo "$RID" "Anyone up for a lunch walk at 12:30?")
  react hana "$m" :raised_hand:
  spam=$(say rex "$RID" "🔥 LIMITED OFFER 🔥 10,000 followers for \$5 at followers-deal.example.invalid !!!")
  say rex "$RID" "Seriously, click it, everything else in this channel is shit." >/dev/null
  say rex "$RID" "DM me for a \"special deal\" 😉" >/dev/null
  say nora "$RID" "Please keep ads out of this channel." >/dev/null
  if [[ -n "$spam" ]]; then
    as nora POST chat.reportMessage "$(jb messageId="$spam" description="Spam: unsolicited advertising")" >/dev/null
  fi
  as mateo POST moderation.reportUser "$(jb userId="${UIDS[${U[rex]}]}" description="Keeps posting ads in #random")" >/dev/null
fi

ensure_room p ingrid "leadership$SFX" "${U[nora]}" "${U[felix]}" "$REVIEWER" "$RADMIN"
if ((FRESH)); then
  say ingrid "$RID" "Budget review moved to Thursday 10:00." >/dev/null
  m=$(say nora "$RID" "The draft hiring plan is ready, comments welcome before Wednesday.")
  react felix "$m" :ok_hand:
fi

# Team with a discussion.
team_rid=$(as nora GET "teams.info?teamName=$TEAM_NAME" | jget 'd["teamInfo"]["roomId"]')
team_members=("${U[felix]}" "${U[hana]}" "${U[mateo]}" "${U[ingrid]}" "$REVIEWER" "$RADMIN")
if [[ -z "$team_rid" ]]; then
  ids=()
  for m in "${team_members[@]}"; do ids+=("${UIDS[$m]}"); done
  r=$(as nora POST teams.create "$(jb name="$TEAM_NAME" type:=0 members:="$(json_list "${ids[@]}")")")
  team_rid=$(jget 'd["team"]["roomId"]' <<<"$r")
  [[ -n "$team_rid" ]] || die "creating team $TEAM_NAME failed: $(jget 'd.get("error")' <<<"$r")"
  log "created team $TEAM_NAME"
fi
members_json=$(python3 -I -c '
import json, sys
print(json.dumps([{"userId": u, "roles": ["member"]} for u in sys.argv[1:]]))
' "${UIDS[${U[felix]}]}" "${UIDS[${U[hana]}]}" "${UIDS[${U[mateo]}]}" "${UIDS[${U[ingrid]}]}" \
  "${UIDS[$REVIEWER]}" "${UIDS[$RADMIN]}")
as nora POST teams.addMembers "$(jb teamName="$TEAM_NAME" members:="$members_json")" >/dev/null
if [[ "$(user_messages nora channels "$team_rid")" == 0 ]]; then
  m=$(say nora "$team_rid" "Kickoff notes for the spring launch are in the discussion below :point_down:")
  react hana "$m" :+1:
fi
DISCUSSION="Spring launch plan"
has_discussion=$(as nora GET "rooms.getDiscussions?roomId=$team_rid&count=100" |
  python3 -I -c '
import json, sys
d = json.load(sys.stdin)
print("yes" if any(r.get("fname") == sys.argv[1] for r in d.get("discussions", [])) else "")
' "$DISCUSSION" 2>/dev/null || true)
if [[ -z "$has_discussion" ]]; then
  r=$(as nora POST rooms.createDiscussion "$(jb prid="$team_rid" t_name="$DISCUSSION" \
    users:="$(json_list "${team_members[@]}")" reply="Let's collect the launch tasks here.")")
  drid=$(jget 'd["discussion"]["_id"]' <<<"$r")
  if [[ -n "$drid" ]]; then
    log "created discussion \"$DISCUSSION\""
    say felix "$drid" "Firmware: release candidate by the 10th." >/dev/null
    say hana "$drid" "Packaging artwork goes to print on the 12th." >/dev/null
    m=$(say mateo "$drid" "Support docs and the FAQ will be ready the same week.")
    react nora "$m" :tada:
  else
    log "warning: creating the discussion failed: $(jget 'd.get("error")' <<<"$r")"
  fi
fi

# End-to-end encrypted room. Its key is created by the first member whose
# client has E2E keys (the reviewer's Rustket), so it starts empty.
if [[ "$REVIEW_E2E" == true ]]; then
  EXTRA='{"encrypted":true}'
  ensure_room p nora "$E2E_ROOM" "$REVIEWER" "$RADMIN"
fi

# --- direct messages ---------------------------------------------------------------

dm nora "$REVIEWER"
if ((FRESH)); then
  say nora "$D" "Hi! Welcome aboard. Ping me here if you need access to anything." >/dev/null
  say nora "$D" "The team handbook is pinned in #announcements$SFX." >/dev/null
fi
dm rex "$REVIEWER"
if ((FRESH)); then
  say rex "$D" "Hey!! I have an amazing opportunity for you 💰💰💰" >/dev/null
  say rex "$D" "Reply fast, the offer ends tonight: followers-deal.example.invalid" >/dev/null
fi
dm felix "${U[hana]}"
if ((FRESH)); then
  say felix "$D" "Could you send me the latest icon exports?" >/dev/null
  say hana "$D" "Sure, after lunch." >/dev/null
fi

# --- omnichannel: one visitor chat waiting in the queue -------------------------

http POST livechat/visitor "$(jb visitor:="$(jb token="$VISITOR_TOKEN" name="Jordan Avery" \
  email="jordan.avery@visitor.example")")" >/dev/null
r=$(http GET "livechat/room?token=$VISITOR_TOKEN")
lrid=$(jget '(d.get("room") or {}).get("_id")' <<<"$r")
LIVECHAT="Jordan Avery waiting in the queue for $AGENT"
if [[ -z "$lrid" ]]; then
  LIVECHAT="none ($(jget 'd.get("error")' <<<"$r"))"
  log "warning: no omnichannel chat: $(jget 'd.get("error")' <<<"$r")"
elif [[ "$(jget 'd.get("newRoom")' <<<"$r")" == true ]]; then
  for text in "Hi, my sensor kit stopped syncing after the last update." "It's the KT-200 model, if that helps."; do
    http POST livechat/message "$(jb token="$VISITOR_TOKEN" rid="$lrid" msg="$text")" >/dev/null
    sleep 0.3
  done
  log "queued an omnichannel chat from Jordan Avery"
fi

for entry in "${PEOPLE[@]}"; do logout "${entry%%|*}"; done

cat <<EOF

Seeded $SERVER:
  channels   #announcements$SFX (pinned welcome)  #engineering$SFX (thread, code)  #random$SFX (poll, spam)
  private    leadership$SFX
  team       $TEAM_NAME with the discussion "$DISCUSSION"
  E2E room   $([[ "$REVIEW_E2E" == true ]] && echo "$E2E_ROOM (empty until the reviewer opens it)" || echo "off")
  DMs        ${U[nora]} and ${U[rex]} -> $REVIEWER, ${U[felix]} <-> ${U[hana]}
  spammer    ${U[rex]} (one message and the user already reported, for the moderation console)
  omnichannel  $LIVECHAT
EOF
